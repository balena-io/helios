use bollard::Docker;
use bollard::query_parameters::{BuildImageOptions, PushImageOptions};
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::json;

const UPDATER_IMAGE: &str = "registry:5000/test-updater:latest";

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

    // we expectd an aborted state because it has to wait for the reboot
    assert_eq!(status, json!({"status": "aborted"}));

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
    // FIXME: this needs to be re-added once helios handles locks
    // assert!(
    //     args_content.contains("--no-reboot"),
    //     "args should contain --no-reboot, got: {args_content}"
    // );

    let breadcrumb = format!("/tmp/run/balenahup-{RELEASE_COMMIT}-breadcrumb");
    assert!(
        tokio::fs::metadata(&breadcrumb).await.is_ok(),
        "breadcrumb file should exist at {breadcrumb}"
    );

    // verify state report includes the hostapp with aborted status
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
