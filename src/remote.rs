use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::Instant;
use tracing::{field, instrument, warn, Span};

use crate::config::Config;
use crate::request::{make_uri, Get, Patch, RequestConfig};

pub fn get_poll_client(config: &Config) -> Option<Get> {
    if let Some(uri) = config.remote.api_endpoint.clone() {
        let endpoint = make_uri(
            uri,
            format!("/device/v3/{}/state", config.uuid).as_str(),
            None,
        )
        .expect("remote API endpoint must be a valid URI")
        .to_string();

        let client_config = RequestConfig {
            timeout: config.remote.request_timeout,
            min_interval: config.remote.min_interval,
            max_backoff: config.remote.poll_interval,
            api_token: config.remote.api_key.clone(),
        };

        let client = Get::new(endpoint, client_config);

        Some(client)
    } else {
        None
    }
}

// Return type from poll_remote
pub type PollResult = (Option<Value>, Instant);

#[instrument(skip_all, fields(success_rate=field::Empty))]
pub async fn poll_remote(poll_client: &mut Option<Get>, config: &Config) -> PollResult {
    let max_jitter = &config.remote.max_poll_jitter;
    let jitter_ms = rand::random_range(0..=max_jitter.as_millis() as u64);
    let jitter = Duration::from_millis(jitter_ms);
    let mut next_poll_time = Instant::now() + config.remote.poll_interval + jitter;

    // poll if we have a client
    if let Some(ref mut client) = poll_client {
        let result = client.get().await;

        // Reset the poll timer after we get the response
        next_poll_time = Instant::now() + config.remote.poll_interval + jitter;
        let res = match result {
            Ok(response) if response.modified => response.value,
            Ok(_) => None,
            Err(e) => {
                warn!("poll failed: {e}");
                None
            }
        };

        let metrics = client.metrics();
        Span::current().record("success_rate", metrics.success_rate());

        (res, next_poll_time)
    } else {
        (None, next_poll_time)
    }
}

#[derive(Serialize, Debug, Clone)]
pub struct DeviceReport {}

// The state for report
#[derive(Serialize, Debug, Clone)]
pub struct Report(HashMap<String, DeviceReport>);

impl Report {
    pub fn new(device: HashMap<String, DeviceReport>) -> Self {
        Report(device)
    }
}

// Return type from send_report
pub type LastReport = Option<Value>;

pub fn get_report_client(config: &Config) -> Option<Patch> {
    if let Some(uri) = config.remote.api_endpoint.clone() {
        let endpoint = make_uri(uri, "/device/v3/state", None)
            .expect("remote API endpoint must be a valid URI")
            .to_string();

        let client_config = RequestConfig {
            timeout: config.remote.request_timeout,
            min_interval: config.remote.min_interval,
            max_backoff: config.remote.poll_interval,
            api_token: config.remote.api_key.clone(),
        };

        let client = Patch::new(endpoint, client_config);

        Some(client)
    } else {
        None
    }
}

#[instrument(skip_all, fields(success_rate=field::Empty))]
pub async fn send_report(
    report_client: &mut Option<Patch>,
    current_state: Value,
    last_report: LastReport,
) -> LastReport {
    if let Some(client) = report_client {
        // TODO: calculate differences with the last report and just send that
        let res = match client.patch(current_state.clone()).await {
            Ok(_) => Some(current_state),
            Err(e) => {
                warn!("patch failed: {e}");
                last_report
            }
        };

        let metrics = client.metrics();
        Span::current().record("success_rate", metrics.success_rate());

        res
    } else {
        last_report
    }
}
