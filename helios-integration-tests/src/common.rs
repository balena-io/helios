use std::collections::HashMap;

use bollard::{Docker, query_parameters::PruneImagesOptions};
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
