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
    }

    if cfg!(feature = "userapps") {
        worker = with_userapp_tasks(worker);
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
    let mut worker = worker().resource(docker).initial_state(initial)?;

    if let Some(auth_client) = registry_auth_client {
        // Add a registry auth client behind a RwLock to allow
        // tasks to modify it
        worker.use_resource(RwLock::new(auth_client));
    }

    worker.use_resource(host_runtime_dir);
    worker.use_resource(local_store);

    Ok(worker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::DeviceTarget;

    use mahler::dag::{Dag, par, seq};
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

    #[tokio::test]
    async fn it_finds_a_workflow_to_create_a_single_app() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "auths": [],
            "needs_cleanup": false,
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
            "needs_cleanup": false
        }))
        .unwrap();

        let workflow = worker().find_workflow(initial_state, target).unwrap();
        let expected: Dag<&str> = seq!(
            "ensure clean-up",
            "initialize app with uuid 'my-app-uuid'",
            "perform clean-up"
        );
        assert_eq!(workflow.to_string(), expected.to_string());
    }

    #[tokio::test]
    async fn it_finds_a_workflow_to_update_an_app_and_configs() {
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
            "needs_cleanup": false,
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
            "needs_cleanup": false,
        }))
        .unwrap();

        let workflow = worker().find_workflow(initial_state, target).unwrap();
        let expected: Dag<&str> = seq!("ensure clean-up")
            + par!(
                "update device name",
                "update name for app with uuid 'my-app-uuid'",
            )
            + seq!("perform clean-up");
        assert_eq!(workflow.to_string(), expected.to_string());
    }

    #[tokio::test]
    async fn it_finds_a_workflow_to_change_an_app_name() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "name": "device-name",
            "auths": [],
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                }
            },
            "needs_cleanup": false,
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
            "needs_cleanup": false,
        }))
        .unwrap();

        let workflow = worker().find_workflow(initial_state, target).unwrap();
        let expected: Dag<&str> = seq!(
            "ensure clean-up",
            "update name for app with uuid 'my-app-uuid'",
            "perform clean-up"
        );
        assert_eq!(workflow.to_string(), expected.to_string());
    }

    #[tokio::test]
    async fn it_finds_a_workflow_to_fetch_and_install_services() {
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
            "needs_cleanup": false,
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
                                    "config": {},
                                },
                                "service2": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "config": {},
                                },
                                "service3": {
                                    "id": 3,
                                    // different image same digest
                                    "image": "registry2.balena-cloud.com/v2/deafc41f@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "config": {},
                                },
                                // additional images to test download batching
                                "service4": {
                                    "id": 4,
                                    "image": "alpine:latest",
                                    "config": {},
                                },
                                "service5": {
                                    "id": 5,
                                    "image": "alpine:3.20",
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
            "needs_cleanup": false,
        }))
        .unwrap();

        let workflow = worker().find_workflow(initial_state, target).unwrap();
        let expected: Dag<&str> = seq!("ensure clean-up", "request registry credentials")
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
            + seq!(
                "initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'",
                "initialize service 'service1' for release 'my-release-uuid'",
                "initialize service 'service2' for release 'my-release-uuid'",
                "initialize service 'service3' for release 'my-release-uuid'",
                "initialize service 'service4' for release 'my-release-uuid'",
                "initialize service 'service5' for release 'my-release-uuid'",
                "perform clean-up"
            );

        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[tokio::test]
    async fn it_finds_a_workflow_to_update_services_image_metadata() {
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
                                    "config": {},
                                },
                                "two": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:b111111111111111111111111111111111111111111111111111111111111111",
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
            "needs_cleanup": false,
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
                                    "config": {},
                                },
                                "two": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:b111111111111111111111111111111111111111111111111111111111111111",
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
            "needs_cleanup": false,
        }))
        .unwrap();

        let workflow = worker().find_workflow(initial_state, target).unwrap();
        let expected: Dag<&str> = seq!(
            "ensure clean-up",
            "update image metadata for service 'one' of release 'my-release-uuid'",
            "perform clean-up"
        );

        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    // The worker doesn't have any tasks to update services or delete releases
    // so this plan should fail
    #[tokio::test]
    async fn it_fails_to_find_a_workflow_for_updating_services() {
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
                                    "config": {},
                                },
                                "service2":  {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc2@sha256:a222222222222222222222222222222222222222222222222222222222222222",
                                    "config": {},
                                },

                            }
                        }
                    }
                }
            },
            "needs_cleanup": false
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
                                    "config": {},
                                },
                                "service2":  {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/newsvc2@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                    "config": {},
                                },

                            }
                        }
                    }
                }
            },
            "needs_cleanup": false
        }))
        .unwrap();

        // this should return Err(NotFound) and not panic
        let workflow = worker().find_workflow(initial_state, target);
        assert!(workflow.is_err(), "unexpected plan:\n{}", workflow.unwrap());
    }

    #[tokio::test]
    async fn it_finds_a_workflow_to_update_the_hostapp_on_a_fresh_device() {
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
            "needs_cleanup": false
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
            "needs_cleanup": false
        }))
        .unwrap();

        let workflow = worker().find_workflow(initial_state, target).unwrap();
        let expected: Dag<&str> = seq!(
            "ensure clean-up",
            "initialize hostOS release 'target-release'",
            "install hostOS release 'target-release'",
            "complete hostOS release install for 'target-release'",
            "perform clean-up"
        );
        assert_eq!(workflow.to_string(), expected.to_string());
    }

    #[tokio::test]
    async fn it_finds_a_workflow_to_update_the_hostapp() {
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
            "needs_cleanup": false,
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
            "needs_cleanup": false,
        }))
        .unwrap();

        let workflow = worker().find_workflow(initial_state, target).unwrap();
        let expected: Dag<&str> =
            seq!("ensure clean-up", "initialize hostOS release 'new-release'",)
                + par!(
                    "install hostOS release 'new-release'",
                    "clean up metadata for previous hostOS release 'old-release'",
                )
                + seq!(
                    "complete hostOS release install for 'new-release'",
                    "perform clean-up"
                );
        assert_eq!(workflow.to_string(), expected.to_string());
    }
}
