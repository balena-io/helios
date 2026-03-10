use mahler::exception;
use mahler::worker::{Uninitialized, Worker};

use helios_store::DocumentStore;

use crate::oci::{Client as Docker, RegistryAuth};

use super::config::HostRuntimeDir;
use super::models::Device;
use super::tasks::{with_device_tasks, with_hostapp_tasks, with_image_tasks, with_userapp_tasks};

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

pub type LocalWorker = Worker<Device, Uninitialized>;

/// Create worker with necessary resources
pub fn create(
    docker: Docker,
    local_store: DocumentStore,
    host_runtime_dir: HostRuntimeDir,
    registry_auth_client: Option<RegistryAuth>,
) -> LocalWorker {
    // Create the worker and set-up resources
    let mut worker = worker().resource(docker);

    if let Some(auth_client) = registry_auth_client {
        worker.use_resource(auth_client);
    }

    worker.use_resource(host_runtime_dir);
    worker.use_resource(local_store);
    worker
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::DeviceTarget;

    use mahler::dag::{Dag, dag, par, seq};
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
                            "installed": true,
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "container_name": "my-release-uuid_service1",
                                    "started": true,
                                    "config": {},
                                },
                                "service2": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "container_name": "my-release-uuid_service2",
                                    "started": true,
                                    "config": {},
                                },
                                "service3": {
                                    "id": 3,
                                    // different image same digest
                                    "image": "registry2.balena-cloud.com/v2/deafc41f@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "started": true,
                                    "container_name": "my-release-uuid_service3",
                                    "config": {},
                                },
                                // additional images to test download batching
                                "service4": {
                                    "id": 4,
                                    "image": "alpine:latest",
                                    "started": true,
                                    "container_name": "my-release-uuid_service4",
                                    "config": {},
                                },
                                "service5": {
                                    "id": 5,
                                    "image": "alpine:3.20",
                                    "container_name": "my-release-uuid_service5",
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
            "initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'"
        ) + par!(
            "initialize service 'service1' for release 'my-release-uuid'",
            "initialize service 'service2' for release 'my-release-uuid'",
            "initialize service 'service3' for release 'my-release-uuid'",
            "initialize service 'service4' for release 'my-release-uuid'",
            "initialize service 'service5' for release 'my-release-uuid'",
        ) + par!(
            "pull image 'ubuntu:latest'",
            "pull image 'registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080'",
            "pull image 'alpine:latest'",
        ) + par!(
            "tag image 'registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080' \
                        as 'registry2.balena-cloud.com/v2/deafc41f@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080'",
            "pull image 'alpine:3.20'",
        ) + par!(
            "install service 'service1' for release 'my-release-uuid'",
            "install service 'service2' for release 'my-release-uuid'",
            "install service 'service3' for release 'my-release-uuid'",
            "install service 'service4' for release 'my-release-uuid'",
            "install service 'service5' for release 'my-release-uuid'",
        ) + par!(
            "start service 'service1' for release 'my-release-uuid'",
            "start service 'service2' for release 'my-release-uuid'",
            "start service 'service3' for release 'my-release-uuid'",
            "start service 'service4' for release 'my-release-uuid'",
            "start service 'service5' for release 'my-release-uuid'",
        ) + seq!(
            "finish release 'my-release-uuid' for app with uuid 'my-app-uuid'"
        ) + seq!("clean-up");

        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_to_reconfigure_a_service() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app-name",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {
                                "my-service": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "container_name": "old_container",
                                    "started": true,
                                    "container": {
                                        "id": "deadbeef",
                                        "status": "running",
                                        "created": "2026-02-11T15:03:43Z",
                                    },
                                    "config": {
                                        "command": ["sleep", "infinity"]
                                    },
                                },
                            }
                        }
                    }
                }
            },
            "images": {
                "ubuntu:latest" : {
                    "engine_id": "abcde",
                    "download_progress": 100,
                }
            }
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app-name",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {
                                "my-service": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "container_name": "new_container",
                                    "started": true,
                                    "config": {
                                        "command": ["sleep", "10"]
                                    },
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
            "stop service 'my-service' for release 'my-release-uuid'",
            "remove container for service 'my-service' for release 'my-release-uuid'",
            "install service 'my-service' for release 'my-release-uuid'",
            "start service 'my-service' for release 'my-release-uuid'"
        ) + seq!("clean-up");

        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_to_rename_a_service_container() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app-name",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {
                                "my-service": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "started": true,
                                    "container_name": "old_container",
                                    "container": {
                                        "id": "deadbeef",
                                        "status": "running",
                                        "created": "2026-02-11T15:03:43Z",
                                    },
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
            "images": {
                "ubuntu:latest" : {
                    "engine_id": "abcde",
                    "download_progress": 100,
                }
            }
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app-name",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {
                                "my-service": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "started": true,
                                    "container_name": "new_container",
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
        let expected: Dag<&str> =
            seq!("rename container for service 'my-service' for release 'my-release-uuid'",)
                + seq!("clean-up");

        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    // this never really happens, but it's useful for testing that the tasks
    // are well defined
    #[test]
    fn it_finds_a_workflow_to_uninstall_service() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app-name",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "container_name": "my-release-uuid_service1",
                                    "started": true,
                                    "container": {
                                        "id": "deadbeef",
                                        "status": "running",
                                        "created": "2026-02-11T15:03:43Z",
                                    },
                                    "config": {},
                                },
                                "service2": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "container_name": "my-release-uuid_service2",
                                    "started": true,
                                    "container": {
                                        "id": "deadc41f",
                                        "status": "running",
                                        "created": "2026-02-11T15:03:43Z",
                                    },
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
            "images": {
                "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080" : {
                    "engine_id": "abcde",
                    "download_progress": 100,
                }
            }
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app-name",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "container_name": "my-release-uuid_service1",
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
            "stop service 'service2' for release 'my-release-uuid'",
            "uninstall service 'service2' for release 'my-release-uuid'",
        ) + seq!("clean-up");

        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_to_remove_an_app() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app-name",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "container_name": "my-release-uuid_service1",
                                    "started": true,
                                    "container": {
                                        "id": "deadbeef",
                                        "status": "running",
                                        "created": "2026-02-11T15:03:43Z",
                                    },
                                    "config": {},
                                },
                                "service2": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "container_name": "my-release-uuid_service2",
                                    "started": true,
                                    "container": {
                                        "id": "deadc41f",
                                        "status": "running",
                                        "created": "2026-02-11T15:03:43Z",
                                    },
                                    "config": {},
                                },
                            },
                            "networks": {
                                "my-network": {
                                    "network_name": "my-app-uuid_my-network",
                                    "config": {
                                        "driver": "bridge",
                                        "driver_opts": {},
                                        "enable_ipv6": false,
                                        "internal": false,
                                        "labels": {},
                                        "ipam": {
                                            "driver": "default",
                                            "config": [],
                                            "options": {},
                                        },
                                    },
                                },
                            },
                        }
                    }
                }
            },
            "images": {
                "ubuntu:latest": {
                    "engine_id": "dfe123",
                    "download_progress": 100,
                },
                "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080" : {
                    "engine_id": "abcde",
                    "download_progress": 100,
                }
            }
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "uuid": "my-device-uuid",
            "apps": {},
        }))
        .unwrap();

        let (_, workflow) = worker()
            .initial_state(initial_state)
            .find_workflow(target)
            .unwrap();
        let expected: Dag<&str> = dag!(
            seq!("remove network 'my-network' for app 'my-app-uuid'"),
            seq!(
                "stop service 'service1' for release 'my-release-uuid'",
                "uninstall service 'service1' for release 'my-release-uuid'",
            ),
            seq!(
                "stop service 'service2' for release 'my-release-uuid'",
                "uninstall service 'service2' for release 'my-release-uuid'",
            )
        ) + seq!(
            "remove release 'my-release-uuid' for app with uuid 'my-app-uuid'",
            "remove app with uuid 'my-app-uuid'"
        ) + seq!("clean-up");

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
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {
                                "one": {
                                    "id": 1,
                                    "image": "sha256:deadbeef",
                                    "container_name": "my-release-uuid_one",
                                    "started": true,
                                    "container": {
                                        "id": "deadbeef",
                                        "status": "running",
                                        "created": "2026-02-11T15:03:43Z",
                                    },
                                    "config": {},
                                },
                                "two": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:b111111111111111111111111111111111111111111111111111111111111111",
                                    "container": {
                                        "id": "deadc41f",
                                        "status": "running",
                                        "created": "2026-02-11T15:03:43Z",
                                    },
                                    "container_name": "my-release-uuid_two",
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
                            "installed": true,
                            "services": {
                                "one": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/deafc41f@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "container_name": "my-release-uuid_one",
                                    "started": true,
                                    "config": {},
                                },
                                "two": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:b111111111111111111111111111111111111111111111111111111111111111",
                                    "container_name": "my-release-uuid_two",
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
            "update image metadata for service 'one' of release 'my-release-uuid'",
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
    fn it_finds_a_workflow_for_updating_services() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
                    "releases": {
                        "old-release": {
                            "installed": true,
                            "services": {
                                // this service is being updated
                                "service1": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc1@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "container_name": "old-release_service1",
                                    "started": true,
                                    "container": {
                                        "id": "deadbeef",
                                        "status": "running",
                                        "created": "2026-02-11T15:03:43Z",
                                    },
                                    "config": {},
                                },
                                // so is this service, however is not currently running
                                "service2":  {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc2@sha256:a222222222222222222222222222222222222222222222222222222222222222",
                                    "container_name": "old-release_service2",
                                    "started": true,
                                    "container": {
                                        "id": "deadc41f",
                                        "status": "stopped",
                                        "created": "2026-02-11T15:03:43Z",
                                    },
                                    "config": {},
                                },
                                // this service should be migrated
                                "service3":  {
                                    "id": 3,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc2@sha256:a333333333333333333333333333333333333333333333333333333333333333",
                                    "container_name": "old-release_service3",
                                    "started": true,
                                    "container": {
                                        "id": "badbeef",
                                        "status": "running",
                                        "created": "2026-02-11T15:03:43Z",
                                    },
                                    "config": {},
                                },
                                // this service is being removed
                                "service4a":  {
                                    "id": 3,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc4@sha256:a444444444444444444444444444444444444444444444444444444444444444",
                                    "container_name": "old-release_service4a",
                                    "started": true,
                                    "container": {
                                        "id": "badc41f",
                                        "status": "running",
                                        "created": "2026-02-11T15:03:43Z",
                                    },
                                    "config": {},
                                },

                            }
                        }
                    }
                }
            },
            "images": {
                "registry2.balena-cloud.com/v2/oldsvc1@sha256:a111111111111111111111111111111111111111111111111111111111111111": {
                    "config": {},
                    "download_progress": 100,
                    "engine_id": "111"
                },
                "registry2.balena-cloud.com/v2/oldsvc2@sha256:a222222222222222222222222222222222222222222222222222222222222222": {
                    "config": {},
                    "download_progress": 100,
                    "engine_id": "222"
                },
                "registry2.balena-cloud.com/v2/oldsvc3@sha256:a333333333333333333333333333333333333333333333333333333333333333": {
                    "config": {},
                    "download_progress": 100,
                    "engine_id": "333"
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
                            "installed": true,
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/newsvc1@sha256:b111111111111111111111111111111111111111111111111111111111111111",
                                    "container_name": "new-release_service1",
                                    "started": true,
                                    "config": {},
                                },
                                "service2":  {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/newsvc2@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                    "container_name": "new-release_service2",
                                    "started": true,
                                    "config": {},
                                },
                                "service3":  {
                                    "id": 3,
                                    // same image hash as the service from the old release
                                    "image": "registry2.balena-cloud.com/v2/newsvc3@sha256:a333333333333333333333333333333333333333333333333333333333333333",
                                    "container_name": "new-release_service3",
                                    "started": true,
                                    "config": {},
                                },
                                // this is a new service
                                "service4b":  {
                                    "id": 3,
                                    "image": "registry2.balena-cloud.com/v2/newsvc4@sha256:b444444444444444444444444444444444444444444444444444444444444444",
                                    "container_name": "new-release_service4b",
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

        let expected: Dag<&str> = seq!(
            "initialize release 'new-release' for app with uuid 'my-app-uuid'",
        ) + par!(
            "initialize service 'service1' for release 'new-release'",
            "initialize service 'service2' for release 'new-release'",
            "initialize service 'service3' for release 'new-release'",
            "initialize service 'service4b' for release 'new-release'"
        ) + par!(
            "pull image 'registry2.balena-cloud.com/v2/newsvc1@sha256:b111111111111111111111111111111111111111111111111111111111111111'",
            "pull image 'registry2.balena-cloud.com/v2/newsvc2@sha256:b222222222222222222222222222222222222222222222222222222222222222'",
            "tag image 'registry2.balena-cloud.com/v2/oldsvc3@sha256:a333333333333333333333333333333333333333333333333333333333333333' as 'registry2.balena-cloud.com/v2/newsvc3@sha256:a333333333333333333333333333333333333333333333333333333333333333'"
        ) + seq!(
            "pull image 'registry2.balena-cloud.com/v2/newsvc4@sha256:b444444444444444444444444444444444444444444444444444444444444444'"
        ) + dag!(
            // all the operations below can happen concurrently
            // uninstalls cannot happen until all images have been downloaded
            seq!("install service 'service1' for release 'new-release'"),
            seq!("install service 'service2' for release 'new-release'"),
            seq!("install service 'service4b' for release 'new-release'"),
            seq!(
                "stop service 'service1' for release 'old-release'",
                "uninstall service 'service1' for release 'old-release'"
            ),
            seq!("uninstall service 'service2' for release 'old-release'"),
            par!(
                "remove data for 'service3' for release 'old-release'",
                "migrate service 'service3' to release 'new-release'"
            ),
            seq!(
                "stop service 'service4a' for release 'old-release'",
                "uninstall service 'service4a' for release 'old-release'"
            ),
        ) + par!(
            "remove release 'old-release' for app with uuid 'my-app-uuid'",
            "start service 'service1' for release 'new-release'",
            "start service 'service2' for release 'new-release'",
            "rename container for service 'service3' for release 'new-release'",
            "start service 'service4b' for release 'new-release'",
        ) + seq!(
            "update image metadata for service 'service3' of release 'new-release'",
            "finish release 'new-release' for app with uuid 'my-app-uuid'",
            "clean-up"
        );
        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_to_create_networks() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
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
                            "installed": true,
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "started": true,
                                    "container_name": "my-release-uuid_service1",
                                    "image": "ubuntu:latest",
                                    "config": {},
                                },
                            },
                            "networks": {
                                "my-network": {
                                    "network_name": "my-app-uuid_my-network",
                                },
                            },
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
        let expected: Dag<&str> =
            seq!("initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'")
                + par!(
                    "setup network 'my-network' for app 'my-app-uuid'",
                    "initialize service 'service1' for release 'my-release-uuid'",
                )
                + seq!(
                    "pull image 'ubuntu:latest'",
                    "install service 'service1' for release 'my-release-uuid'",
                    "start service 'service1' for release 'my-release-uuid'",
                    "finish release 'my-release-uuid' for app with uuid 'my-app-uuid'",
                    "clean-up",
                );
        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_to_remove_networks() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {},
                            "networks": {
                                "old-network": {},
                            },
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
                            "installed": true,
                            "services": {},
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
            "remove network 'old-network' for app 'my-app-uuid'",
            "clean-up"
        );

        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_to_create_and_remove_networks() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {},
                            "networks": {
                                "network-a": {},
                            },
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
                            "installed": true,
                            "services": {},
                            "networks": {
                                "network-b": {
                                    "network_name": "my-app-uuid_network-b",
                                },
                            },
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
        let expected: Dag<&str> = par!(
            "remove network 'network-a' for app 'my-app-uuid'",
            "setup network 'network-b' for app 'my-app-uuid'",
        ) + seq!("clean-up");

        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    // Network config updates are handled by removing the old network
    // and creating it with the new config
    #[test]
    fn it_finds_a_workflow_for_updating_networks() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {},
                            "networks": {
                                "my-network": {
                                    "network_name": "my-app-uuid_my-network",
                                    "config": {
                                        "driver": "bridge",
                                        "enable_ipv6": false,
                                    },
                                },
                            },
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
                            "installed": true,
                            "services": {},
                            "networks": {
                                "my-network": {
                                    "network_name": "my-app-uuid_my-network",
                                    "config": {
                                        "driver": "overlay",
                                        "enable_ipv6": true,
                                    },
                                },
                            },
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
            "remove network 'my-network' for app 'my-app-uuid'",
            "setup network 'my-network' for app 'my-app-uuid'",
            "clean-up"
        );

        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_to_create_multiple_networks_and_finalizes_release_after_network_create()
    {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
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
                            "installed": true,
                            "services": {},
                            "networks": {
                                "net-a": {
                                    "network_name": "my-app-uuid_net-a",
                                },
                                "net-b": {
                                    "network_name": "my-app-uuid_net-b",
                                },
                            },
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
        let expected: Dag<&str> =
            seq!("initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'")
                + par!(
                    "setup network 'net-a' for app 'my-app-uuid'",
                    "setup network 'net-b' for app 'my-app-uuid'",
                )
                + seq!(
                    "finish release 'my-release-uuid' for app with uuid 'my-app-uuid'",
                    "clean-up",
                );
        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_to_create_volumes() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
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
                            "installed": true,
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "started": true,
                                    "container_name": "my-release-uuid_service1",
                                    "image": "ubuntu:latest",
                                    "config": {},
                                },
                            },
                            "volumes": {
                                "my-volume": {
                                    "volume_name": "my-app-uuid_my-volume",
                                },
                            },
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
        let expected: Dag<&str> =
            seq!("initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'")
                + par!(
                    "initialize service 'service1' for release 'my-release-uuid'",
                    "setup volume 'my-volume' for app 'my-app-uuid'",
                )
                + seq!(
                    "pull image 'ubuntu:latest'",
                    "install service 'service1' for release 'my-release-uuid'",
                    "start service 'service1' for release 'my-release-uuid'",
                    "finish release 'my-release-uuid' for app with uuid 'my-app-uuid'",
                    "clean-up",
                );
        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_to_remove_volumes() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {},
                            "volumes": {
                                "old-volume": {},
                            },
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
                            "installed": true,
                            "services": {},
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
            "remove volume 'old-volume' for app 'my-app-uuid'",
            "clean-up"
        );

        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_to_create_and_remove_volumes() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {},
                            "volumes": {
                                "volume-a": {},
                            },
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
                            "installed": true,
                            "services": {},
                            "volumes": {
                                "volume-b": {
                                    "volume_name": "my-app-uuid_volume-b",
                                },
                            },
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
        let expected: Dag<&str> = par!(
            "remove volume 'volume-a' for app 'my-app-uuid'",
            "setup volume 'volume-b' for app 'my-app-uuid'",
        ) + seq!("clean-up");

        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    // Volume config updates are handled by removing the old volume
    // and creating it with the new config
    #[test]
    fn it_finds_a_workflow_for_updating_volumes() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {},
                            "volumes": {
                                "my-volume": {
                                    "volume_name": "my-app-uuid_my-volume",
                                    "config": {
                                        "driver": "local",
                                    },
                                },
                            },
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
                            "installed": true,
                            "services": {},
                            "volumes": {
                                "my-volume": {
                                    "volume_name": "my-app-uuid_my-volume",
                                    "config": {
                                        "driver": "local",
                                        "driver_opts": {
                                            "type": "nfs",
                                            "o": "addr=10.0.0.1,rw",
                                            "device": ":/export/data"
                                        },
                                    },
                                },
                            },
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
            "remove volume 'my-volume' for app 'my-app-uuid'",
            "setup volume 'my-volume' for app 'my-app-uuid'",
            "clean-up"
        );

        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_to_create_multiple_volumes_and_finalizes_release_after_volume_create() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
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
                            "installed": true,
                            "services": {},
                            "volumes": {
                                "vol-a": {
                                    "volume_name": "my-app-uuid_vol-a",
                                },
                                "vol-b": {
                                    "volume_name": "my-app-uuid_vol-b",
                                },
                            },
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
        let expected: Dag<&str> =
            seq!("initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'")
                + par!(
                    "setup volume 'vol-a' for app 'my-app-uuid'",
                    "setup volume 'vol-b' for app 'my-app-uuid'",
                )
                + seq!(
                    "finish release 'my-release-uuid' for app with uuid 'my-app-uuid'",
                    "clean-up",
                );
        let workflow = workflow.unwrap();
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_to_update_the_hostapp_on_a_fresh_device() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
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
                        "status": "running",
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
            "clean-up"
        );
        assert_eq!(workflow.unwrap().to_string(), expected.to_string());
    }

    #[test]
    fn it_finds_a_workflow_to_update_the_hostapp_to_a_new_release() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
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
                        "status": "running",
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
                        "status": "running"
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
            + seq!("clean-up");
        assert_eq!(workflow.unwrap().to_string(), expected.to_string());
    }

    #[test]
    fn it_skips_a_hostapp_install_if_already_installed() {
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
                        "status": "running",
                        "install_attempts": 1,
                    },
                    "new-release": {
                        "app": "hostapp-uuid",
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "cde2354",
                        "status": "installed",
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
                        "status": "running"
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
            "clean up metadata for previous hostOS release 'old-release'",
            "clean-up"
        );
        assert_eq!(workflow.unwrap().to_string(), expected.to_string());
    }

    #[test]
    fn it_skips_a_hostapp_install_after_too_many_install_failures() {
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
                        "status": "running",
                        "install_attempts": 1,
                    },
                    "new-release": {
                        "app": "hostapp-uuid",
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "cde2354",
                        "status": "created",
                        "install_attempts": 4,
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
                        "status": "running"
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
            "clean up metadata for previous hostOS release 'old-release'",
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
                        "status": "running",
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

    #[test]
    fn it_finds_a_workflow_for_migrating_networks() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "old-release": {
                            "installed": true,
                            "services": {},
                            "networks": {
                                "my-net": {
                                    "network_name": "my-app-uuid_my-net",
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
                        }
                    }
                }
            }
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "new-release": {
                            "installed": true,
                            "services": {},
                            "networks": {
                                "my-net": {
                                    "network_name": "my-app-uuid_my-net",
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
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

        let workflow = workflow.unwrap();

        let expected: Dag<&str> = seq!(
            "initialize release 'new-release' for app with uuid 'my-app-uuid'",
            "setup network 'my-net' for app 'my-app-uuid'",
        ) + par!(
            "finish release 'new-release' for app with uuid 'my-app-uuid'",
            "remove data for network 'my-net' from release 'old-release'",
        ) + seq!(
            "remove release 'old-release' for app with uuid 'my-app-uuid'",
            "clean-up",
        );

        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_for_migrating_volumes() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "old-release": {
                            "installed": true,
                            "services": {},
                            "volumes": {
                                "my-vol": {
                                    "volume_name": "my-app-uuid_my-vol",
                                    "config": {
                                        "driver_opts": {
                                            "type": "nfs",
                                            "o": "addr=10.0.0.1,rw",
                                            "device": ":/export/data"
                                        },
                                    },
                                },
                            },
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
                        "new-release": {
                            "installed": true,
                            "services": {},
                            "volumes": {
                                "my-vol": {
                                    "volume_name": "my-app-uuid_my-vol",
                                    "config": {
                                        "driver_opts": {
                                            "type": "nfs",
                                            "o": "addr=10.0.0.1,rw",
                                            "device": ":/export/data"
                                        },
                                    },
                                },
                            },
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

        let workflow = workflow.unwrap();

        let expected: Dag<&str> = seq!(
            "initialize release 'new-release' for app with uuid 'my-app-uuid'",
            "setup volume 'my-vol' for app 'my-app-uuid'",
        ) + par!(
            "finish release 'new-release' for app with uuid 'my-app-uuid'",
            "remove data for volume 'my-vol' from release 'old-release'",
        ) + seq!(
            "remove release 'old-release' for app with uuid 'my-app-uuid'",
            "clean-up",
        );

        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_for_migrating_networks_and_volumes() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "old-release": {
                            "installed": true,
                            "services": {},
                            "networks": {
                                "my-net": {
                                    "network_name": "my-app-uuid_my-net",
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
                            "volumes": {
                                "my-vol": {
                                    "volume_name": "my-app-uuid_my-vol",
                                    "config": {
                                        "driver_opts": {
                                            "type": "nfs",
                                            "o": "addr=10.0.0.1,rw",
                                            "device": ":/export/data"
                                        },
                                    },
                                },
                            },
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
                        "new-release": {
                            "installed": true,
                            "services": {},
                            "networks": {
                                "my-net": {
                                    "network_name": "my-app-uuid_my-net",
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
                            "volumes": {
                                "my-vol": {
                                    "volume_name": "my-app-uuid_my-vol",
                                    "config": {
                                        "driver_opts": {
                                            "type": "nfs",
                                            "o": "addr=10.0.0.1,rw",
                                            "device": ":/export/data"
                                        },
                                    },
                                },
                            },
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

        let workflow = workflow.expect("workflow should be found");

        let expected: Dag<&str> =
            seq!("initialize release 'new-release' for app with uuid 'my-app-uuid'")
                + par!(
                    "setup network 'my-net' for app 'my-app-uuid'",
                    "setup volume 'my-vol' for app 'my-app-uuid'",
                )
                + par!(
                    "finish release 'new-release' for app with uuid 'my-app-uuid'",
                    "remove data for network 'my-net' from release 'old-release'",
                    "remove data for volume 'my-vol' from release 'old-release'",
                )
                + seq!(
                    "remove release 'old-release' for app with uuid 'my-app-uuid'",
                    "clean-up",
                );
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_for_migrating_services_networks_and_volumes() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "old-release": {
                            "installed": true,
                            "services": {
                                "my-svc": {
                                    "id": 1,
                                    // same digest as target, different URI name
                                    "image": "registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "container_name": "old-release_my-svc",
                                    "started": true,
                                    "container": {
                                        "id": "deadbeef",
                                        "status": "running",
                                        "created": "2026-02-11T15:03:43Z",
                                    },
                                    "config": {},
                                },
                            },
                            "networks": {
                                "my-net": {
                                    "network_name": "my-app-uuid_my-net",
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
                            "volumes": {
                                "my-vol": {
                                    "volume_name": "my-app-uuid_my-vol",
                                    "config": {
                                        "driver_opts": {
                                            "type": "nfs",
                                            "o": "addr=10.0.0.1,rw",
                                            "device": ":/export/data"
                                        },
                                    },
                                },
                            },
                        }
                    }
                }
            },
            "images": {
                "registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111": {
                    "config": {},
                    "download_progress": 100,
                    "engine_id": "111"
                },
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
                        "new-release": {
                            "installed": true,
                            "services": {
                                "my-svc": {
                                    "id": 1,
                                    // same digest as old release, different URI name
                                    "image": "registry2.balena-cloud.com/v2/newsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "container_name": "new-release_my-svc",
                                    "started": true,
                                    "config": {},
                                },
                            },
                            "networks": {
                                "my-net": {
                                    "network_name": "my-app-uuid_my-net",
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
                            "volumes": {
                                "my-vol": {
                                    "volume_name": "my-app-uuid_my-vol",
                                    "config": {
                                        "driver_opts": {
                                            "type": "nfs",
                                            "o": "addr=10.0.0.1,rw",
                                            "device": ":/export/data"
                                        },
                                    },
                                },
                            },
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

        let workflow = workflow.expect("workflow should be found");

        let expected: Dag<&str> = seq!(
            "initialize release 'new-release' for app with uuid 'my-app-uuid'"
        ) + par!(
            "setup network 'my-net' for app 'my-app-uuid'",
            "initialize service 'my-svc' for release 'new-release'",
            "setup volume 'my-vol' for app 'my-app-uuid'",
        ) + seq!(
            "tag image 'registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111' as 'registry2.balena-cloud.com/v2/newsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111'",
        ) + dag!(
            seq!("remove data for network 'my-net' from release 'old-release'"),
            par!(
                "remove data for 'my-svc' for release 'old-release'",
                "migrate service 'my-svc' to release 'new-release'"
            ),
            seq!("remove data for volume 'my-vol' from release 'old-release'"),
        ) + par!(
            "remove release 'old-release' for app with uuid 'my-app-uuid'",
            "rename container for service 'my-svc' for release 'new-release'",
        ) + seq!(
            "update image metadata for service 'my-svc' of release 'new-release'",
            "finish release 'new-release' for app with uuid 'my-app-uuid'",
            "clean-up",
        );
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }

    #[test]
    fn it_finds_a_workflow_for_migrating_and_recreating_networks_and_volumes() {
        before();

        // Old release has same-config network + volume (will migrate)
        // and a different-config network + volume (will uninstall and recreate)
        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "old-release": {
                            "installed": true,
                            "services": {},
                            "networks": {
                                "same-net": {
                                    "network_name": "my-app-uuid_same-net",
                                    "config": {
                                        "driver_opts": { "foo": "bar" },
                                    },
                                },
                                "changed-net": {
                                    "network_name": "my-app-uuid_changed-net",
                                    "config": {
                                        "enable_ipv6": false,
                                    },
                                },
                            },
                            "volumes": {
                                "same-vol": {
                                    "volume_name": "my-app-uuid_same-vol",
                                    "config": {
                                        "driver_opts": { "type": "nfs" },
                                    },
                                },
                                "changed-vol": {
                                    "volume_name": "my-app-uuid_changed-vol",
                                    "config": {
                                        "driver_opts": { "type": "local" },
                                    },
                                },
                            },
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
                        "new-release": {
                            "installed": true,
                            "services": {},
                            "networks": {
                                "same-net": {
                                    "network_name": "my-app-uuid_same-net",
                                    "config": {
                                        "driver_opts": { "foo": "bar" },
                                    },
                                },
                                "changed-net": {
                                    "network_name": "my-app-uuid_changed-net",
                                    "config": {
                                        "enable_ipv6": true,
                                    },
                                },
                            },
                            "volumes": {
                                "same-vol": {
                                    "volume_name": "my-app-uuid_same-vol",
                                    "config": {
                                        "driver_opts": { "type": "nfs" },
                                    },
                                },
                                "changed-vol": {
                                    "volume_name": "my-app-uuid_changed-vol",
                                    "config": {
                                        "driver_opts": { "type": "tmpfs" },
                                    },
                                },
                            },
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

        let workflow = workflow.expect("workflow should be found");

        // changed-net and changed-vol get uninstalled (Docker delete) then recreated
        // same-net and same-vol get migrated (state-only removal from old release)
        let expected: Dag<&str> = par!(
            "initialize release 'new-release' for app with uuid 'my-app-uuid'",
            "remove network 'changed-net' for app 'my-app-uuid'",
            "remove volume 'changed-vol' for app 'my-app-uuid'",
        ) + par!(
            "setup network 'changed-net' for app 'my-app-uuid'",
            "setup network 'same-net' for app 'my-app-uuid'",
            "setup volume 'changed-vol' for app 'my-app-uuid'",
            "setup volume 'same-vol' for app 'my-app-uuid'",
        ) + par!(
            "finish release 'new-release' for app with uuid 'my-app-uuid'",
            "remove data for network 'same-net' from release 'old-release'",
            "remove data for volume 'same-vol' from release 'old-release'",
        ) + seq!(
            "remove release 'old-release' for app with uuid 'my-app-uuid'",
            "clean-up",
        );
        assert_eq!(
            workflow.to_string(),
            expected.to_string(),
            "unexpected plan:\n{workflow}"
        );
    }
}
