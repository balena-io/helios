use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bollard::Docker;
use bollard::query_parameters::{
    BuildImageOptions, EventsOptions, ListContainersOptions, PushImageOptions, RemoveImageOptions,
};
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::{Value, json};

use super::common::{HELIOS_URL, prune_images, wait_for_target_apply};

const APP_UUID: &str = "strategy-app";
const APP_ID: u32 = 4321;
const REPO: &str = "registry:5000/strategy-test";
const BASE_IMAGE: &str = "alpine:3.23";

// Services trap TERM so a stop is quick and the kill time is a clean signal
const SERVE: &str = "trap 'exit 0' TERM; sleep 300 & wait";

fn docker() -> Docker {
    Docker::connect_with_defaults().unwrap()
}

// --- Images ---

/// Build a distinct image, push it to the test registry and drop the local
/// copy, so helios has to pull it. Returns its tag and digest references.
async fn registry_image(docker: &Docker, tag: &str) -> (String, String) {
    let dockerfile = format!("FROM {BASE_IMAGE}\nLABEL io.balena.test.tag={tag}\n");
    let mut tar_buf = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(dockerfile.len() as u64);
    header.set_mode(0o644);
    tar_buf
        .append_data(&mut header, "Dockerfile", dockerfile.as_bytes())
        .unwrap();
    let context = tar_buf.into_inner().unwrap();

    let reference = format!("{REPO}:{tag}");
    let mut stream = docker.build_image(
        BuildImageOptions {
            t: Some(reference.clone()),
            ..Default::default()
        },
        None,
        Some(bollard::body_full(context.into())),
    );
    while let Some(res) = stream.next().await {
        res.expect("image build failed");
    }

    let mut stream = docker.push_image(
        REPO,
        Some(PushImageOptions {
            tag: Some(tag.to_string()),
            ..Default::default()
        }),
        None,
    );
    while let Some(res) = stream.next().await {
        res.expect("image push failed");
    }

    let digest = docker
        .inspect_image(&reference)
        .await
        .unwrap()
        .repo_digests
        .unwrap_or_default()
        .into_iter()
        .find(|d| d.starts_with(REPO))
        .expect("pushed image has no digest");

    remove_image(docker, &reference).await;
    (reference, digest)
}

async fn remove_image(docker: &Docker, reference: &str) {
    let _ = docker
        .remove_image(
            reference,
            Some(RemoveImageOptions {
                force: true,
                ..Default::default()
            }),
            None,
        )
        .await;
}

async fn image_id(docker: &Docker, reference: &str) -> String {
    docker.inspect_image(reference).await.unwrap().id.unwrap()
}

// --- Targets ---

fn strategy(name: &str) -> Value {
    json!({ "io.balena.update.strategy": name })
}

fn service(id: u32, image: &str, command: &str, labels: Value) -> Value {
    json!({
        "id": id,
        "image": image,
        "composition": {
            "command": ["sh", "-c", command],
            "labels": labels,
        }
    })
}

