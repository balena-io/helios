use std::collections::HashMap;

use mahler::extract::{Args, Res, System, Target, View};
use mahler::job;
use mahler::state::Map;
use mahler::task::prelude::*;
use mahler::worker::{Uninitialized, Worker};

use crate::common_types::{ImageUri, Uuid};
use crate::models::{
    App, AppMap, AppTarget, Device, ImageRef, Network, RegistryAuthSet, Release, Service,
    ServiceContainerStatus, ServiceContainerSummary, ServiceTarget,
};
use crate::oci::{Client as Docker, Error as OciError, RegistryAuth, WithContext};
use crate::util::store::{Store, StoreError};

use super::image::{create_image, remove_image, remove_images, request_registry_credentials};
use super::utils::find_installed_service;

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Oci(#[from] OciError),
}

/// Request authorization for new image installs
fn request_registry_token_for_new_images(
    System(device): System<Device>,
    Target(tgt_apps): Target<AppMap>,
) -> Option<Task> {
    // Find all images for new services in the target state
    let images_to_install: Vec<&ImageUri> = tgt_apps
        .iter()
        .flat_map(|(app_uuid, app)| {
            app.releases.iter().flat_map(|(commit, release)| {
                release
                    .services
                    .iter()
                    .filter_map(|(svc_name, svc)| {
                        if device
                            .apps
                            .get(app_uuid)
                            .and_then(|app| app.releases.get(commit))
                            .and_then(|rel| rel.services.get(svc_name))
                            .is_some()
                        {
                            // the service already exists, ignore it
                            return None;
                        }

                        if let ImageRef::Uri(img_uri) = &svc.image {
                            Some((svc_name, img_uri))
                        } else {
                            None
                        }
                    })
                    .filter(|(svc_name, svc_img)| {
                        // ignore the image if it already exists
                        if device.images.contains_key(svc_img) {
                            return false;
                        }

                        // if the image is for a new service or the existing service has a
                        // different digest (which means a new download), then add it to the
                        // authorization list
                        find_installed_service(&device, app_uuid, commit, svc_name).is_none_or(
                            |s| s.image.digest().is_none() || s.image.digest() != svc_img.digest(),
                        )
                    })
                    // then select the image
                    .map(|(_, svc_img)| svc_img)
            })
        })
        .collect();

    // Group images to install by registry
    let tgt_auths: RegistryAuthSet = images_to_install
        .iter()
        .fold(HashMap::<String, Vec<ImageUri>>::new(), |mut acc, img| {
            let img = (*img).clone();
            if let Some(service) = img.registry() {
                if let Some(scope) = acc.get_mut(service) {
                    scope.push(img);
                } else {
                    acc.insert(service.clone(), vec![img]);
                }
            }
            acc
        })
        .into_values()
        .map(|scope| RegistryAuth::try_from(scope).expect("auth creation should not fail"))
        .filter(|scope| device.auths.iter().all(|s| !s.is_super_scope(scope)))
        .collect();

    if tgt_auths.is_empty() {
        return None;
    }

    Some(request_registry_credentials.with_target(tgt_auths))
}

/// Initialize the app and store its local data
fn create_app(
    maybe_app: View<Option<App>>,
    Target(tgt_app): Target<App>,
    Args(app_uuid): Args<Uuid>,
    store: Res<Store>,
) -> IO<App, StoreError> {
    enforce!(maybe_app.is_none(), "app already exists");

    let AppTarget { id, name, .. } = tgt_app;
    let app = maybe_app.create(App {
        id,
        name,
        releases: Map::new(),
    });

    with_io(app, async move |app| {
        let local_store = store.as_ref().expect("store should be available");
        // store id and name as local state
        local_store
            .write(format!("/apps/{app_uuid}"), "id", &app.id)
            .await?;
        local_store
            .write(format!("/apps/{app_uuid}"), "name", &app.name)
            .await?;

        Ok(app)
    })
}

