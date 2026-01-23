use helios_state::models::Device;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::sync::watch::Receiver;
use tracing::{error, info, instrument, trace};

use crate::state::{LocalState, UpdateStatus};
use crate::util::http::Uri;
use crate::util::interrupt::Interrupt;
use crate::util::request::{Patch, PatchError, RequestConfig};
use crate::util::types::Uuid;

use super::config::RemoteConfig;

#[derive(Serialize, Debug)]
enum ServiceStatus {
    // Downloading,
    // Downloaded,
    Installing,
    // Installed,
    Running,
    // Stopped,
}

#[derive(Serialize, Debug)]
struct ServiceReport {
    /// The service ImageUri without the sha
    image: String,
    /// The service runtime status
    status: ServiceStatus,
    // TODO: add download_progress
}

#[derive(Serialize, Debug)]
struct ReleaseReport {
    update_status: UpdateStatus,
    services: HashMap<String, ServiceReport>,
}

#[derive(Serialize, Debug)]
struct AppReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    release_uuid: Option<Uuid>,
    releases: HashMap<Uuid, ReleaseReport>,
}

#[derive(Serialize, Debug)]
struct DeviceReport {
    // the apps object should not be present if the
    // previous report apps match the current report
    #[serde(skip_serializing_if = "Option::is_none")]
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
    fn from(state: LocalState) -> Self {
        let mut apps = HashMap::new();

        let LocalState {
            device: Device { host, .. },
            ..
        } = state;

        // Convert the host data into an app report if any
        if let Some(host_state) = host {
            // Get the current release
            let release_uuid = host_state
                .releases
                .iter()
                .find(|(_, rel)| host_state.meta.build.as_ref() == Some(&rel.build))
                .map(|(uuid, _)| uuid.clone());

            for (rel_uuid, release) in host_state.releases {
                // Get the status of the app as services based on
                // whether the running release is the current release
                let (update_status, service_status, app_release) =
                    if Some(&rel_uuid) == release_uuid.as_ref() {
                        (
                            UpdateStatus::Done,
                            ServiceStatus::Running,
                            Some(rel_uuid.clone()),
                        )
                    } else {
                        (
                            UpdateStatus::ApplyingChanges,
                            ServiceStatus::Installing,
                            None,
                        )
                    };

                // Get the app that corresponds to the current release
                // or create one
                let app = apps.entry(release.app).or_insert(AppReport {
                    release_uuid: app_release,
                    releases: HashMap::new(),
                });

                // Update the releases
                app.releases.insert(
                    rel_uuid,
                    ReleaseReport {
                        update_status,
                        services: [(
                            // the backend doesn't really care for the service name, it creates
                            // image installs using the image uri
                            "hostapp".to_owned(),
                            ServiceReport {
                                image: release.image.repo(),
                                status: service_status,
                            },
                        )]
                        .into(),
                    },
                );
            }
        }

        // TODO: convert the rest of the apps to an accepted report

        DeviceReport { apps: Some(apps) }
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
        auth_token: Some(config.api_key.to_string()),
    };

    Patch::new(endpoint, client_config)
}

// Return type from send_report
type LastReport = Option<Value>;

#[allow(dead_code)]
fn calculate_report_diff(
    device_uuid: String,
    last_report: &Option<Value>,
    new_report: Value,
) -> Value {
    let mut new_report = new_report;

    // calculate differences with the last report
    if let Some(old_device) = last_report
        .as_ref()
        .and_then(|old_report| old_report.get(&device_uuid))
        && let Some(device) = new_report
            .get_mut(&device_uuid)
            .and_then(|d| d.as_object_mut())
        && let Some(apps) = device.get_mut("apps").and_then(|a| a.as_object_mut())
    {
        // Track which apps to remove
        let apps_to_remove: Vec<String> = apps
            .iter()
            .filter(|(app_uuid, app)| {
                old_device
                    .get("apps")
                    .and_then(|old_apps| old_apps.get(*app_uuid))
                    == Some(*app)
            })
            .map(|(uuid, _)| uuid.clone())
            .collect();

        // Remove unchanged apps
        for app_uuid in apps_to_remove {
            apps.remove(&app_uuid);
        }

        // Remove apps field if empty
        if apps.is_empty() {
            device.remove("apps");
        }
    }

    new_report
}