/// POST a single-release target for the app, without waiting for it.
async fn post_release(release_uuid: &str, services: &[(&str, Value)]) {
    let services: serde_json::Map<String, Value> = services
        .iter()
        .map(|(name, svc)| (name.to_string(), svc.clone()))
        .collect();
    let res = reqwest::Client::new()
        .post(format!("{HELIOS_URL}/v3/device/apps/{APP_UUID}"))
        .json(&json!({
            "id": APP_ID,
            "name": APP_UUID,
            "releases": {
                release_uuid: { "services": services, "networks": {}, "volumes": {} }
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
}

async fn apply_release(release_uuid: &str, services: &[(&str, Value)]) {
    post_release(release_uuid, services).await;
    assert_eq!(wait_for_target_apply().await, json!({"status": "done"}));
}

/// Remove the app, whether or not an earlier test left it behind.
async fn remove_app() {
    let res = reqwest::Client::new()
        .delete(format!("{HELIOS_URL}/v3/device/apps/{APP_UUID}"))
        .send()
        .await
        .unwrap();
    if res.status() == StatusCode::ACCEPTED {
        assert_eq!(wait_for_target_apply().await, json!({"status": "done"}));
    }
}

/// Remove the app and its images, so later tests start from an empty engine.
async fn cleanup() {
    remove_app().await;
    prune_images().await;
}

// --- Engine state ---

fn container_name(service: &str, release_uuid: &str) -> String {
    format!("{service}_{release_uuid}")
}

/// Every container of the app, by name, with whether it is running.
async fn app_containers(docker: &Docker) -> HashMap<String, bool> {
    let filters = HashMap::from([(
        "label".to_string(),
        vec![format!("io.balena.app-uuid={APP_UUID}")],
    )]);
    docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(filters),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_iter()
        .filter_map(|c| {
            let name = c.names?.first()?.trim_start_matches('/').to_string();
            Some((name, c.state.is_some_and(|s| s.to_string() == "running")))
        })
        .collect()
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

struct Event {
    time: f64,
    /// The container name, or the image reference or id.
    subject: String,
    action: String,
}

/// Engine events of `kind` (`container` or `image`) since `since`.
///
/// A container event names the container. An image pull names the reference
/// pulled, and a delete names the image id.
async fn events(docker: &Docker, since: f64, kind: &str) -> Vec<Event> {
    let mut stream = docker.events(Some(EventsOptions {
        since: Some(format!("{since:.3}")),
        until: Some(format!("{:.3}", now())),
        filters: Some(HashMap::from([(
            "type".to_string(),
            vec![kind.to_string()],
        )])),
    }));
    let mut out = Vec::new();
    while let Some(msg) = stream.next().await {
        let msg = msg.unwrap();
        let actor = msg.actor.unwrap_or_default();
        let subject = match kind {
            "container" => actor.attributes.and_then(|mut a| a.remove("name")),
            _ => actor.id,
        };
        out.push(Event {
            time: msg.time_nano.unwrap_or_default() as f64 / 1e9,
            subject: subject.unwrap_or_default(),
            action: msg.action.unwrap_or_default(),
        });
    }
    out
}

fn first(events: &[Event], subject: &str, action: &str) -> Option<f64> {
    events
        .iter()
        .find(|e| e.subject == subject && e.action == action)
        .map(|e| e.time)
}

// --- Tests ---

/// Replace a service under `strategy` and check the old one is removed, and
/// with `deletes_image` its image deleted, before the new image is pulled.
async fn assert_removed_before_pull(strategy_name: &str, prefix: &str, deletes_image: bool) {
    let docker = docker();
    remove_app().await;
    let (old_img, _) = registry_image(&docker, &format!("{prefix}-old")).await;
    let (new_img, _) = registry_image(&docker, &format!("{prefix}-new")).await;
    let labels = strategy(strategy_name);

    let (rel1, rel2) = (format!("{prefix}-one"), format!("{prefix}-two"));
    apply_release(
        &rel1,
        &[("main", service(1, &old_img, SERVE, labels.clone()))],
    )
    .await;
    let old_id = image_id(&docker, &old_img).await;

    let since = now();
    apply_release(&rel2, &[("main", service(1, &new_img, SERVE, labels))]).await;
    let (old, new) = (container_name("main", &rel1), container_name("main", &rel2));
    assert_eq!(
        app_containers(&docker).await,
        HashMap::from([(new.clone(), true)])
    );

    let cevts = events(&docker, since, "container").await;
    let ievts = events(&docker, since, "image").await;
    let destroyed = first(&cevts, &old, "destroy").expect("old service not removed");
    let created = first(&cevts, &new, "create").expect("new service not created");
    let pulled = first(&ievts, &new_img, "pull").expect("new image not pulled");
    assert!(destroyed < pulled, "old service removed after the pull");
    assert!(pulled < created, "new service created before the pull");

    let deleted = first(&ievts, &old_id, "delete");
    if deletes_image {
        assert!(
            deleted.is_some_and(|d| d < pulled),
            "old image not deleted before the pull"
        );
    } else {
        // the cleanup after the apply removes it
        assert!(
            deleted.is_none_or(|d| d > pulled),
            "old image deleted before the pull"
        );
    }

    cleanup().await;
}

#[tokio::test]
async fn test_kill_then_download_removes_the_service_before_the_pull() {
    assert_removed_before_pull("kill-then-download", "ktd", false).await;
}

#[tokio::test]
async fn test_delete_then_download_deletes_the_image_before_the_pull() {
    assert_removed_before_pull("delete-then-download", "dtd", true).await;
}

#[tokio::test]
async fn test_each_service_follows_its_own_strategy() {
    let docker = docker();
    remove_app().await;
    let plan = [
        ("dtk", "download-then-kill"),
        ("ktd", "kill-then-download"),
        ("dtd", "delete-then-download"),
    ];
    let mut images = HashMap::new();
    for (name, _) in plan {
        let old = registry_image(&docker, &format!("mixed-{name}-old"))
            .await
            .0;
        let new = registry_image(&docker, &format!("mixed-{name}-new"))
            .await
            .0;
        images.insert(name, (old, new));
    }
    let release = |new: bool| -> Vec<(&str, Value)> {
        plan.iter()
            .zip(1..)
            .map(|(&(name, strat), id)| {
                let (old_img, new_img) = &images[name];
                let image = if new { new_img } else { old_img };
                (name, service(id, image, SERVE, strategy(strat)))
            })
            .collect()
    };

    apply_release("mixed-one", &release(false)).await;
    let mut old_ids = HashMap::new();
    for (name, _) in plan {
        old_ids.insert(name, image_id(&docker, &images[name].0).await);
    }

    let since = now();
    apply_release("mixed-two", &release(true)).await;
    let old = |name: &str| container_name(name, "mixed-one");
    let new = |name: &str| container_name(name, "mixed-two");

    let running: HashMap<_, _> = plan.iter().map(|(name, _)| (new(name), true)).collect();
    assert_eq!(app_containers(&docker).await, running);

    let cevts = events(&docker, since, "container").await;
    let ievts = events(&docker, since, "image").await;
    let at = |name: String, action: &str| {
        first(&cevts, &name, action).unwrap_or_else(|| panic!("no {action} for {name}"))
    };
    let pull = |name: &str| first(&ievts, &images[name].1, "pull").expect("image not pulled");
    let deleted = |name: &str| first(&ievts, &old_ids[name], "delete");

    // the new container may be created early, only its start waits for the kill
    let (p, k, s) = (pull("dtk"), at(old("dtk"), "kill"), at(new("dtk"), "start"));
    assert!(p < k && k < s, "dtk: pull/kill/start = {p} {k} {s}");

    for name in ["ktd", "dtd"] {
        let (d, p, c) = (
            at(old(name), "destroy"),
            pull(name),
            at(new(name), "create"),
        );
        assert!(d < p && p < c, "{name}: destroy/pull/create = {d} {p} {c}");
    }
    assert!(deleted("dtd").is_some_and(|d| d < pull("dtd")));
    assert!(deleted("ktd").is_none_or(|d| d > pull("ktd")));

    cleanup().await;
}