/// Remove an empty app
fn remove_app(
    app: View<App>,
    Args(app_uuid): Args<Uuid>,
    store: Res<Store>,
) -> IO<Option<App>, StoreError> {
    enforce!(app.releases.is_empty());

    let app = app.delete();
    with_io(app, async move |app| {
        // remove app metadata
        let local_store = store.as_ref().expect("store should be available");
        local_store.delete_all(format!("/apps/{app_uuid}")).await?;

        Ok(app)
    })
}

/// Update the local app id
fn store_app_id(
    mut id: View<u32>,
    Target(tgt): Target<u32>,
    Args(app_uuid): Args<String>,
    store: Res<Store>,
) -> IO<u32, StoreError> {
    *id = tgt;
    with_io(id, async move |id| {
        let local_store = store.as_ref().expect("store should be available");
        local_store
            .write(format!("/apps/{app_uuid}"), "id", &*id)
            .await?;
        Ok(id)
    })
}

/// Update the local app name
fn store_app_name(
    mut name: View<Option<String>>,
    Target(tgt): Target<Option<String>>,
    Args(app_uuid): Args<String>,
    store: Res<Store>,
) -> IO<Option<String>, StoreError> {
    *name = tgt;
    with_io(name, async move |name| {
        let local_store = store.as_ref().expect("store should be available");
        local_store
            .write(format!("/apps/{app_uuid}"), "name", &*name)
            .await?;
        Ok(name)
    })
}

/// Install all new images for all target apps
fn fetch_apps_images(
    System(device): System<Device>,
    Target(tgt_apps): Target<AppMap>,
) -> Vec<Task> {
    // Find all images for new services in the target state
    let images_to_install: Vec<&ImageUri> = tgt_apps
        .iter()
        .flat_map(|(app_uuid, app)| {
            app.releases.iter().flat_map(|(commit, release)| {
                release
                    .services
                    .iter()
                    // find all services that need downloading
                    .filter_map(|(svc_name, svc)| {
                        if device
                            .apps
                            .get(app_uuid)
                            .and_then(|app| app.releases.get(commit))
                            .and_then(|rel| rel.services.get(svc_name))
                            .is_none_or(|svc| svc.container.is_some())
                        {
                            // the service already has a container, ignore
                            // the image
                            return None;
                        }

                        // only use the target image ref if it is an URI
                        // (this should always be the case)
                        if let ImageRef::Uri(img_uri) = &svc.image {
                            Some((svc_name, img_uri))
                        } else {
                            None
                        }
                    })
                    .filter(|(svc_name, svc_img)| {
                        // ignore the image if it already exists
                        if device.images.contains_key(svc_img) {
                            return false;
                        }

                        // select the image if it is for a new service or the existing service image has the
                        // same digest (which means we are just re-tagging)
                        find_installed_service(&device, app_uuid, commit, svc_name).is_none_or(
                            |s| s.image.digest().is_some() && svc_img.digest() == s.image.digest(),
                        )
                    })
            })
        })
        // remove duplicate digests
        .fold(Vec::<&ImageUri>::new(), |mut acc, (_, svc_img)| {
            if acc
                .iter()
                .all(|img| img.digest().is_none() || img.digest() != svc_img.digest())
            {
                acc.push(svc_img);
            }
            acc
        });

    // download at most 3 images at the time
    let mut tasks: Vec<Task> = Vec::new();
    for image in images_to_install.into_iter().take(3) {
        tasks.push(create_image.with_arg("image_name", image.clone()))
    }
    tasks
}

/// Initialize an empty release
fn create_release(release: View<Option<Release>>) -> View<Release> {
    release.create(Release {
        installed: false,
        services: Map::new(),
        networks: Map::new(),
    })
}