async fn send_report(
    client: &mut Patch,
    report: Report,
    last_report: LastReport,
    interrupt: Interrupt,
) -> LastReport {
    let device_uuid = report
        .0
        .keys()
        .last()
        .expect("report cannot be empty")
        .clone();

    // FIXME: this is disabled because reporting here and in the legacy supervisor causes
    // conflicts with service installs on the backend.
    // Once we implement user app reporting we can report all apps from helios
    // and block report calls from the legacy supervisor to the backend
    // let new_report: Value = calculate_report_diff(device_uuid, &last_report, report.into());
    let new_report = json!({ device_uuid: {} });

    match client.patch(new_report.clone(), Some(interrupt)).await {
        Ok(_) => Some(new_report),
        Err(PatchError::Cancelled) => last_report,
        Err(e) => {
            error!("report failed: {e}");
            last_report
        }
    }
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

    use crate::state::models::Device;

    use super::*;

    #[test]
    fn it_creates_a_device_report_from_a_device() {
        let device = Device::new("test-uuid".into(), "balenaOS 5.3.1".parse().ok());

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

    #[test]
    fn calculate_report_diff_returns_full_report_when_no_last_report() {
        let device_uuid = "device-123".to_string();
        let new_report = json!({
            "device-123": {
                "apps": {
                    "app-456": {
                        "release_uuid": "release-789",
                        "releases": {
                            "release-789": {
                                "update_status": "Done",
                                "services": {
                                    "hostapp": {
                                        "image": "example/hostapp",
                                        "status": "Running"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        let result = calculate_report_diff(device_uuid, &None, new_report.clone());

        assert_eq!(result, new_report);
    }

    #[test]
    fn calculate_report_diff_removes_unchanged_apps() {
        let device_uuid = "device-123".to_string();
        let app_data = json!({
            "release_uuid": "release-789",
            "releases": {
                "release-789": {
                    "update_status": "Done",
                    "services": {
                        "hostapp": {
                            "image": "example/hostapp",
                            "status": "Running"
                        }
                    }
                }
            }
        });

        let last_report = Some(json!({
            "device-123": {
                "apps": {
                    "app-456": app_data.clone()
                }
            }
        }));

        let new_report = json!({
            "device-123": {
                "apps": {
                    "app-456": app_data
                }
            }
        });

        let result = calculate_report_diff(device_uuid, &last_report, new_report);

        assert_eq!(
            result,
            json!({
                "device-123": {}
            })
        );
    }

    #[test]
    fn calculate_report_diff_keeps_changed_apps() {
        let device_uuid = "device-123".to_string();
        let last_report = Some(json!({
            "device-123": {
                "apps": {
                    "app-456": {
                        "release_uuid": "release-789",
                        "releases": {
                            "release-789": {
                                "update_status": "Done",
                                "services": {
                                    "hostapp": {
                                        "image": "example/hostapp",
                                        "status": "Running"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }));

        let new_report = json!({
            "device-123": {
                "apps": {
                    "app-456": {
                        "release_uuid": "release-999",
                        "releases": {
                            "release-999": {
                                "update_status": "ApplyingChanges",
                                "services": {
                                    "hostapp": {
                                        "image": "example/hostapp-new",
                                        "status": "Installing"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        let result = calculate_report_diff(device_uuid, &last_report, new_report.clone());

        assert_eq!(result, new_report);
    }

    #[test]
    fn calculate_report_diff_handles_multiple_apps_with_partial_changes() {
        let device_uuid = "device-123".to_string();
        let unchanged_app = json!({
            "release_uuid": "release-789",
            "releases": {
                "release-789": {
                    "update_status": "Done",
                    "services": {
                        "hostapp": {
                            "image": "example/hostapp",
                            "status": "Running"
                        }
                    }
                }
            }
        });

        let last_report = Some(json!({
            "device-123": {
                "apps": {
                    "app-unchanged": unchanged_app.clone(),
                    "app-changed": {
                        "release_uuid": "release-old",
                        "releases": {}
                    }
                }
            }
        }));

        let new_report = json!({
            "device-123": {
                "apps": {
                    "app-unchanged": unchanged_app,
                    "app-changed": {
                        "release_uuid": "release-new",
                        "releases": {}
                    }
                }
            }
        });

        let result = calculate_report_diff(device_uuid, &last_report, new_report);

        assert_eq!(
            result,
            json!({
                "device-123": {
                    "apps": {
                        "app-changed": {
                            "release_uuid": "release-new",
                            "releases": {}
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn calculate_report_diff_handles_new_apps() {
        let device_uuid = "device-123".to_string();
        let last_report = Some(json!({
            "device-123": {
                "apps": {
                    "app-existing": {
                        "release_uuid": "release-789",
                        "releases": {}
                    }
                }
            }
        }));

        let new_report = json!({
            "device-123": {
                "apps": {
                    "app-existing": {
                        "release_uuid": "release-789",
                        "releases": {}
                    },
                    "app-new": {
                        "release_uuid": "release-999",
                        "releases": {}
                    }
                }
            }
        });

        let result = calculate_report_diff(device_uuid, &last_report, new_report);

        assert_eq!(
            result,
            json!({
                "device-123": {
                    "apps": {
                        "app-new": {
                            "release_uuid": "release-999",
                            "releases": {}
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn calculate_report_diff_handles_missing_apps_field_in_last_report() {
        let device_uuid = "device-123".to_string();
        let last_report = Some(json!({
            "device-123": {}
        }));

        let new_report = json!({
            "device-123": {
                "apps": {
                    "app-456": {
                        "release_uuid": "release-789",
                        "releases": {}
                    }
                }
            }
        });

        let result = calculate_report_diff(device_uuid, &last_report, new_report.clone());

        assert_eq!(result, new_report);
    }

    #[test]
    fn calculate_report_diff_handles_empty_apps_object() {
        let device_uuid = "device-123".to_string();
        let last_report = Some(json!({
            "device-123": {
                "apps": {}
            }
        }));

        let new_report = json!({
            "device-123": {
                "apps": {}
            }
        });

        let result = calculate_report_diff(device_uuid, &last_report, new_report);

        assert_eq!(
            result,
            json!({
                "device-123": {}
            })
        );
    }
}
