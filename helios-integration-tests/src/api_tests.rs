use bollard::Docker;
use reqwest::StatusCode;
use serde_json::json;

use super::common::{
    HELIOS_URL, clear_reports, prune_images, wait_for_report, wait_for_target_apply,
};

const TEST_APP_UUID: &str = "test-app";

#[tokio::test]
async fn test_service_running() {
    let body = reqwest::get(format!("{HELIOS_URL}/ping"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "OK")
}

#[tokio::test]
async fn test_initial_state() {
    let body: serde_json::Value = reqwest::get(format!("{HELIOS_URL}/v3/device"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body.get("uuid"), Some(&json!("test-uuid")));
    assert_eq!(body.get("apps"), Some(&json!({})));
    assert_eq!(body.get("images"), Some(&json!({})));
}

#[tokio::test]
async fn test_get_app_state() {
    let client = reqwest::Client::new();
    let body = client
        .get(format!("{HELIOS_URL}/v3/device/apps/{TEST_APP_UUID}"))
        .send()
        .await
        .unwrap();
    assert_eq!(body.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_set_app_target() {
    let client = reqwest::Client::new();
    let target = json!({"id": 0, "name": "my-app"});
    let body = client
        .post(format!("{HELIOS_URL}/v3/device/apps/{TEST_APP_UUID}"))
        .json(&target)
        .send()
        .await
        .unwrap();
    assert_eq!(body.status(), StatusCode::ACCEPTED);
    let status = wait_for_target_apply().await;
    assert_eq!(status, json!({"status": "done"}));

    let app: serde_json::Value =
        reqwest::get(format!("{HELIOS_URL}/v3/device/apps/{TEST_APP_UUID}"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

    assert_eq!(app, json!({"id": 0, "name": "my-app", "releases": {}}));

    // remove the test app
    let body = client
        .delete(format!("{HELIOS_URL}/v3/device/apps/{TEST_APP_UUID}"))
        .send()
        .await
        .unwrap();
    assert_eq!(body.status(), StatusCode::ACCEPTED);
    let status = wait_for_target_apply().await;
    assert_eq!(status, json!({"status": "done"}));
}

#[tokio::test]
async fn test_set_app_target_install_images() {
    prune_images().await;
    clear_reports().await;

    let client = reqwest::Client::new();
    let target = json!({
        "id": 0,
        "name": "my-new-app-name",
        "releases": {
            "my-release-uuid": {
                "services": {
                    "service-one": {
                        "id": 1,
                        "image": "ubuntu:latest",
                        "composition": {
                            "command": "sleep infinity"
                        }
                    },
                    "service-two": {
                        "id": 2,
                        "image": "alpine:latest",
                        "composition": {
                            "command": ["sleep", "10"],
                            "labels": {
                                "my-label": "true"
                            }
                        }
                    }
                }
            }
        }
    });
    let body = client
        .post(format!("{HELIOS_URL}/v3/device/apps/{TEST_APP_UUID}"))
        .json(&target)
        .send()
        .await
        .unwrap();
    assert_eq!(body.status(), StatusCode::ACCEPTED);

    let status = wait_for_target_apply().await;
    assert_eq!(status, json!({"status": "done"}));

    let device: serde_json::Value = reqwest::get(format!("{HELIOS_URL}/v3/device"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let app = device
        .get("apps")
        .and_then(|apps| apps.get(TEST_APP_UUID))
        .unwrap();

    let svc_one_container_id = app
        .get("releases")
        .and_then(|r| r.get("my-release-uuid"))
        .and_then(|r| r.get("services"))
        .and_then(|s| s.get("service-one"))
        .and_then(|s| s.get("container"))
        .and_then(|c| c.get("id"))
        .unwrap()
        .as_str()
        .unwrap();

    let svc_two_container_id = app
        .get("releases")
        .and_then(|r| r.get("my-release-uuid"))
        .and_then(|r| r.get("services"))
        .and_then(|s| s.get("service-two"))
        .and_then(|s| s.get("container"))
        .and_then(|c| c.get("id"))
        .unwrap()
        .as_str()
        .unwrap();

    let images = device.get("images").unwrap();

    let ubuntu_img = images.get("ubuntu:latest").unwrap();
    let alpine_img = images.get("alpine:latest").unwrap();

    let ubuntu_img_id = ubuntu_img.get("engine_id").unwrap();
    let alpine_img_id = alpine_img.get("engine_id").unwrap();

    let docker = Docker::connect_with_defaults().unwrap();
    let img = docker.inspect_image("ubuntu:latest").await.unwrap();
    assert_eq!(img.id.unwrap(), *ubuntu_img_id);

    let img = docker.inspect_image("alpine:latest").await.unwrap();
    assert_eq!(img.id.unwrap(), *alpine_img_id);

    // the containers exist and have the right configuration
    let container = docker
        .inspect_container(svc_one_container_id, None)
        .await
        .unwrap();
    let config = container.config.unwrap();
    assert_eq!(config.cmd.unwrap(), vec!["sleep", "infinity"]);
    assert_eq!(
        config.labels.unwrap().get("io.balena.app-uuid").unwrap(),
        TEST_APP_UUID
    );

    let container = docker
        .inspect_container(svc_two_container_id, None)
        .await
        .unwrap();

    let config = container.config.unwrap();
    assert_eq!(config.cmd.unwrap(), vec!["sleep", "10"]);
    let labels = config.labels.unwrap();
    assert_eq!(labels.get("io.balena.app-uuid").unwrap(), TEST_APP_UUID);
    assert_eq!(labels.get("my-label").unwrap(), "true");

    // verify state report was sent with correct data
    let release_report = wait_for_report(TEST_APP_UUID, "my-release-uuid", "done", 10).await;
    assert_eq!(
        release_report["services"]["service-one"]["status"],
        "Running"
    );
    assert_eq!(
        release_report["services"]["service-two"]["status"],
        "Running"
    );

    // remove the test app
    clear_reports().await;
    let body = client
        .delete(format!("{HELIOS_URL}/v3/device/apps/{TEST_APP_UUID}"))
        .send()
        .await
        .unwrap();
    assert_eq!(body.status(), StatusCode::ACCEPTED);
    let status = wait_for_target_apply().await;
    assert_eq!(status, json!({"status": "done"}));

    // images should be cleaned up after removing the app
    assert!(docker.inspect_image("ubuntu:latest").await.is_err());
    assert!(docker.inspect_image("alpine:latest").await.is_err());
}