/// Finalize an installed release
fn finish_release(
    mut release: View<Release>,
    Target(target): Target<Release>,
    Args((app_uuid, commit)): Args<(Uuid, Uuid)>,
    store: Res<Store>,
) -> IO<Release, StoreError> {
    // all target services have been installed
    enforce!(target.services.iter().all(|(svc_name, tgt_svc)| {
        release
            .services
            .get(svc_name)
            .map(|svc| tgt_svc == &ServiceTarget::from(svc.clone()))
            .unwrap_or_default()
    }));

    // all target networks have been created
    enforce!(target.networks.iter().all(|(net_name, tgt_net)| {
        release
            .networks
            .get(net_name)
            .map(|net| tgt_net == net)
            .unwrap_or_default()
    }));

    release.installed = true;
    with_io(release, async move |release| {
        // mark the release as installed on the local store
        let local_store = store.as_ref().expect("store should be available");
        local_store
            .write(
                format!("/apps/{app_uuid}/releases/{commit}"),
                "installed",
                &true,
            )
            .await?;

        Ok(release)
    })
}

/// Remove an empty release
fn remove_release(
    release: View<Release>,
    Args((app_uuid, commit)): Args<(Uuid, Uuid)>,
    store: Res<Store>,
) -> IO<Option<Release>, StoreError> {
    enforce!(release.services.is_empty());
    enforce!(release.networks.is_empty());

    let release = release.delete();
    with_io(release, async move |release| {
        // remove release metadata
        let local_store = store.as_ref().expect("store should be available");
        local_store
            .delete_all(format!("/apps/{app_uuid}/releases/{commit}"))
            .await?;

        Ok(release)
    })
}

/// Remove release methods through a service to help with
/// operation concurrency
fn remove_release_services(
    System(device): System<Device>,
    release: View<Release>,
    Args((app_uuid, commit)): Args<(Uuid, Uuid)>,
) -> Vec<Task> {
    let mut tasks = Vec::new();

    let mut images_to_remove = Vec::new();
    for (svc_name, svc) in release.services.iter() {
        if svc
            .container
            .as_ref()
            .is_some_and(|c| c.status == ServiceContainerStatus::Running)
        {
            // stop the service if running
            tasks.push(stop_service.with_arg("service_name", svc_name));
        } else {
            // otherwise remove it
            tasks.push(remove_service.with_arg("service_name", svc_name));

            if let ImageRef::Uri(svc_img) = &svc.image
                && device.images.contains_key(svc_img)
            {
                // find any services from a different app and release that
                // are still using the image
                let image_is_used = device.apps.iter().any(|(a_uuid, app)| {
                    a_uuid != &app_uuid
                        && app.releases.iter().any(|(r_uuid, rel)| {
                            r_uuid != &commit
                                && rel.services.values().any(|s| {
                                    if let ImageRef::Uri(s_img) = &s.image {
                                        s_img == svc_img
                                    } else {
                                        false
                                    }
                                })
                        })
                });

                // remove the image if it exists and it's not used by other service
                if !image_is_used {
                    images_to_remove.push(svc_img);
                }
            }
        }
    }

    // remove all images from removed services
    tasks.push(remove_images.with_target(images_to_remove));

    tasks
}

/// Create a network in Docker and the state tree
fn create_network(
    net: View<Option<Network>>,
    Target(tgt): Target<Network>,
    Args((_app_uuid, _, _network_name)): Args<(Uuid, Uuid, String)>,
    docker: Res<Docker>,
) -> IO<Network, Error> {
    let net = net.create(tgt);

    with_io(net, async move |mut net| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        docker
            .network()
            .create(&net.network_name, net.config.clone().into())
            .await?;

        // Re-read the network from Docker to capture engine-assigned values
        let local_network = docker
            .network()
            .inspect(&net.network_name)
            .await
            .with_context(|| format!("failed to inspect network '{}'", net.network_name))?;
        *net = Network::from(local_network);
        Ok(net)
    })
}

/// Remove a network from Docker and the state tree
fn remove_network(
    net: View<Network>,
    Args((_app_uuid, _, _network_name)): Args<(Uuid, Uuid, String)>,
    docker: Res<Docker>,
) -> IO<Option<Network>, Error> {
    let docker_name = net.network_name.clone();
    let net = net.delete();

    with_io(net, async move |net| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        docker.network().remove(&docker_name).await?;
        Ok(net)
    })
}

