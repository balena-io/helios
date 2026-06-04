use bollard::Docker;
use bollard::config::ContainerStateStatusEnum;
use bollard::query_parameters::{BuildImageOptions, ListContainersOptions, PushImageOptions};
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::json;

const UPDATER_IMAGE: &str = "registry:5000/test-updater:latest";
const FAILING_UPDATER_IMAGE: &str = "registry:5000/test-failing-updater:latest";
const OVERLAY_IMAGE: &str = "registry:5000/test-overlay:latest";

use super::common::{
    HELIOS_URL, MOCK_REMOTE_URL, clear_reports, prune_images, wait_for_report,
    wait_for_target_apply,
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
    let dockerfile = b"FROM alpine:3.23\nVOLUME /boot\n";

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
                }
            }
        }),
    );
    let app_obj = json!({
        "id": 200,
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

    assert_ne!(
        status,
        json!({"status": "aborted"}),
        "Phase 2 removed the install-defer; the apply must no longer abort, got: {status}"
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

    // The overlay container must have been created, run under the `extension`
    // runtime, and exited 0 BEFORE the (balenahup-issued) reboot: helios gates
    // the host install on every target overlay being deployed first.
    let mut filters = std::collections::HashMap::new();
    filters.insert(
        "label".to_string(),
        vec![
            "io.balena.image.class=overlay".to_string(),
            "io.balena.service-name=kernel-modules".to_string(),
            format!("io.balena.private.hostapp.release={RELEASE_COMMIT}"),
        ],
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
        "exactly one overlay container should be deployed, got: {containers:?}"
    );
    let inspect = docker
        .inspect_container(containers[0].id.as_deref().unwrap(), None)
        .await
        .unwrap();
    let state = inspect.state.expect("overlay container should have state");
    assert_eq!(
        state.status,
        Some(ContainerStateStatusEnum::EXITED),
        "overlay container should have exited"
    );
    assert_eq!(
        state.exit_code,
        Some(0),
        "overlay should exit 0 (deployed), got: {state:?}"
    );

    // Assert helios issued the coordinated reboot itself.
    let mut reboot_observed = false;
    for _ in 0..15 {
        let out = tokio::process::Command::new("dbus-send")
            .args([
                "--system",
                "--print-reply",
                "--dest=org.freedesktop.login1",
                "/org/freedesktop/login1",
                "org.freedesktop.DBus.Properties.Get",
                "string:org.freedesktop.login1.Manager",
                "string:MockState",
            ])
            .output()
            .await;
        if let Ok(o) = out {
            if o.status.success() && String::from_utf8_lossy(&o.stdout).contains("rebooting") {
                reboot_observed = true;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    assert!(
        reboot_observed,
        "helios should have issued the activation reboot via logind \
         (org.freedesktop.login1.Manager.Reboot), flipping the mock's MockState \
         to `rebooting`, but MockState never became `rebooting`"
    );

    let _ = tokio::process::Command::new("dbus-send")
        .args([
            "--system",
            "--print-reply",
            "--dest=org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager.MockReset",
        ])
        .output()
        .await;

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
