#[cfg(test)]
mod tests {
    use std::{collections::HashMap, time::Duration};

    use bollard::{query_parameters::PruneImagesOptions, Docker};
    use reqwest::StatusCode;
    use serde_json::json;

    const HELIOS_URL: &str = "http://helios-api";

    async fn wait_for_target_apply() -> serde_json::Value {
        loop {
            let status_res: serde_json::Value = reqwest::get(format!("{HELIOS_URL}/v3/status"))
                .await
                .unwrap()
                .json()
                .await
                .unwrap();

            let status = status_res.get("status").unwrap();
            if *status != "applying_changes" {
                return status_res;
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn cleanup() {
        let docker = Docker::connect_with_defaults().unwrap();
        // Delete unused images
        let mut filters = HashMap::new();
        filters.insert("dangling".to_string(), vec!["false".to_string()]);
        let _ = docker
            .prune_images(Some(PruneImagesOptions {
                filters: Some(filters),
            }))
            .await;
    }

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
            .get(format!("{HELIOS_URL}/v3/device/apps/test-app"))
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
            .post(format!("{HELIOS_URL}/v3/device/apps/test-app"))
            .json(&target)
            .send()
            .await
            .unwrap();
        assert_eq!(body.status(), StatusCode::ACCEPTED);
        let status = wait_for_target_apply().await;
        assert_eq!(status, json!({"status": "done"}));

        // test that the app is returned by the API
        let app: serde_json::Value = reqwest::get(format!("{HELIOS_URL}/v3/device/apps/test-app"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        assert_eq!(app, json!({"id": 0, "name": "my-app", "releases": {}}))
    }

    #[tokio::test]
    async fn test_set_app_target_install_images() {
        // clean images
        cleanup().await;

        let client = reqwest::Client::new();
        let target = json!({
            "id": 0,
            "name": "my-new-app-name",
            "releases": {
                "my-release-uuid": {
                    "services": {
                        "service-one": {
                            "id": 1,
                            "image": "ubuntu:latest"
                        },
                        "service-two": {
                            "id": 2,
                            "image": "alpine:latest"
                        }
                    }
                }
            }
        }
        );
        let body = client
            .post(format!("{HELIOS_URL}/v3/device/apps/test-app"))
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

        // test that the app is returned by the API
        let app = device
            .get("apps")
            .and_then(|apps| apps.get("test-app"))
            .unwrap();

        assert_eq!(
            app,
            &json!({
                "id": 0,
                "name": "my-new-app-name",
                "releases": {
                    "my-release-uuid": {
                        "services": {
                            "service-one": {
                                "id": 1,
                                "image": "ubuntu:latest"
                            },
                            "service-two": {
                                "id": 2,
                                "image": "alpine:latest"
                            }
                        }
                    }
                }
            })
        );

        let images = device.get("images").unwrap();

        let ubuntu_img = images.get("ubuntu:latest").unwrap();
        let alpine_img = images.get("alpine:latest").unwrap();

        let ubuntu_img_id = ubuntu_img.get("engine_id").unwrap();
        let alpine_img_id = alpine_img.get("engine_id").unwrap();

        let docker = Docker::connect_with_defaults().unwrap();
        let img = docker.inspect_image("ubuntu:latest").await.unwrap();
        // image ids should match
        assert_eq!(img.id.unwrap(), *ubuntu_img_id);

        let img = docker.inspect_image("alpine:latest").await.unwrap();
        // image ids should match
        assert_eq!(img.id.unwrap(), *alpine_img_id);

        // clean images
        cleanup().await;
    }
}