/// Create the service in memory before initiating download
fn create_service(maybe_svc: View<Option<Service>>, Target(tgt): Target<Service>) -> View<Service> {
    let ServiceTarget {
        id,
        container_name,
        image,
        config,
        ..
    } = tgt;
    maybe_svc.create(Service {
        id,
        container_name,
        image,
        started: false,
        container: None,
        config,
    })
}

/// Install the service if the images exist
///
/// Implement this as a method to allow to make concurrent service installs
fn install_service_when_image_is_ready(
    System(device): System<Device>,
    Target(tgt): Target<Service>,
) -> Option<Task> {
    // Skip the task if the image for the service doesn't exist yet
    if let ImageRef::Uri(tgt_img) = &tgt.image
        && device.images.contains_key(tgt_img)
    {
        return Some(install_service.with_target(tgt));
    }

    None
}

/// Install the service
fn install_service(
    mut svc: View<Service>,
    Args((app_uuid, commit, svc_name)): Args<(Uuid, Uuid, String)>,
    docker: Res<Docker>,
    store: Res<Store>,
) -> IO<Service, Error> {
    enforce!(svc.container.is_none(), "service container already exists");

    // simulate a service install by creating a mock container
    // the mock will never be seen by users
    svc.container.replace(ServiceContainerSummary::mock());
    with_io(svc, async move |mut svc| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        let local_store = store.as_ref().expect("store should be available");
        let container_name = svc
            .container_name
            .clone()
            .expect("container name should be available");

        if let ImageRef::Uri(svc_img) = svc.image.clone() {
            let image = docker
                .image()
                .inspect(svc_img.as_str())
                .await
                .with_context(|| format!("failed to inspect image {svc_img}"))?;

            // convert the service configuration to a container configuration
            // and mark duplicates from the image
            let container_config = svc.config.clone().into_container_config(&image.config);

            let container_id = docker
                .container()
                .create(&container_name, &svc_img, container_config)
                .await?;

            // check that the container was created and generate the Service configuration
            // from the image config and container info
            let local_container = docker
                .container()
                .inspect(&container_id)
                .await
                .context("failed to inspect container for service")?;
            *svc = Service::from_local_container(local_container, &image.config);
            svc.image = ImageRef::Uri(svc_img);

            // store the image uri that corresponds to the current release service
            local_store
                .write(
                    format!("/apps/{app_uuid}/releases/{commit}/services/{svc_name}"),
                    "image",
                    &svc.image,
                )
                .await?;
        }

        Ok(svc)
    })
}

/// Start the service if it is not running yet
fn start_service(
    mut svc: View<Service>,
    Target(tgt_svc): Target<Service>,
    docker: Res<Docker>,
) -> IO<Service, OciError> {
    enforce!(
        svc.container
            .as_ref()
            .is_some_and(|c| c.status != ServiceContainerStatus::Running),
        "service container should exist and should not be running"
    );

    // creating the container will not fail if the container already exists, however
    // that doesn't guarantee the configuration will be the same. In that case we'll
    // need to loop again to re-create the container
    enforce!(
        svc.config == tgt_svc.config,
        "service configuration should match the target before"
    );

    svc.started = true;
    with_io(svc, async move |mut svc| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        // this is guaranteed by the enforce above
        let container_id = svc
            .container
            .as_ref()
            .map(|c| &c.id)
            .expect("container should be available");

        // start the container
        docker
            .container()
            .start(container_id)
            .await
            .context("failed to start container for service")?;

        // re-read container state
        let local_container = docker
            .container()
            .inspect(container_id)
            .await
            .context("failed to inspect container for service")?;

        svc.container.replace(ServiceContainerSummary::from((
            local_container.id.as_ref(),
            local_container.state,
        )));

        Ok(svc)
    })
}

