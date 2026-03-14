use std::collections::HashMap;

use bollard::{Docker, query_parameters::PruneImagesOptions};
use serde_json::Value;
use std::time::Duration;

pub const HELIOS_URL: &str = "http://helios-api";
pub const MOCK_REMOTE_URL: &str = "http://mock-remote";

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

async fn get_reports() -> Vec<Value> {
    let res: Vec<Value> = reqwest::get(format!("{MOCK_REMOTE_URL}/mock/reports"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    res
}

pub async fn clear_reports() {
    reqwest::Client::new()
        .delete(format!("{MOCK_REMOTE_URL}/mock/reports"))
        .send()
        .await
        .unwrap();
}

/// Polls reports until one contains the given app/release with the expected update_status.
/// Returns the matching release report value.
pub async fn wait_for_report(
    app_uuid: &str,
    release_uuid: &str,
    expected_status: &str,
    timeout_secs: u64,
) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let release_path = format!("/apps/{app_uuid}/releases/{release_uuid}");
    let status_path = format!("{release_path}/update_status");
    loop {
        let reports = get_reports().await;
        for report in &reports {
            if let Some(device) = report.as_object().and_then(|m| m.values().next())
                && let Some(status) = device.pointer(&status_path).and_then(|s| s.as_str())
                && status == expected_status
            {
                return device.pointer(&release_path).unwrap().clone();
            }
        }

        if tokio::time::Instant::now() >= deadline {
            panic!(
                "Timed out waiting for report with app '{app_uuid}' release '{release_uuid}' status '{expected_status}'. Got: {reports:?}"
            );
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub async fn prune_images() {
    let docker = Docker::connect_with_defaults().unwrap();
    let mut filters = HashMap::new();
    filters.insert("dangling".to_string(), vec!["false".to_string()]);
    let _ = docker
        .prune_images(Some(PruneImagesOptions {
            filters: Some(filters),
        }))
        .await;
}
