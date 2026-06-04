use bollard::Docker;
use bollard::config::ContainerStateStatusEnum;
use bollard::query_parameters::{BuildImageOptions, ListContainersOptions, PushImageOptions};
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::json;

const UPDATER_IMAGE: &str = "registry:5000/test-updater:latest";
const FAILING_UPDATER_IMAGE: &str = "registry:5000/test-failing-updater:latest";
const OVERLAY_IMAGE: &str = "registry:5000/test-overlay:latest";
const FAILING_OVERLAY_IMAGE: &str = "registry:5000/test-failing-overlay:latest";

use super::common::{
    HELIOS_URL, MOCK_REMOTE_URL, clear_reports, prune_images, take_reboot_requested,
    reset_mock_power_state, wait_for_report, wait_for_report_where, wait_for_target_apply,
};

async fn build_test_updater_image(docker: &Docker) {
    let dockerfile =
        b"FROM alpine:3.23\nRUN mkdir -p /app && printf '#!/bin/sh\\necho \"$*\" > ./args.txt\\n' > /app/entry.sh && chmod +x /app/entry.sh\n";

    let mut tar_buf = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(dockerfile.len() as u64);
    header.set_mode(0o644);
    tar_buf
        .append_data(&mut header, "Dockerfile", dockerfile.as_slice())
        .unwrap();
    let context_bytes = tar_buf.into_inner().unwrap();

    let build_opts = BuildImageOptions {
        t: Some(UPDATER_IMAGE.to_string()),
        ..Default::default()
    };

    let mut stream = docker.build_image(
        build_opts,
        None,
        Some(bollard::body_full(context_bytes.into())),
    );
    while let Some(result) = stream.next().await {
        result.expect("image build failed");
    }

    let push_opts = PushImageOptions {
        tag: Some("latest".to_string()),
        ..Default::default()
    };

    let mut stream = docker.push_image("registry:5000/test-updater", Some(push_opts), None);
    while let Some(result) = stream.next().await {
        result.expect("image push failed");
    }

    // remove leftover images after build
    prune_images().await;
}

/// Build and push a minimal hostapp overlay test image.
async fn build_overlay_image(docker: &Docker) {
    let dockerfile = b"FROM alpine:3.23\n\
LABEL io.balena.image.class=overlay\n\
LABEL io.balena.image.kernel-version=6.1.0\n\
LABEL io.balena.image.os-version=6.0.39\n\
LABEL org.opencontainers.image.title=test-overlay\n\
VOLUME /boot\n";

    let mut tar_buf = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(dockerfile.len() as u64);
    header.set_mode(0o644);
    tar_buf
        .append_data(&mut header, "Dockerfile", dockerfile.as_slice())
        .unwrap();
    let context_bytes = tar_buf.into_inner().unwrap();

    let build_opts = BuildImageOptions {
        t: Some(OVERLAY_IMAGE.to_string()),
        ..Default::default()
    };

    let mut stream = docker.build_image(
        build_opts,
        None,
        Some(bollard::body_full(context_bytes.into())),
    );
    while let Some(result) = stream.next().await {
        result.expect("overlay image build failed");
    }

    let push_opts = PushImageOptions {
        tag: Some("latest".to_string()),
        ..Default::default()
    };

    let mut stream = docker.push_image("registry:5000/test-overlay", Some(push_opts), None);
    while let Some(result) = stream.next().await {
        result.expect("overlay image push failed");
    }

    prune_images().await;
}

/// Build and push an overlay image whose activation fails
async fn build_failing_overlay_image(docker: &Docker) {
    let dockerfile = b"FROM alpine:3.23\nVOLUME /boot\nRUN mkdir -p /hooks && printf '#!/bin/sh\\nexit 1\\n' > /hooks/start && chmod +x /hooks/start\n";

    let mut tar_buf = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(dockerfile.len() as u64);
    header.set_mode(0o644);
    tar_buf
        .append_data(&mut header, "Dockerfile", dockerfile.as_slice())
        .unwrap();
    let context_bytes = tar_buf.into_inner().unwrap();

    let build_opts = BuildImageOptions {
        t: Some(FAILING_OVERLAY_IMAGE.to_string()),
        ..Default::default()
    };

    let mut stream = docker.build_image(
        build_opts,
        None,
        Some(bollard::body_full(context_bytes.into())),
    );
    while let Some(result) = stream.next().await {
        result.expect("failing overlay image build failed");
    }

    let push_opts = PushImageOptions {
        tag: Some("latest".to_string()),
        ..Default::default()
    };

    let mut stream =
        docker.push_image("registry:5000/test-failing-overlay", Some(push_opts), None);
    while let Some(result) = stream.next().await {
        result.expect("failing overlay image push failed");
    }

    prune_images().await;
}

