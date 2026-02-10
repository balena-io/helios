use mahler::exception;
use mahler::worker::Uninitialized;
use mahler::worker::{Ready, Worker};
use tokio::sync::RwLock;

use crate::oci::{Client as Docker, Error as DockerError, RegistryAuthClient};
use crate::util::store::Store;

use super::config::HostRuntimeDir;
use super::models::Device;
use super::tasks::{with_device_tasks, with_hostapp_tasks, with_image_tasks, with_userapp_tasks};

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("failed to connect to Docker daemon: {0}")]
    DockerConnection(#[from] DockerError),

    #[error("failed to initialize worker: {0}")]
    WorkerInit(#[from] mahler::error::Error),
}

/// Configure the worker jobs
///
/// This is mostly used for tests
fn worker() -> Worker<Device, Uninitialized> {
    let mut worker = Worker::new();

    worker = with_device_tasks(worker);
    worker = with_image_tasks(worker);

    if cfg!(feature = "balenahup") {
        worker = with_hostapp_tasks(worker);
    } else {
        // ignore hostapps in this case
        worker = worker.exception("/host/releases", exception::update(|| true))
    }

    if cfg!(feature = "userapps") {
        worker = with_userapp_tasks(worker);
    } else {
        // ignore user apps when planning
        worker = worker.exception("/apps", exception::update(|| true));
    }

    worker
}

type LocalWorker = Worker<Device, Ready>;

/// Create and initialize the worker
pub fn create(
    docker: Docker,
    local_store: Store,
    host_runtime_dir: HostRuntimeDir,
    registry_auth_client: Option<RegistryAuthClient>,
    initial: Device,
) -> Result<LocalWorker, CreateError> {
    // Create the worker and set-up resources
    let mut worker = worker().resource(docker);

    if let Some(auth_client) = registry_auth_client {
        // Add a registry auth client behind a RwLock to allow
        // tasks to modify it
        worker.use_resource(RwLock::new(auth_client));
    }

    worker.use_resource(host_runtime_dir);
    worker.use_resource(local_store);

    Ok(worker.initial_state(initial)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::DeviceTarget;

    use mahler::dag::{Dag, par, seq};
    use mahler::worker::FindWorkflow;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tracing_subscriber::fmt::{self, format::FmtSpan};
    use tracing_subscriber::{EnvFilter, prelude::*};

    fn before() {
        // Initialize tracing subscriber with custom formatting
        tracing_subscriber::registry()
            .with(EnvFilter::from_default_env())
            .with(
                fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_span_events(FmtSpan::CLOSE)
                    .event_format(fmt::format().pretty().with_target(false)),
            )
            .try_init()
            .unwrap_or(());
    }

    #[test]
    fn it_finds_a_workflow_to_create_a_single_app() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "auths": [],
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 0,
                    "name": "my-app",
                }
            },
        }))
        .unwrap();

        let (_, workflow) = worker()
            .initial_state(initial_state)
            .find_workflow(target)
            .unwrap();

        let expected: Dag<&str> = seq!("initialize app with uuid 'my-app-uuid'", "clean-up");
        assert_eq!(workflow.unwrap().to_string(), expected.to_string());
    }

    #[test]
    fn it_finds_a_workflow_to_update_an_app_and_configs() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "name": "my-device-name",
            "uuid": "my-device-uuid",
            "auths": [],
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app-old-name",
                }
            },
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                }
            },
        }))
        .unwrap();

        let (_, workflow) = worker()
            .initial_state(initial_state)
            .find_workflow(target)
            .unwrap();
        let expected: Dag<&str> = par!(
            "update device name",
            "store name for app with uuid 'my-app-uuid'",
        ) + seq!("clean-up");
        assert_eq!(workflow.unwrap().to_string(), expected.to_string());
    }

    #[test]
    fn it_finds_a_workflow_to_change_an_app_name_and_id() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "name": "device-name",
            "auths": [],
            "apps": {
                "my-app-uuid": {
                    "id": 0,
                    "name": "my-app",
                }
            },
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "uuid": "my-device-uuid",
            "name": "device-name",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
                }
            },
        }))
        .unwrap();

        let (_, workflow) = worker()
            .initial_state(initial_state)
            .find_workflow(target)
            .unwrap();
        let expected: Dag<&str> = par!(
            "store id for app with uuid 'my-app-uuid'",
            "store name for app with uuid 'my-app-uuid'"
        ) + seq!("clean-up");
        assert_eq!(workflow.unwrap().to_string(), expected.to_string());
    }

    #[test]
    fn it_finds_a_workflow_to_fetch_and_install_services() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "auths": [],
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
                }
            },
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
                    "releases": {
                        "my-release-uuid": {
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "started": true,
                                    "config": {},
                                },
                                "service2": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "started": true,
                                    "config": {},
                                },
                                "service3": {
                                    "id": 3,
                                    // different image same digest
                                    "image": "registry2.balena-cloud.com/v2/deafc41f@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "started": true,
                                    "config": {},
                                },
                                // additional images to test download batching
                                "service4": {
                                    "id": 4,
                                    "image": "alpine:latest",
                                    "started": true,
                                    "config": {},
                                },
                                "service5": {
                                    "id": 5,
                                    "image": "alpine:3.20",
                                    "started": true,
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
        }))
        .unwrap();

        let (_, workflow) = worker()
            .initial_state(initial_state)
            .find_workflow(target)
            .unwrap();
        let expected: Dag<&str> = seq!("request registry credentials")
            + seq!("initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'")
            + par!(
                "initialize service 'service1' for release 'my-release-uuid'",
                "initialize service 'service2' for release 'my-release-uuid'",
                "initialize service 'service3' for release 'my-release-uuid'",
                "initialize service 'service4' for release 'my-release-uuid'",
                "initialize service 'service5' for release 'my-release-uuid'",
            )
            + par!(
                "pull image 'ubuntu:latest'",
                "pull image 'registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080'",
                "pull image 'alpine:latest'",
            )
            + par!(
                "tag image 'registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080' \
                        as 'registry2.balena-cloud.com/v2/deafc41f@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080'",
                "pull image 'alpine:3.20'",
            )
            + par!(
                "install service 'service1' for release 'my-release-uuid'",
                "install service 'service2' for release 'my-release-uuid'",
                "install service 'service3' for release 'my-release-uuid'",
                "install service 'service4' for release 'my-release-uuid'",
                "install service 'service5' for release 'my-release-uuid'",
            )
            + par!(
                "start service 'service1' for release 'my-release-uuid'",
                "start service 'service2' for release 'my-release-uuid'",
                "start service 'service3' for release 'my-release-uuid'",
                "start service 'service4' for release 'my-release-uuid'",
                "start service 'service5' for release 'my-release-uuid'",
            )
            + seq!("clean-up");

        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_to_update_services_image_metadata() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "auths": [],
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "my-release-uuid": {
                            "services": {
                                "one": {
                                    "id": 1,
                                    "image": "sha256:deadbeef",
                                    "started": true,
                                    "config": {},
                                },
                                "two": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:b111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "my-release-uuid": {
                            "services": {
                                "one": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/deafc41f@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "config": {},
                                },
                                "two": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:b111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
        }))
        .unwrap();

        let (_, workflow) = worker()
            .initial_state(initial_state)
            .find_workflow(target)
            .unwrap();
        let expected: Dag<&str> = seq!(
            "store image metadata for service 'one' of release 'my-release-uuid'",
            "clean-up"
        );

        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    // The worker doesn't have any tasks to update services or delete releases
    // so this plan should fail
    #[test]
    fn it_fails_to_find_a_workflow_for_updating_services() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "auths": [],
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
                    "releases": {
                        "old-release": {
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc1@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "config": {},
                                },
                                "service2":  {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc2@sha256:a222222222222222222222222222222222222222222222222222222222222222",
                                    "started": true,
                                    "config": {},
                                },

                            }
                        }
                    }
                }
            },
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
                    "releases": {
                        "new-release": {
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/newsvc1@sha256:b111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "config": {},
                                },
                                "service2":  {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/newsvc2@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                    "started": true,
                                    "config": {},
                                },

                            }
                        }
                    }
                }
            },
        }))
        .unwrap();

        // this should return Err(NotFound) and not panic
        let (_, workflow) = worker()
            .initial_state(initial_state)
            .find_workflow(target)
            .unwrap();

        assert!(
            workflow.is_none(),
            "unexpected plan:\n{}",
            workflow.unwrap()
        );
    }

    #[test]
    fn it_finds_a_workflow_to_update_the_hostapp_on_a_fresh_device() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "auths": [],
            "host": {
                "meta": {
                    "name": "balenaOS",
                    "version": "5.7.3",
                    "build": "abcd1234",
                },
            },
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "releases": {
                    "target-release": {
                        "app": "hostapp-uuid",
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "cde2354",
                        "status": "Running",
                    }
                }
            },
        }))
        .unwrap();

        let (_, workflow) = worker()
            .initial_state(initial_state)
            .find_workflow(target)
            .unwrap();
        let expected: Dag<&str> = seq!(
            "initialize hostOS release 'target-release'",
            "install hostOS release 'target-release'",
            "complete hostOS release install for 'target-release'",
            "clean-up"
        );
        assert_eq!(workflow.unwrap().to_string(), expected.to_string());
    }

    #[test]
    fn it_finds_a_workflow_to_update_the_hostapp() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "auths": [],
            "host": {
                "meta": {
                    "name": "balenaOS",
                    "version": "5.7.3",
                    "build": "abcd1234",
                },
                "releases": {
                    "old-release": {
                        "app": "hostapp-uuid",
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "abcd1234",
                        "status": "Running",
                        "install_attempts": 1,
                    }
                }
            },
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "releases": {
                    "new-release": {
                        "app": "hostapp-uuid",
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "cde2354",
                        "status": "Running"
                    }
                }
            },
        }))
        .unwrap();

        let (_, workflow) = worker()
            .initial_state(initial_state)
            .find_workflow(target)
            .unwrap();
        let expected: Dag<&str> = seq!("initialize hostOS release 'new-release'",)
            + par!(
                "install hostOS release 'new-release'",
                "clean up metadata for previous hostOS release 'old-release'",
            )
            + seq!(
                "complete hostOS release install for 'new-release'",
                "clean-up"
            );
        assert_eq!(workflow.unwrap().to_string(), expected.to_string());
    }

    #[test]
    fn it_ignores_a_target_that_deletes_the_hostapp() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "auths": [],
            "host": {
                "meta": {
                    "name": "balenaOS",
                    "version": "5.7.3",
                    "build": "abcd1234",
                },
                "releases": {
                    "old-release": {
                        "app": "hostapp-uuid",
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "abcd1234",
                        "status": "Running",
                        "install_attempts": 1,
                    }
                }
            },
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "name": "new-device-name",
            "uuid": "my-device-uuid",
        }))
        .unwrap();

        let (_, workflow) = worker()
            .initial_state(initial_state)
            .find_workflow(target)
            .unwrap();
        let expected: Dag<&str> = seq!("update device name", "clean-up");
        assert_eq!(workflow.unwrap().to_string(), expected.to_string());
    }
}
