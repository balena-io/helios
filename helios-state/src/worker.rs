use mahler::extract::Target;
use mahler::worker::Uninitialized;
use mahler::{
    extract::Args,
    task,
    worker::{Ready, Worker},
};
use tokio::sync::RwLock;

use crate::common_types::Uuid;
use crate::oci::{Client as Docker, Error as DockerError, RegistryAuthClient};
use crate::tasks::app::{
    fetch_release_images, fetch_service_image, install_service, prepare_app, prepare_release,
    set_app_name,
};
use crate::tasks::device::{ensure_cleanup, perform_cleanup, set_device_name};
use crate::tasks::image::{
    create_image, pull_image, remove_image, request_registry_credentials,
    request_registry_token_for_new_images, tag_image,
};
use crate::util::store::Store;

use super::models::{Device, DeviceTarget, Image};

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("Failed to connect to Docker daemon: {0}")]
    DockerConnection(#[from] DockerError),

    #[error("Failed to serialize initial state: {0}")]
    StateSerialization(#[from] mahler::errors::SerializationError),
}

/// Configure the worker jobs
///
/// This is mostly used for tests
fn worker() -> Worker<Device, Uninitialized, DeviceTarget> {
    Worker::new()
        .job(
            "/name",
            task::any(set_device_name).with_description(|| "update device name"),
        )
        // XXX: this is not added first because of
        // https://github.com/balena-io-modules/mahler-rs/pull/50
        .jobs(
            "",
            [
                task::update(ensure_cleanup).with_description(|| "ensure clean-up"),
                task::update(perform_cleanup).with_description(|| "perform clean-up"),
            ],
        )
        .job(
            "/auths",
            task::none(request_registry_credentials)
                .with_description(|| "request registry credentials"),
        )
        .jobs(
            "/images/{image_name}",
            [
                task::none(pull_image).with_description(|Args(image_name): Args<String>| {
                    format!("pull image '{image_name}'")
                }),
                task::none(remove_image).with_description(|Args(image_name): Args<String>| {
                    format!("delete image '{image_name}'")
                }),
                task::none(tag_image).with_description(
                    |Args(image_name): Args<String>, tgt: Target<Image>| {
                        format!("tag image '{}' as '{image_name}'", tgt.engine_id)
                    },
                ),
                task::none(create_image),
            ],
        )
        .job("/apps", task::update(request_registry_token_for_new_images))
        .job(
            "/apps/{app_uuid}",
            task::create(prepare_app).with_description(|Args(uuid): Args<Uuid>| {
                format!("initialize app with uuid '{uuid}'")
            }),
        )
        .job(
            "/apps/{app_uuid}/name",
            task::any(set_app_name).with_description(|Args(uuid): Args<Uuid>| {
                format!("update name for app with uuid '{uuid}'")
            }),
        )
        .jobs(
            "/apps/{app_uuid}/releases/{commit}",
            [
                task::create(fetch_release_images),
                task::create(prepare_release).with_description(
                    |Args((uuid, commit)): Args<(Uuid, Uuid)>| {
                        format!("initialize release '{commit}' for app with uuid '{uuid}'")
                    },
                ),
            ],
        )
        .jobs(
            "/apps/{app_uuid}/releases/{commit}/services/{service_name}",
            [
                task::create(fetch_service_image),
                task::create(install_service).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("initialize service '{service_name}' for release '{commit}'")
                    },
                ),
            ],
        )
}

type LocalWorker = Worker<Device, Ready, DeviceTarget>;

/// Create and initialize the worker
pub fn create(
    docker: Docker,
    local_store: Store,
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

    worker.use_resource(local_store);

    Ok(worker)
}

#[cfg(test)]
mod tests {
    use super::*;

    use mahler::{Dag, par, seq};
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
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 0,
                    "name": "my-app"
                }
            }
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
    async fn it_finds_a_workflow_to_create_an_app_and_configs() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 0,
                    "name": "my-app"
                }
            },
        }))
        .unwrap();

        let workflow = worker().find_workflow(initial_state, target).unwrap();
        let expected: Dag<&str> = seq!("ensure clean-up")
            + par!(
                "update device name",
                "initialize app with uuid 'my-app-uuid'",
            )
            + seq!("perform clean-up");
        assert_eq!(workflow.to_string(), expected.to_string());
    }

    #[tokio::test]
    async fn it_finds_a_workflow_to_update_an_app_and_configs() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "name": "my-device-name",
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app-old-name"
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
                    "name": "my-app"
                }
            },
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
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app"
                }
            }
        }))
        .unwrap();
        let target = serde_json::from_value::<DeviceTarget>(json!({
            "uuid": "my-device-uuid",
            "name": "device-name",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name"
                }
            }
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
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
                }
            }
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
                                    "image": "ubuntu:latest"
                                },
                                "service2": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080"
                                },
                                "service3": {
                                    "id": 3,
                                    // different image same digest
                                    "image": "registry2.balena-cloud.com/v2/deafc41f@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080"
                                },
                                // additional images to test download batching
                                "service4": {
                                    "id": 4,
                                    "image": "alpine:latest"
                                },
                                "service5": {
                                    "id": 5,
                                    "image": "alpine:3.20"
                                },
                            }
                        }
                    }
                }
            }
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

    // The worker doesn't have any tasks to update services or delete releases
    // so this plan should fail
    #[tokio::test]
    async fn it_fails_to_find_a_workflow_for_updating_services() {
        before();

        let initial_state = serde_json::from_value::<Device>(json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-new-app-name",
                    "releases": {
                        "old-release": {
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc1@sha256:a111111111111111111111111111111111111111111111111111111111111111"
                                },
                                "service2":  {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc2@sha256:a222222222222222222222222222222222222222222222222222222222222222"
                                },

                            }
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
                    "name": "my-new-app-name",
                    "releases": {
                        "new-release": {
                            "services": {
                                "service1": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/newsvc1@sha256:b111111111111111111111111111111111111111111111111111111111111111"
                                },
                                "service2":  {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/newsvc2@sha256:b222222222222222222222222222222222222222222222222222222222222222"
                                },

                            }
                        }
                    }
                }
            }
        }))
        .unwrap();

        // this should return Err(NotFound) and not panic
        let workflow = worker().find_workflow(initial_state, target);
        assert!(workflow.is_err(), "unexpected plan:\n{}", workflow.unwrap());
    }
}