/// Stop a running service
fn stop_service(mut svc: View<Service>, docker: Res<Docker>) -> IO<Service, OciError> {
    let container_id = if let Some(container) = svc.container.as_mut()
        && container.status == ServiceContainerStatus::Running
    {
        container.status = ServiceContainerStatus::Stopped;
        container.id.clone()
    } else {
        return IO::abort("service container should exist and should be running");
    };

    with_io(svc, async move |mut svc| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        if let Some(container) = svc.container.as_mut() {
            // set the container status before stopping
            container.status = ServiceContainerStatus::Stopping;
        }
        let _ = svc.flush().await;

        // stop the container
        // FIXME: this needs update locks
        docker
            .container()
            .stop(&container_id)
            .await
            .context("failed to stop container for service")?;

        // re-read container state
        let local_container = docker
            .container()
            .inspect(&container_id)
            .await
            .context("failed to inspect container for service")?;

        svc.container.replace(ServiceContainerSummary::from((
            local_container.id.as_ref(),
            local_container.state,
        )));

        Ok(svc)
    })
}

/// Stop a service if running and remove the service and its image
fn stop_and_remove_service(
    System(device): System<Device>,
    svc: View<Service>,
    Args((app_uuid, commit, svc_name)): Args<(Uuid, Uuid, String)>,
) -> Vec<Task> {
    let mut tasks = Vec::new();
    if svc
        .container
        .as_ref()
        .is_some_and(|c| c.status == ServiceContainerStatus::Running)
    {
        // stop the service if running
        tasks.push(stop_service.into_task());
    }

    // rmeove the service
    tasks.push(remove_service.into_task());

    if let ImageRef::Uri(svc_img) = &svc.image
        && device.images.contains_key(svc_img)
    {
        let image_is_used = device.apps.iter().any(|(a_uuid, app)| {
            app.releases.iter().any(|(r_uuid, rel)| {
                rel.services.iter().any(|(s_name, s)| {
                    // ignore the current service
                    if a_uuid == &app_uuid && r_uuid == &commit && s_name == &svc_name {
                        return false;
                    }

                    if let ImageRef::Uri(s_img) = &s.image {
                        s_img == svc_img
                    } else {
                        false
                    }
                })
            })
        });

        // remove the image if it exists and it's not used by other service
        if !image_is_used {
            tasks.push(remove_image.with_arg("image_name", svc_img.as_str()));
        }
    }

    tasks
}

/// Remove a stopped service
fn remove_service(
    svc: View<Service>,
    Args((app_uuid, commit, svc_name)): Args<(Uuid, Uuid, String)>,
    docker: Res<Docker>,
    store: Res<Store>,
) -> IO<Option<Service>, Error> {
    let container_id = if let Some(container) = svc.container.as_ref()
        && container.status != ServiceContainerStatus::Running
    {
        container.id.clone()
    } else {
        return IO::abort("service container should exist and should be stopped");
    };

    let svc = svc.delete();
    with_io(svc, async move |svc| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        let local_store = store.as_ref().expect("store should be available");

        // remove the container
        docker
            .container()
            .remove(&container_id)
            .await
            .context("failed to remove container for service")?;

        // remove the service metadata from the local store
        local_store
            .delete_all(format!(
                "/apps/{app_uuid}/releases/{commit}/services/{svc_name}"
            ))
            .await?;
        Ok(svc)
    })
}

/// Update a service image metadata in local storage
fn store_service_image_uri(
    mut img: View<ImageRef>,
    Target(tgt): Target<ImageUri>,
    Args((app_uuid, commit, svc_name)): Args<(Uuid, Uuid, String)>,
    store: Res<Store>,
) -> IO<ImageRef, StoreError> {
    *img = ImageRef::Uri(tgt);

    with_io(img, async move |img| {
        let local_store = store.as_ref().expect("store should be available");
        // store the image uri that corresponds to the current release service
        local_store
            .write(
                format!("/apps/{app_uuid}/releases/{commit}/services/{svc_name}"),
                "image",
                &*img,
            )
            .await?;
        Ok(img)
    })
}

