use helios_state::models::{Device, ImageRef};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use tokio::sync::watch::Receiver;
use tracing::{error, info, instrument, trace};

use crate::state::{LocalState, models::ServiceStatus as LocalServiceStatus};
use crate::util::http::Uri;
use crate::util::interrupt::Interrupt;
use crate::util::request::{Patch, PatchError, RequestConfig};
use crate::util::types::Uuid;

use super::config::RemoteConfig;

#[derive(Serialize, Debug, PartialEq, Eq)]
enum ServiceStatus {
    Downloading,
    Installing,
    Installed,
    Running,
    // Stopped,
}

#[derive(Clone, Serialize, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum UpdateStatus {
    Done,
    // Aborted,
    #[serde(rename = "applying changes")]
    ApplyingChanges,
}

#[derive(Serialize, Debug)]
struct ServiceReport {
    /// The service ImageUri without the sha
    image: String,
    /// The service runtime status
    status: ServiceStatus,
    // Service image pull progress
    #[serde(skip_serializing_if = "Option::is_none")]
    download_progress: Option<u8>,
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
            device:
                Device {
                    host,
                    apps: userapps,
                    images,
                    ..
                },
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
                                download_progress: None,
                            },
                        )]
                        .into(),
                    },
                );
            }
        }

        // convert the user apps to an accepted report
        for (app_uuid, app) in userapps {
            for (rel_uuid, rel) in app.releases {
                for (svc_name, svc) in rel.services {
                    let svc_img = if let ImageRef::Uri(img) = svc.image {
                        img
                    } else {
                        // skip services that don't have an image uri defined
                        continue;
                    };
                    // Get the status of the app as services based on
                    // whether the running release is the current release
                    let (update_status, service_status, download_progress) = match svc.status {
                        LocalServiceStatus::Installing => {
                            let maybe_img = images.get(&svc_img);
                            if let Some(img) = maybe_img
                                && img.download_progress < 100
                            {
                                (
                                    UpdateStatus::ApplyingChanges,
                                    ServiceStatus::Downloading,
                                    Some(img.download_progress),
                                )
                            } else {
                                (
                                    UpdateStatus::ApplyingChanges,
                                    ServiceStatus::Installing,
                                    None,
                                )
                            }
                        }
                        LocalServiceStatus::Installed => {
                            (UpdateStatus::Done, ServiceStatus::Installed, None)
                        }
                    };

                    // Get or create the app
                    let app = apps.entry(app_uuid.clone()).or_insert(AppReport {
                        // FIXME: the current release should come from the worker once
                        // the release has successfully installed
                        release_uuid: None,
                        releases: HashMap::new(),
                    });

                    let release = app
                        .releases
                        .entry(rel_uuid.clone())
                        .or_insert(ReleaseReport {
                            update_status: update_status.clone(),
                            services: HashMap::new(),
                        });

                    if release.update_status < update_status {
                        release.update_status = update_status;
                    }

                    release.services.insert(
                        svc_name,
                        ServiceReport {
                            image: svc_img.repo(),
                            status: service_status,
                            download_progress,
                        },
                    );
                }
            }
        }

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

    // FIXME: this may conflict with service installs coming from the supervisor. Needs testing
    // Once we implement more complete service management, reporting can be done by this service
    // and we will block report calls from the legacy supervisor to the backend
    let new_report: Value = calculate_report_diff(device_uuid, &last_report, report.into());

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
    let mut state_changed = false;
    'report: loop {
        // loop again if the state changed while waiting for the previous patch to complete
        if !state_changed && state_rx.changed().await.is_err() {
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

        // Wait for the report to be sent. Only interrupt the patch if the channel closes,
        // which indicates an error condition. State changes during the request should not
        // interrupt it - we'll report the new state after this request completes.
        last_report = loop {
            tokio::select! {
                res = &mut report_future => break res,
                Err(_) = state_rx.changed() => {
                    // Channel closed - interrupt the request and exit
                    interrupt.trigger();
                    report_future.await;
                    break 'report;
                }
                else => {
                    // mark the state changed and keep waiting
                    state_changed = true
                },
            }
        };
    }
    trace!("state channel closed");
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::state::models::Device;

    use super::*;

    #[test]
    fn it_creates_a_device_report_from_a_device() {
        let device = serde_json::from_value::<Device>(json!({
            "uuid": "device-123",
            "auths": [],
            "images": {
                "ubuntu": {
                    "engine_id": "sha256:abc",
                    "download_progress": 100,
                },
                "alpine": {
                    "download_progress": 50,
                },
                "fedora": {
                    "engine_id": "sha256:def",
                    "download_progress": 100,
                }
            },
            "apps": {
                "my-app": {
                    "id": 1,
                    "releases": {
                        "new-release": {
                            "services": {
                                "one": {
                                    "id": 1,
                                    "image": "ubuntu",
                                    "status": "Installed",
                                    "config": {}
                                },
                                "two": {
                                    "id": 2,
                                    "image": "alpine",
                                    "status": "Installing",
                                    "config": {}
                                },
                                "three": {
                                    "id": 3,
                                    "image": "fedora",
                                    "status": "Installing",
                                    "config": {},
                                }
                            }
                        }
                    }
                }
            },
            "needs_cleanup": true,
        }))
        .unwrap();

        let report = Report::from(LocalState {
            status: crate::state::UpdateStatus::ApplyingChanges,
            device,
        });

        assert_eq!(
            report
                .0
                .get("device-123")
                .and_then(|device| device.apps.as_ref())
                .and_then(|apps| apps.get(&Uuid::from("my-app")))
                .and_then(|app| app.releases.get(&Uuid::from("new-release")))
                .map(|rel| &rel.update_status),
            Some(&UpdateStatus::ApplyingChanges),
        );
        assert_eq!(
            report
                .0
                .get("device-123")
                .and_then(|device| device.apps.as_ref())
                .and_then(|apps| apps.get(&Uuid::from("my-app")))
                .and_then(|app| app.releases.get(&Uuid::from("new-release")))
                .and_then(|rel| rel.services.get("two"))
                .map(|svc| (&svc.status, svc.download_progress)),
            Some((&ServiceStatus::Downloading, Some(50))),
        );
        assert_eq!(
            report
                .0
                .get("device-123")
                .and_then(|device| device.apps.as_ref())
                .and_then(|apps| apps.get(&Uuid::from("my-app")))
                .and_then(|app| app.releases.get(&Uuid::from("new-release")))
                .and_then(|rel| rel.services.get("three"))
                .map(|svc| (&svc.status, svc.download_progress)),
            Some((&ServiceStatus::Installing, None)),
        );
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
