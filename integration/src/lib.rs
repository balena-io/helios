#[cfg(test)]
mod tests {
    use std::time::Duration;

    use reqwest::StatusCode;
    use serde_json::json;

    const HELIOS_URL: &str = "http://helios-api";

    async fn wait_for_target_apply() -> serde_json::Value {
        let mut status: serde_json::Value = reqwest::get(format!("{HELIOS_URL}/v3/status"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        while status == "applying_changes" {
            tokio::time::sleep(Duration::from_secs(1)).await;
            status = reqwest::get(format!("{HELIOS_URL}/v3/status"))
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
        }

        status
    }

    #[tokio::test]
    async fn test_service_running() {
        let body = reqwest::get(format!("{HELIOS_URL}/v3/ping"))
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
        assert_eq!(body.get("config"), Some(&json!({})));
        assert_eq!(body.get("images"), Some(&json!({})));
    }

    #[tokio::test]
    async fn test_set_device_target() {
        let client = reqwest::Client::new();
        let target = json!({"apps": {"test-app": {"name": "my-app"}},  "config": {}});
        let body = client
            .post(format!("{HELIOS_URL}/v3/device"))
            .json(&target)
            .send()
            .await
            .unwrap();
        assert_eq!(body.status(), StatusCode::ACCEPTED);
        let status = wait_for_target_apply().await;
        assert_eq!(status, json!({"status": "done"}));
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
        let target = json!({"name": "my-app"});
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

        assert_eq!(app, json!({"id": 0, "name": "my-app"}))
    }
}