/// Update worker with user app tasks
pub fn with_userapp_tasks<O>(worker: Worker<O, Uninitialized>) -> Worker<O, Uninitialized> {
    worker
        .jobs(
            "/apps",
            [
                job::update(request_registry_token_for_new_images),
                job::update(fetch_apps_images),
            ],
        )
        .jobs(
            "/apps/{app_uuid}",
            [
                job::create(create_app).with_description(|Args(uuid): Args<Uuid>| {
                    format!("initialize app with uuid '{uuid}'")
                }),
                job::delete(remove_app).with_description(|Args(uuid): Args<Uuid>| {
                    format!("remove app with uuid '{uuid}'")
                }),
            ],
        )
        .jobs(
            "/apps/{app_uuid}/name",
            [
                job::create(store_app_name).with_description(|Args(uuid): Args<Uuid>| {
                    format!("store name for app with uuid '{uuid}'")
                }),
                job::update(store_app_name).with_description(|Args(uuid): Args<Uuid>| {
                    format!("store name for app with uuid '{uuid}'")
                }),
            ],
        )
        .job(
            "/apps/{app_uuid}/id",
            job::update(store_app_id).with_description(|Args(uuid): Args<Uuid>| {
                format!("store id for app with uuid '{uuid}'")
            }),
        )
        .jobs(
            "/apps/{app_uuid}/releases/{commit}",
            [
                job::create(create_release).with_description(
                    |Args((uuid, commit)): Args<(Uuid, Uuid)>| {
                        format!("initialize release '{commit}' for app with uuid '{uuid}'")
                    },
                ),
                job::update(finish_release).with_description(
                    |Args((uuid, commit)): Args<(Uuid, Uuid)>| {
                        format!("finish release '{commit}' for app with uuid '{uuid}'")
                    },
                ),
                job::delete(remove_release_services),
                job::delete(remove_release).with_description(
                    |Args((uuid, commit)): Args<(Uuid, Uuid)>| {
                        format!("remove release '{commit}' for app with uuid '{uuid}'")
                    },
                ),
            ],
        )
        .jobs(
            "/apps/{app_uuid}/releases/{commit}/networks/{network_name}",
            [
                job::create(create_network).with_description(
                    |Args((app_uuid, _, network_name)): Args<(Uuid, Uuid, String)>| {
                        format!("create network '{network_name}' for app '{app_uuid}'")
                    },
                ),
                job::update(remove_network).with_description(
                    |Args((app_uuid, _, network_name)): Args<(Uuid, Uuid, String)>| {
                        format!("remove network '{network_name}' for app '{app_uuid}'")
                    },
                ),
                job::delete(remove_network).with_description(
                    |Args((app_uuid, _, network_name)): Args<(Uuid, Uuid, String)>| {
                        format!("remove network '{network_name}' for app '{app_uuid}'")
                    },
                ),
            ],
        )
        .jobs(
            "/apps/{app_uuid}/releases/{commit}/services/{service_name}",
            [
                job::create(create_service).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("initialize service '{service_name}' for release '{commit}'")
                    },
                ),
                job::update(install_service_when_image_is_ready),
                job::update(start_service).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("start service '{service_name}' for release '{commit}'")
                    },
                ),
                job::update(stop_service).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("stop service '{service_name}' for release '{commit}'")
                    },
                ),
                job::delete(stop_and_remove_service),
                job::none(remove_service).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("remove service '{service_name}' for release '{commit}'")
                    },
                ),
                job::none(install_service).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("install service '{service_name}' for release '{commit}'")
                    },
                ),
            ],
        )
        .job(
            "/apps/{app_uuid}/releases/{commit}/services/{service_name}/image",
            job::update(store_service_image_uri).with_description(
                |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                    format!(
                        "store image metadata for service '{service_name}' of release '{commit}'"
                    )
                },
            ),
        )
}
