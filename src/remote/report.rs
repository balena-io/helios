use mahler::workflow::Interrupt;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use tokio::sync::watch::Receiver;
use tracing::{error, info, instrument, trace};

use crate::state::LocalState;
use crate::types::Uuid;
use crate::util::http::Uri;

use super::config::RemoteConfig;
use super::request::{Patch, PatchError, RequestConfig};

#[derive(Serialize, Debug)]
struct AppReport {
    release_uuid: Option<Uuid>,
}

#[derive(Serialize, Debug)]
struct DeviceReport {
    // the apps object should not be present if the
    // previous report apps match the current report
    apps: Option<HashMap<Uuid, AppReport>>,
}

// The state for report
#[derive(Serialize, Debug)]
struct Report(HashMap<String, DeviceReport>);

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

impl From<LocalState> for DeviceReport {
    fn from(local_state: LocalState) -> Self {
        let LocalState { device, .. } = local_state;
        DeviceReport {
            apps: Some(
                device
                    .apps
                    .into_keys()
                    .map(|uuid| (uuid, AppReport { release_uuid: None }))
                    .collect(),
            ),
        }
    }
}

impl From<LocalState> for Report {
    fn from(state: LocalState) -> Self {
        Report::new(HashMap::from([(
            state.device.uuid.clone().into(),
            state.into(),
        )]))
    }
}

fn get_report_client(config: &RemoteConfig) -> Patch {
    let uri = config.api_endpoint.clone();
    let endpoint = Uri::from_parts(uri, "/device/v3/state", None)
        .expect("remote API endpoint must be a valid URI")
        .to_string();

    let client_config = RequestConfig {
        timeout: config.request.timeout,
        min_interval: config.request.poll_min_interval,
        max_backoff: config.request.poll_interval,
        api_token: Some(config.api_key.to_string()),
    };

    Patch::new(endpoint, client_config)
}

// Return type from send_report
type LastReport = Option<Value>;

async fn send_report(
    client: &mut Patch,
    report: Report,
    last_report: LastReport,
    interrupt: Interrupt,
) -> LastReport {
    // TODO: calculate differences with the last report and just send that
    let new_report: Value = report.into();
    let res = match client.patch(new_report.clone(), Some(interrupt)).await {
        Ok(_) => Some(new_report),
        Err(PatchError::Cancelled) => last_report,
        Err(e) => {
            error!("report failed: {e}");
            last_report
        }
    };

    res
}

#[instrument(name = "report", skip_all)]
pub async fn start_report(config: RemoteConfig, mut state_rx: Receiver<LocalState>) {
    let mut report_client = get_report_client(&config);

    info!("waiting for state changes");
    let mut last_report = LastReport::None;
    loop {
        let state_changed = state_rx.changed().await;
        if state_changed.is_err() {
            // Not really an error, it just means the API closed
            trace!("state channel closed");
            break;
        }

        let report = state_rx.borrow_and_update().clone().into();

        let interrupt = Interrupt::new();
        let report_future = send_report(
            &mut report_client,
            report,
            last_report.clone(),
            interrupt.clone(),
        );
        tokio::pin!(report_future);

        // Wait for the report to be sent or cancel the patch if the state changes
        // before the patch is completed
        last_report = tokio::select! {
            res = &mut report_future => res,
            _ = state_rx.changed() => {
                // Interrupt the future
                interrupt.trigger();

                // Wait for the future to complete
                report_future.await;

                // Reuse the last report since the request was interrupted
                last_report
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::state::models::{Device, Host};

    use super::*;

    #[test]
    fn it_creates_a_device_report_from_a_device() {
        let device = Device::new(
            "test-uuid".into(),
            Some("generic-aarch64".into()),
            Host {
                os: "balenaOS 5.3.1".into(),
                arch: "aarch64".into(),
            },
        );

        let report = Report::from(LocalState {
            status: crate::state::UpdateStatus::Done,
            device,
        });

        let value: Value = report.into();
        assert_eq!(
            value,
            json!({"test-uuid": {
                "apps": {}
            }})
        )
    }
}
