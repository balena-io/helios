use std::time::Duration;

pub const HELIOS_URL: &str = "http://helios-api";

pub async fn wait_for_target_apply() -> serde_json::Value {
    tokio::time::sleep(Duration::from_secs(1)).await;
    loop {
        let status_res: serde_json::Value = reqwest::get(format!("{HELIOS_URL}/v3/status"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        let status = status_res.get("status").unwrap();
        if *status != "applying changes" {
            return status_res;
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
