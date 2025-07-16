use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{field, info_span, instrument, warn, Span};

mod request;

use crate::config::Config;
use crate::util::uri::make_uri;
use request::{Get, Patch, RequestConfig};

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
pub type PollResult = Option<Value>;

#[instrument(skip_all, fields(success_rate = field::Empty))]
pub async fn poll_remote(client: &mut Get) -> PollResult {
    // Only enter the poll span if not unmanaged
    let span = info_span!("poll_remote", success_rate = field::Empty);
    let _ = span.enter();

    let result = client.get().await;

    // Reset the poll timer after we get the response
    let res = match result {
        Ok(response) if response.modified => response.value,
        Ok(_) => None,
        Err(e) => {
            warn!("poll failed: {e}");
            None
        }
    };

    let metrics = client.metrics();
    span.record("success_rate", metrics.success_rate());

    res
}

pub fn next_poll(config: &Config) -> Duration {
    let max_jitter = &config.remote.max_poll_jitter;
    let jitter_ms = rand::random_range(0..=max_jitter.as_millis() as u64);
    let jitter = Duration::from_millis(jitter_ms);
    config.remote.poll_interval + jitter
}

pub async fn poll_remote_if_managed(poll_client: &mut Option<Get>) -> PollResult {
    // poll if we have a client
    if let Some(ref mut client) = poll_client {
        poll_remote(client).await
    } else {
        None
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

impl From<Report> for Value {
    fn from(value: Report) -> Self {
        serde_json::to_value(value)
            // This is probably a bug in the types, it shouldn't really happen
            .expect("state report serialization failed")
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
async fn send_report(client: &mut Patch, report: Report, last_report: LastReport) -> LastReport {
    // TODO: calculate differences with the last report and just send that
    let res = match client.patch(report.clone().into()).await {
        Ok(_) => Some(report.into()),
        Err(e) => {
            warn!("patch failed: {e}");
            last_report
        }
    };

    let metrics = client.metrics();
    Span::current().record("success_rate", metrics.success_rate());

    res
}

pub async fn send_report_if_managed(
    report_client: &mut Option<Patch>,
    report: Report,
    last_report: LastReport,
) -> LastReport {
    if let Some(client) = report_client {
        send_report(client, report, last_report).await
    } else {
        last_report
    }
}
