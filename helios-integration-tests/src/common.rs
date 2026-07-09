use std::collections::HashMap;

use bollard::{Docker, query_parameters::PruneImagesOptions};
use serde_json::Value;
use std::time::Duration;
use tokio_stream::StreamExt;

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

pub async fn clear_reports() {
    reqwest::Client::new()
        .delete(format!("{MOCK_REMOTE_URL}/mock/reports"))
        .send()
        .await
        .unwrap();
}

/// Connects to the mock-remote NDJSON reports stream and waits until a report
/// contains the given app/release with the expected update_status.
/// Returns the matching release report value.
pub async fn wait_for_report(
    app_uuid: &str,
    release_uuid: &str,
    expected_status: &str,
    timeout_secs: u64,
) -> Value {
    let release_path = format!("/apps/{app_uuid}/releases/{release_uuid}");
    let status_path = format!("{release_path}/update_status");

    let response = reqwest::get(format!("{MOCK_REMOTE_URL}/mock/reports"))
        .await
        .unwrap();
    let mut stream = response.bytes_stream();
    let mut buf = String::new();

    let result = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            buf.push_str(&String::from_utf8_lossy(&chunk));

            // Parse NDJSON: each line is a complete JSON object
            while let Some(end) = buf.find('\n') {
                let line = buf[..end].to_string();
                buf = buf[end + 1..].to_string();

                if line.is_empty() {
                    continue;
                }

                let report: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                if let Some(device) = report.as_object().and_then(|m| m.values().next())
                    && let Some(status) = device.pointer(&status_path).and_then(|s| s.as_str())
                    && status == expected_status
                {
                    return device.pointer(&release_path).unwrap().clone();
                }
            }
        }
        panic!("Report stream ended without matching report");
    })
    .await;

    match result {
        Ok(value) => value,
        Err(_) => panic!(
            "Timed out waiting for report with app '{app_uuid}' release '{release_uuid}' status '{expected_status}'"
        ),
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