async fn build_failing_updater_image(docker: &Docker) {
    let dockerfile =
        b"FROM alpine:3.23\nRUN mkdir -p /app && printf '#!/bin/sh\\nexit 1\\n' > /app/entry.sh && chmod +x /app/entry.sh\n";

    let mut tar_buf = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(dockerfile.len() as u64);
    header.set_mode(0o644);
    tar_buf
        .append_data(&mut header, "Dockerfile", dockerfile.as_slice())
        .unwrap();
    let context_bytes = tar_buf.into_inner().unwrap();

    let build_opts = BuildImageOptions {
        t: Some(FAILING_UPDATER_IMAGE.to_string()),
        ..Default::default()
    };

    let mut stream = docker.build_image(
        build_opts,
        None,
        Some(bollard::body_full(context_bytes.into())),
    );
    while let Some(result) = stream.next().await {
        result.expect("failing image build failed");
    }

    let push_opts = PushImageOptions {
        tag: Some("latest".to_string()),
        ..Default::default()
    };

    let mut stream = docker.push_image("registry:5000/test-failing-updater", Some(push_opts), None);
    while let Some(result) = stream.next().await {
        result.expect("failing image push failed");
    }

    prune_images().await;
}

#[tokio::test]
async fn test_remote_poll_user_app() {
    let client = reqwest::Client::new();

    let device_target = json!({
        "name": "test-device",
        "apps": {
            "remote-app-uuid": {
                "id": 100,
                "name": "my-remote-app"
            },
        }
    });

    let res = client
        .put(format!("{MOCK_REMOTE_URL}/mock/state"))
        .json(&device_target)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = client
        .post(format!("{HELIOS_URL}/v1/update"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    let status = wait_for_target_apply().await;

    // we expect an aborted state because of the hostapp, but the
    // user app should have been created
    assert_eq!(status, json!({"status": "aborted"}));

    let device: serde_json::Value = reqwest::get(format!("{HELIOS_URL}/v3/device"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let apps = device.get("apps").unwrap();
    assert!(
        apps.get("remote-app-uuid").is_some(),
        "remote app should be present in device state"
    );

    // Clean up helios state by applying an empty target before removing mock state
    let empty_target = json!({"name": "test-device", "apps": {}});
    client
        .put(format!("{MOCK_REMOTE_URL}/mock/state"))
        .json(&empty_target)
        .send()
        .await
        .unwrap();

    let res = client
        .post(format!("{HELIOS_URL}/v1/update"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    wait_for_target_apply().await;

    client
        .delete(format!("{MOCK_REMOTE_URL}/mock/state"))
        .send()
        .await
        .unwrap();
}

#[tokio::test]
async fn test_remote_poll_hostos_update() {
    let docker = Docker::connect_with_defaults().unwrap();
    build_test_updater_image(&docker).await;
    build_overlay_image(&docker).await;

    let client = reqwest::Client::new();

    const APP_UUID: &str = "test-hostapp-uuid-abc";
    const RELEASE_COMMIT: &str = "aabbccddeeff00112233445566778899";

    // Build JSON with dynamic keys using serde_json::Map
    let mut releases = serde_json::Map::new();
    releases.insert(
        RELEASE_COMMIT.to_string(),
        json!({
            "services": {
                "hostapp": {
                    "id": 201,
                    "image": UPDATER_IMAGE,
                    "labels": {
                        "io.balena.private.updater": UPDATER_IMAGE
                    },
                    "composition": {
                        "labels": {
                            "io.balena.image.class": "hostapp",
                            "io.balena.private.hostapp.board-rev": "test-board-rev-123"
                        }
                    }
                },
                "kernel-modules": {
                    "id": 202,
                    "image": OVERLAY_IMAGE,
                    "labels": {},
                    "composition": {
                        "labels": {
                            "io.balena.image.class": "overlay",
                            "io.balena.update.requires-reboot": "1"
                        }
                    }
                },
                "extra-modules": {
                    "id": 203,
                    "image": OVERLAY_IMAGE,
                    "labels": {},
                    "composition": {
                        "labels": {
                            "io.balena.image.class": "overlay",
                            "io.balena.update.requires-reboot": "1"
                        }
                    }
                }
            }
        }),
    );
    let app_obj = json!({
        "id": 200,
        "name": "generic-aarch64",
        "is_host": true,
        "releases": serde_json::Value::Object(releases.clone())
    });
    let mut apps = serde_json::Map::new();
    apps.insert(APP_UUID.to_string(), app_obj);
    let device_target = json!({
        "name": "test-device",
        "apps": serde_json::Value::Object(apps)
    });

    clear_reports().await;

    let res = client
        .put(format!("{MOCK_REMOTE_URL}/mock/state"))
        .json(&device_target)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = client
        .post(format!("{HELIOS_URL}/v1/update"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    let status = wait_for_target_apply().await;

    assert_ne!(
        status,
        json!({"status": "aborted"}),
        "a hostapp update with overlays must plan and apply without aborting, got: {status}"
    );

    let args_content = tokio::fs::read_to_string("/tmp/run/balenahup/args.txt")
        .await
        .expect("args.txt should exist after hostOS update");

    assert!(
        args_content.contains("--app-uuid"),
        "args should contain --app-uuid, got: {args_content}"
    );
    assert!(
        args_content.contains(APP_UUID),
        "argsshould contain app uuid value, got: {args_content}"
    );
    assert!(
        args_content.contains("--release-commit"),
        "args should contain --release-commit, got: {args_content}"
    );
    assert!(
        args_content.contains(RELEASE_COMMIT),
        "args should contain release commit, got: {args_content}"
    );
    assert!(
        args_content.contains("--target-image-uri"),
        "args should contain --target-image-uri, got: {args_content}"
    );
    assert!(
        args_content.contains(UPDATER_IMAGE),
        "args should contain updater image uri, got: {args_content}"
    );
    assert!(
        args_content.contains("--no-reboot"),
        "args should contain --no-reboot (helios owns the reboot now), got: {args_content}"
    );

    let breadcrumb = format!("/tmp/run/balenahup-{RELEASE_COMMIT}-breadcrumb");
    assert!(
        tokio::fs::metadata(&breadcrumb).await.is_ok(),
        "breadcrumb file should exist at {breadcrumb}"
    );

    // Each target overlay container must have been created, run under the
    // `extension` runtime, and exited 0 BEFORE the (balenahup) reboot
    let mut overlay_volumes: Vec<(&str, String)> = Vec::new();
    for service_name in ["kernel-modules", "extra-modules"] {
        let mut filters = std::collections::HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![
                "io.balena.image.class=overlay".to_string(),
                format!("io.balena.service-name={service_name}"),
            ],
        );
        filters.insert(
            "name".to_string(),
            vec![format!("{service_name}_{RELEASE_COMMIT}")],
        );
        let containers = docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters: Some(filters),
                ..Default::default()
            }))
            .await
            .unwrap();
        assert_eq!(
            containers.len(),
            1,
            "exactly one '{service_name}' overlay container should be deployed, got: {containers:?}"
        );
        let inspect = docker
            .inspect_container(containers[0].id.as_deref().unwrap(), None)
            .await
            .unwrap();
        // Read the ext_* volume name off the container's own mount rather than
        // recomputing it from the image id.
        let boot_volume = inspect
            .mounts
            .as_ref()
            .and_then(|mounts| {
                mounts
                    .iter()
                    .find(|m| m.destination.as_deref() == Some("/boot"))
            })
            .and_then(|m| m.name.clone())
            .unwrap_or_else(|| {
                panic!("'{service_name}' overlay should mount a named volume at /boot")
            });
        overlay_volumes.push((service_name, boot_volume));

        let state = inspect
            .state
            .expect("overlay container should have state");
        assert_eq!(
            state.status,
            Some(ContainerStateStatusEnum::EXITED),
            "'{service_name}' overlay container should have exited"
        );
        assert_eq!(
            state.exit_code,
            Some(0),
            "'{service_name}' overlay should exit 0 (deployed), got: {state:?}"
        );
    }

    // Each image-declared VOLUME must be backed by a named ext_* volume
    // carrying the image's io.balena.image.* labels.
    let all_volumes = docker
        .list_volumes(None::<bollard::query_parameters::ListVolumesOptions>)
        .await
        .unwrap()
        .volumes
        .unwrap_or_default();

    for (service_name, expected) in &overlay_volumes {
        // The `ext_` prefix and `_boot` suffix are what the OS volume discovery
        // matches on; the middle segment is the image content id.
        assert!(
            expected.starts_with(&format!("ext_{service_name}_")) && expected.ends_with("_boot"),
            "overlay volume should follow the ext_<service>_<id>_<dest> convention, got '{expected}'"
        );
        let vol = all_volumes
            .iter()
            .find(|v| &v.name == expected)
            .unwrap_or_else(|| {
                let names: Vec<&str> = all_volumes.iter().map(|v| v.name.as_str()).collect();
                panic!("expected volume '{expected}', have: {names:?}")
            });

        assert_eq!(
            vol.labels.get("io.balena.image.class").map(String::as_str),
            Some("overlay"),
            "volume '{expected}' must carry the image class label"
        );
        assert_eq!(
            vol.labels
                .get("io.balena.image.kernel-version")
                .map(String::as_str),
            Some("6.1.0"),
            "volume '{expected}' must carry the kernel-version label the sweep reads"
        );
        assert_eq!(
            vol.labels
                .get("io.balena.image.os-version")
                .map(String::as_str),
            Some("6.0.39"),
            "volume '{expected}' must carry the os-version label the sweep reads"
        );
        assert!(
            !vol.labels.contains_key("org.opencontainers.image.title"),
            "only io.balena.image.* labels are copied, got: {:?}",
            vol.labels
        );
    }

    // Assert helios issued the coordinated reboot itself.
    let mut reboot_observed = false;
    for _ in 0..15 {
        if take_reboot_requested().await {
            reboot_observed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    assert!(
        reboot_observed,
        "helios should have issued the activation reboot via logind \
         (org.freedesktop.login1.Manager.Reboot), flipping the mock's MockState \
         to `rebooting`, but MockState never became `rebooting`"
    );

    reset_mock_power_state().await;

    let overlays_running = |rel: &serde_json::Value| {
        ["kernel-modules", "extra-modules"]
            .iter()
            .all(|svc| rel["services"][svc]["status"] == "Running")
    };
    let release_report = wait_for_report_where(
        APP_UUID,
        RELEASE_COMMIT,
        "applying changes",
        overlays_running,
        30,
    )
    .await;
    assert_eq!(
        release_report["services"]["hostapp"]["status"], "Installing",
        "the hostapp never reaches meta.build in this harness, got: {release_report}"
    );

    // Drop one overlay from the target.
    let mut reduced = releases.clone();
    reduced
        .get_mut(RELEASE_COMMIT)
        .unwrap()
        .get_mut("services")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("extra-modules");
    let reduced_target = json!({
        "name": "test-device",
        "apps": {
            APP_UUID: {
                "id": 200,
                "name": "generic-aarch64",
                "is_host": true,
                "releases": serde_json::Value::Object(reduced)
            }
        }
    });

    let res = client
        .put(format!("{MOCK_REMOTE_URL}/mock/state"))
        .json(&reduced_target)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = client
        .post(format!("{HELIOS_URL}/v1/update"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    wait_for_target_apply().await;

    for (service_name, expected) in [("extra-modules", 0), ("kernel-modules", 1)] {
        let mut filters = std::collections::HashMap::new();
        filters.insert(
            "name".to_string(),
            vec![format!("{service_name}_{RELEASE_COMMIT}")],
        );
        let containers = docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters: Some(filters),
                ..Default::default()
            }))
            .await
            .unwrap();
        assert_eq!(
            containers.len(),
            expected,
            "after dropping 'extra-modules' from the target, expected {expected} \
             '{service_name}' container(s), got: {containers:?}"
        );
    }

    // Removing an overlay must leave its ext_* volume behind.
    let after = docker
        .list_volumes(None::<bollard::query_parameters::ListVolumesOptions>)
        .await
        .unwrap()
        .volumes
        .unwrap_or_default();
    let dropped_volume = &overlay_volumes
        .iter()
        .find(|(service_name, _)| *service_name == "extra-modules")
        .expect("extra-modules volume name was captured above")
        .1;
    assert!(
        after.iter().any(|v| &v.name == dropped_volume),
        "removing an overlay must leave its ext_* volume '{dropped_volume}' for the OS to reap"
    );

    clear_reports().await;
    client
        .delete(format!("{MOCK_REMOTE_URL}/mock/state"))
        .send()
        .await
        .unwrap();
}

#[tokio::test]
async fn test_hostos_update_aborts_on_overlay_activation_failure() {
    let docker = Docker::connect_with_defaults().unwrap();
    build_test_updater_image(&docker).await;
    build_failing_overlay_image(&docker).await;

    let client = reqwest::Client::new();

    const APP_UUID: &str = "test-hostapp-overlay-fail-uuid";
    const RELEASE_COMMIT: &str = "0011223344556677889900aabbccddee";

    let mut releases = serde_json::Map::new();
    releases.insert(
        RELEASE_COMMIT.to_string(),
        json!({
            "services": {
                "hostapp": {
                    "id": 401,
                    "image": UPDATER_IMAGE,
                    "labels": { "io.balena.private.updater": UPDATER_IMAGE },
                    "composition": { "labels": {
                        "io.balena.image.class": "hostapp",
                        "io.balena.private.hostapp.board-rev": "test-board-rev-fail"
                    }}
                },
                "kernel-modules": {
                    "id": 402,
                    "image": FAILING_OVERLAY_IMAGE,
                    "labels": {},
                    "composition": { "labels": {
                        "io.balena.image.class": "overlay",
                        "io.balena.update.requires-reboot": "1"
                    }}
                }
            }
        }),
    );
    let app_obj = json!({
        "id": 400,
        "name": "generic-aarch64",
        "is_host": true,
        "releases": serde_json::Value::Object(releases)
    });
    let mut apps = serde_json::Map::new();
    apps.insert(APP_UUID.to_string(), app_obj);
    let device_target = json!({
        "name": "test-device",
        "apps": serde_json::Value::Object(apps)
    });

    clear_reports().await;

    // Start from a clean reboot state so the negative reboot assertion below is
    // not confused by a prior test's reboot.
    reset_mock_power_state().await;

    let res = client
        .put(format!("{MOCK_REMOTE_URL}/mock/state"))
        .json(&device_target)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = client
        .post(format!("{HELIOS_URL}/v1/update"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    // The overlay deploy is ordered before the hostapp install and the reboot,
    // so a failed activation aborts the whole host update at that point.
    let status = wait_for_target_apply().await;
    assert_eq!(
        status,
        json!({"status": "aborted"}),
        "a failed overlay activation must abort the host update, got: {status}"
    );

    // The install never ran, so no breadcrumb was written for this release.
    let breadcrumb = format!("/tmp/run/balenahup-{RELEASE_COMMIT}-breadcrumb");
    assert!(
        tokio::fs::metadata(&breadcrumb).await.is_err(),
        "breadcrumb must NOT exist at {breadcrumb}: install must not run when an overlay fails"
    );

    // The overlay must not have activated cleanly: helios leaves the container
    // in place (so it derives Failed), and it must not be in the exited-0 state
    // a successful one-shot activation would leave.
    let mut filters = std::collections::HashMap::new();
    filters.insert(
        "name".to_string(),
        vec![format!("kernel-modules_{RELEASE_COMMIT}")],
    );
    let containers = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(filters),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(
        containers.len(),
        1,
        "the failed overlay container should be left in place, got: {containers:?}"
    );
    let inspect = docker
        .inspect_container(containers[0].id.as_deref().unwrap(), None)
        .await
        .unwrap();
    let state = inspect.state.expect("overlay container should have state");
    let cleanly_deployed =
        state.status == Some(ContainerStateStatusEnum::EXITED) && state.exit_code == Some(0);
    assert!(
        !cleanly_deployed,
        "overlay activation must not have succeeded, got state: {state:?}"
    );

    // helios must NOT issue the coordinated reboot when the update aborts. The
    // apply already converged to `aborted`, so a reboot would have fired during
    // it; confirm the mock's reboot state was never tripped.
    assert!(
        !take_reboot_requested().await,
        "helios must not reboot when an overlay activation fails"
    );

    clear_reports().await;
    client
        .delete(format!("{MOCK_REMOTE_URL}/mock/state"))
        .send()
        .await
        .unwrap();
}

#[tokio::test]
async fn test_hostos_update_retry_exhaustion() {
    let docker = Docker::connect_with_defaults().unwrap();
    build_failing_updater_image(&docker).await;

    let client = reqwest::Client::new();

    const APP_UUID: &str = "test-hostapp-retry-uuid";
    const RELEASE_COMMIT: &str = "ff00112233445566778899aabbccddee";

    let mut releases = serde_json::Map::new();
    releases.insert(
        RELEASE_COMMIT.to_string(),
        json!({
            "services": {
                "hostapp": {
                    "id": 301,
                    "image": FAILING_UPDATER_IMAGE,
                    "labels": {
                        "io.balena.private.updater": FAILING_UPDATER_IMAGE
                    },
                    "composition": {
                        "labels": {
                            "io.balena.image.class": "hostapp",
                            "io.balena.private.hostapp.board-rev": "test-board-rev-retry"
                        }
                    }
                }
            }
        }),
    );
    let app_obj = json!({
        "id": 300,
        "name": "generic-aarch64",
        "is_host": true,
        "releases": serde_json::Value::Object(releases)
    });
    let mut apps = serde_json::Map::new();
    apps.insert(APP_UUID.to_string(), app_obj);
    let device_target = json!({
        "name": "test-device",
        "apps": serde_json::Value::Object(apps)
    });

    clear_reports().await;

    let res = client
        .put(format!("{MOCK_REMOTE_URL}/mock/state"))
        .json(&device_target)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = client
        .post(format!("{HELIOS_URL}/v1/update"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    let status = wait_for_target_apply().await;

    // After exhausting retries (install_attempts > 3), the exception fires
    // and seek converges to "aborted"
    assert_eq!(status, json!({"status": "aborted"}));

    // The updater always fails, so no breadcrumb should exist
    let breadcrumb = format!("/tmp/run/balenahup-{RELEASE_COMMIT}-breadcrumb");
    assert!(
        tokio::fs::metadata(&breadcrumb).await.is_err(),
        "breadcrumb file should NOT exist at {breadcrumb} because install always fails"
    );

    // Verify reported state shows aborted with Installing service status
    let release_report = wait_for_report(APP_UUID, RELEASE_COMMIT, "aborted", 10).await;
    assert_eq!(
        release_report["services"]["hostapp"]["status"],
        "Installing"
    );

    clear_reports().await;
    client
        .delete(format!("{MOCK_REMOTE_URL}/mock/state"))
        .send()
        .await
        .unwrap();
}

#[tokio::test]
async fn test_rejected_app_is_reported() {
    let client = reqwest::Client::new();

    const APP_UUID: &str = "rejected-app-uuid";
    const RELEASE_UUID: &str = "c8b48659434e80a8b3adc0c5ad1e347a";

    // A release with a malformed service `command` (unclosed quote) fails
    // per-release deserialization in helios-remote-model and lands as a
    // rejection rather than aborting the whole target.
    let mut releases = serde_json::Map::new();
    releases.insert(
        RELEASE_UUID.to_string(),
        json!({
            "id": 7,
            "services": {
                "main": {
                    "id": 3,
                    "image_id": 4,
                    "image": "registry:5000/test-rejected:latest",
                    "composition": {
                        "command": "echo 'hello world",
                    }
                }
            }
        }),
    );
    let app_obj = json!({
        "id": 400,
        "name": "rejected-app",
        "releases": serde_json::Value::Object(releases),
    });
    let mut apps = serde_json::Map::new();
    apps.insert(APP_UUID.to_string(), app_obj);
    let device_target = json!({
        "name": "test-device",
        "apps": serde_json::Value::Object(apps),
    });

    clear_reports().await;

    let res = client
        .put(format!("{MOCK_REMOTE_URL}/mock/state"))
        .json(&device_target)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = client
        .post(format!("{HELIOS_URL}/v1/update"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    // No hostapp in the target, so the overall apply aborts — same as the
    // baseline user-app test. The rejection is reported independently.
    let status = wait_for_target_apply().await;
    assert_eq!(status, json!({"status": "aborted"}));

    let release_report = wait_for_report(APP_UUID, RELEASE_UUID, "rejected", 10).await;
    assert!(
        release_report["services"]
            .as_object()
            .map(|m| m.is_empty())
    // A failed activation is terminal and must surface as such to the API:
    // Dead, not a status that reads as work still in progress. This is the
    // only place the Failed to Dead mapping is exercised end to end.
    let release_report = wait_for_report_where(
        APP_UUID,
        RELEASE_COMMIT,
        "aborted",
        |rel| rel["services"]["kernel-modules"]["status"] == "Dead",
        30,
    )
    .await;
    assert_eq!(
        release_report["services"]["kernel-modules"]["status"], "Dead",
        "a failed overlay activation must report as Dead, got: {release_report}"
    );

            .unwrap_or(false),
        "rejected release should have no services, got: {release_report}"
    );

    let empty_target = json!({"name": "test-device", "apps": {}});
    client
        .put(format!("{MOCK_REMOTE_URL}/mock/state"))
        .json(&empty_target)
        .send()
        .await
        .unwrap();
    let res = client
        .post(format!("{HELIOS_URL}/v1/update"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    wait_for_target_apply().await;

    clear_reports().await;
    client
        .delete(format!("{MOCK_REMOTE_URL}/mock/state"))
        .send()
        .await
        .unwrap();
}

#[tokio::test]
async fn test_remote_poll_user_app_reports_done() {
    prune_images().await;

    let client = reqwest::Client::new();

    const APP_UUID: &str = "report-app-uuid";
    const RELEASE_UUID: &str = "ddeeff00112233445566778899aabbcc";

    let mut releases = serde_json::Map::new();
    releases.insert(
        RELEASE_UUID.to_string(),
        json!({
            "id": 8,
            "services": {
                "main": {
                    "id": 401,
                    "image_id": 402,
                    "image": "alpine:latest",
                    "composition": {
                        "command": ["sleep", "infinity"],
                    }
                }
            }
        }),
    );
    let app_obj = json!({
        "id": 500,
        "name": "report-app",
        "releases": serde_json::Value::Object(releases),
    });
    let mut apps = serde_json::Map::new();
    apps.insert(APP_UUID.to_string(), app_obj);
    let device_target = json!({
        "name": "test-device",
        "apps": serde_json::Value::Object(apps),
    });

    clear_reports().await;

    let res = client
        .put(format!("{MOCK_REMOTE_URL}/mock/state"))
        .json(&device_target)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = client
        .post(format!("{HELIOS_URL}/v1/update"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    // The remote target has no hostapp, so the overall apply converges
    // to "aborted" (same pattern as the other polling tests). The user
    // app still installs, and its per-release status reports as "done".
    let status = wait_for_target_apply().await;
    assert_eq!(status, json!({"status": "aborted"}));

    let release_report = wait_for_report(APP_UUID, RELEASE_UUID, "done", 30).await;
    assert_eq!(release_report["services"]["main"]["status"], "Running");

    let empty_target = json!({"name": "test-device", "apps": {}});
    client
        .put(format!("{MOCK_REMOTE_URL}/mock/state"))
        .json(&empty_target)
        .send()
        .await
        .unwrap();
    let res = client
        .post(format!("{HELIOS_URL}/v1/update"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    wait_for_target_apply().await;

    clear_reports().await;
    client
        .delete(format!("{MOCK_REMOTE_URL}/mock/state"))
        .send()
        .await
        .unwrap();
}
