#[cfg(test)]
mod tests {
    use serde_json::json;

    #[tokio::test]
    async fn test_service_running() {
        let body = reqwest::get("http://helios:48484/v3/ping")
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "OK")
    }

    #[tokio::test]
    async fn test_initial_state() {
        let body: serde_json::Value = reqwest::get("http://helios:48484/v3/device")
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body, json!({"uuid": "test-uuid", "images": {}}))
    }
}
