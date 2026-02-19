use std::collections::HashMap;

use mahler::extract::{Args, RawTarget, Res, System, SystemTarget, Target, View};
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

use super::image::{create_image, request_registry_credentials};
use super::utils::{find_future_service, find_installed_service};

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
        .flat_map(|(t_app_uuid, t_app)| {
            t_app.releases.iter().flat_map(|(t_rel_uuid, t_rel)| {
                t_rel
                    .services
                    .iter()
                    .filter_map(|(t_svc_name, t_svc)| {
                        if device
                            .apps
                            .get(t_app_uuid)
                            .and_then(|app| app.releases.get(t_rel_uuid))
                            .and_then(|rel| rel.services.get(t_svc_name))
                            .is_some()
                        {
                            // the service already exists in the current state
                            // ignore it. This ensures the request task happens before
                            // creating services
                            return None;
                        }

                        if let ImageRef::Uri(img_uri) = &t_svc.image {
                            Some(img_uri)
                        } else {
                            None
                        }
                    })
                    // ignore the image if it already exists
                    .filter(|t_svc_img| !device.images.contains_key(t_svc_img))
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
fn remove_app(mut app: View<Option<App>>) -> View<Option<App>> {
    if app
        .as_ref()
        .map(|a| a.releases.is_empty())
        .unwrap_or_default()
    {
        app.take();
    }
    app
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
        .flat_map(|(t_app_uuid, t_app)| {
            t_app.releases.iter().flat_map(|(t_rel_uuid, t_rel)| {
                t_rel
                    .services
                    .iter()
                    // find all services that need downloading
                    .filter_map(|(t_svc_name, t_svc)| {
                        if device
                            .apps
                            .get(t_app_uuid)
                            .and_then(|app| app.releases.get(t_rel_uuid))
                            .and_then(|rel| rel.services.get(t_svc_name))
                            .is_none_or(|svc| svc.container.is_some())
                        {
                            // the service already has a container, ignore
                            // the image
                            return None;
                        }

                        // only use the target image ref if it is an URI
                        // (this should always be the case)
                        if let ImageRef::Uri(img_uri) = &t_svc.image {
                            Some(img_uri)
                        } else {
                            None
                        }
                    })
                    // ignore the image if it already exists
                    .filter(|t_svc_img| !device.images.contains_key(t_svc_img))
            })
        })
        // remove duplicate digests
        .fold(Vec::<&ImageUri>::new(), |mut acc, svc_img| {
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
fn remove_release(mut release: View<Option<Release>>) -> View<Option<Release>> {
    // remove the release if it has no services
    if release
        .as_ref()
        .map(|r| r.services.is_empty())
        .unwrap_or_default()
    {
        release.take();
    }
    release
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

/// Migrate a service to the current release from another location
fn migrate_service(
    maybe_svc: View<Option<Service>>,
    RawTarget(tgt): RawTarget<Service>,
) -> View<Service> {
    maybe_svc.create(tgt)
}

/// Install the service when requirements are met for this operation
///
/// A service can be installed after
/// - the image has been pulled
/// - any networks and volumes exist with the right configuration (TODO)
/// - if upgrading between releases, and there is an identically named service in
///   a previous release, the services are for different images and have different
///   configurations, otherwise the old service just requires as migration
///
/// These requirements may vary a little depending on the update strategy
fn install_service_when_requirements_are_met(
    System(device): System<Device>,
    Target(tgt): Target<Service>,
    Args((app_uuid, rel_uuid, svc_name)): Args<(Uuid, Uuid, String)>,
) -> Option<Task> {
    // Skip the task if the image for the service doesn't exist yet
    // or there is an identically named service with the same image and
    // config. In the last scenario, the service will be created by the `uninstall_service`
    // operation
    if let ImageRef::Uri(tgt_img) = &tgt.image
        && device.images.contains_key(tgt_img)
        && find_installed_service(&device, &app_uuid, &rel_uuid, &svc_name)
            .is_none_or(|svc| svc.image.digest() != tgt.image.digest() || svc.config != tgt.config)
    {
        return Some(install_service.with_target(tgt));
    }

    None
}

/// Install the service
fn install_service(
    mut svc: View<Service>,
    Target(tgt): Target<Service>,
    Args((app_uuid, commit, svc_name)): Args<(Uuid, Uuid, String)>,
    docker: Res<Docker>,
    store: Res<Store>,
) -> IO<Service, Error> {
    enforce!(svc.container.is_none(), "service container already exists");

    // simulate a service install by creating a mock container
    // the mock will never be seen by users
    svc.container.replace(ServiceContainerSummary::mock());
    svc.started = false;
    svc.config = tgt.config;
    svc.container_name = tgt.container_name;
    with_io(svc, async move |mut svc| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        let local_store = store.as_ref().expect("store should be available");

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
                .create(&svc.container_name, &svc_img, container_config)
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

/// Start a service when all the requirements have been met
///
/// A service can be started if:
/// - The container has been created and is not already running
/// - If updating between releases, there is no equally named service from a previous release of the same app
/// - Any service dependencies have been started/running/healthy (TODO)
///
/// These requirements may vary a little depending on the update strategy
fn start_service_when_requirements_are_met(
    System(device): System<Device>,
    Target(tgt_svc): Target<Service>,
    Args((app_uuid, rel_uuid, svc_name)): Args<(Uuid, Uuid, String)>,
) -> Option<Task> {
    // only start the service if there are no services from a previous release
    if find_installed_service(&device, &app_uuid, &rel_uuid, &svc_name).is_none() {
        return Some(start_service.with_target(tgt_svc));
    }

    None
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

/// Rename the service container
fn rename_service_container(
    mut svc: View<Service>,
    Target(tgt): Target<Service>,
    docker: Res<Docker>,
) -> IO<Service, OciError> {
    let container_id = if let Some(id) = svc.container.as_ref().map(|c| c.id.clone())
        && svc.container_name != tgt.container_name
    {
        id
    } else {
        return IO::abort("service container should exist with a different name");
    };
    enforce!(
        svc.config == tgt.config,
        "service should already have the correct configuration"
    );

    // update the name
    svc.container_name = tgt.container_name;
    with_io(svc, async move |svc| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        docker
            .container()
            .rename(&container_id, &svc.container_name)
            .await
            .context("failed to rename container for service")?;

        Ok(svc)
    })
}

/// Change a service configuration by uninstalling and re-installing the service
fn reconfigure_service(svc: View<Service>, Target(tgt): Target<Service>) -> Vec<Task> {
    let mut tasks = Vec::new();
    if svc.config != tgt.config {
        tasks.push(stop_service_when_requirements_are_met.with_target(&tgt));
        tasks.push(remove_service_container.into_task());
        tasks.push(install_service_when_requirements_are_met.with_target(&tgt));
    }

    tasks
}

/// Stop a service and its dependents when all the requirements are met
///
/// A service can be stopped if:
/// - Locks are taken (TODO)
/// - Any services depending on it that have `restart: true` are stopped  (TODO)
fn stop_service_when_requirements_are_met() -> Vec<Task> {
    // just push the stop service task for now
    vec![stop_service.into_task()]
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

/// Stop a service if running and remove the service if requirements are met
///
/// A service can be uninstalled if
/// - Locks have been taken by this service (TODO)
/// - If upgrading between releases, all the target images have been pulled
///   has been created (TODO)
/// - If upgrading between releases and there is an identically named service in the new release:
///     - if the service has the same image and configuration, the old service should be migrated (TODO)
///     - otherwise the container for the new service should exist before uninstalling the old one (TODO)
///
/// These requirements may vary a little depending on the update strategy
fn uninstall_service_when_requirements_are_met(
    svc: View<Service>,
    System(device): System<Device>,
    SystemTarget(t_device): SystemTarget<Device>,
    Args((app_uuid, rel_uuid, svc_name)): Args<(Uuid, Uuid, String)>,
) -> Vec<Task> {
    let mut tasks = Vec::new();

    // wait until all target images have been downloaded
    if t_device.apps.values().any(|t_app| {
        t_app.releases.iter().any(|(t_rel_uuid, t_rel)| {
            t_rel_uuid != &rel_uuid
                && t_rel.services.values().any(|t_svc| {
                    !matches!(&t_svc.image, ImageRef::Uri(t_img_uri) if device.images.contains_key(t_img_uri))
                })
        })
    }) {
        return tasks;
    }

    // If there is a new target service for a different release with the same
    // config and image, then do a migration
    if let Some((t_rel_uuid, tgt_svc)) =
        find_future_service(&t_device, &app_uuid, &rel_uuid, &svc_name)
        && tgt_svc.image.digest() == svc.image.digest()
        && svc.config == tgt_svc.config
    {
        tasks.push(remove_service.into_task());
        tasks.push(
            migrate_service
                .with_arg("commit", t_rel_uuid.as_str())
                .with_target(&*svc),
        );
    } else {
        if svc
            .container
            .as_ref()
            .is_some_and(|c| c.status == ServiceContainerStatus::Running)
        {
            // stop the service if running
            tasks.push(stop_service_when_requirements_are_met.into_task());
        }
        // otherwise uninstall the service
        tasks.push(uninstall_service.into_task());
    }

    tasks
}

/// Remove service from the current release state
///
/// NOTE: this does not remove the container, is an in-memory only operation
/// that is used when migrating a service
fn remove_service(svc: View<Service>) -> View<Option<Service>> {
    svc.delete()
}

/// Remove a stopped service container
///
/// NOTE: This doesn't remove the service from the current state
fn remove_service_container(mut svc: View<Service>, docker: Res<Docker>) -> IO<Service, Error> {
    let container_id = if let Some(container) = svc.container.take()
        && container.status != ServiceContainerStatus::Running
    {
        container.id
    } else {
        return IO::abort("service container should exist and be stopped");
    };

    svc.started = false;
    with_io(svc, async move |svc| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        // remove the container
        docker
            .container()
            .remove(&container_id)
            .await
            .context("failed to remove container for service")?;

        Ok(svc)
    })
}

/// Remove a stopped service and its container
fn uninstall_service(mut svc: View<Service>, docker: Res<Docker>) -> IO<Option<Service>, Error> {
    let container_id = if let Some(container) = svc.container.take()
        && container.status != ServiceContainerStatus::Running
    {
        container.id
    } else {
        return IO::abort("service container should exist and be stopped");
    };

    let svc = svc.delete();
    with_io(svc, async move |svc| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        // remove the container
        docker
            .container()
            .remove(&container_id)
            .await
            .context("failed to remove container for service")?;

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
                job::update(install_service_when_requirements_are_met),
                job::none(install_service).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("install service '{service_name}' for release '{commit}'")
                    },
                ),
                job::update(start_service_when_requirements_are_met),
                job::none(start_service).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("start service '{service_name}' for release '{commit}'")
                    },
                ),
                job::none(stop_service_when_requirements_are_met),
                job::none(stop_service).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("stop service '{service_name}' for release '{commit}'")
                    },
                ),
                job::delete(uninstall_service_when_requirements_are_met),
                job::none(uninstall_service).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("uninstall service '{service_name}' for release '{commit}'")
                    },
                ),
                job::none(remove_service).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("remove data for '{service_name}' for release '{commit}'")
                    },
                ),
                job::none(migrate_service).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!("migrate service '{service_name}' to release '{commit}'")
                    },
                ),
                job::none(remove_service_container).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!(
                            "remove container for service '{service_name}' for release '{commit}'"
                        )
                    },
                ),
                job::update(reconfigure_service),
                job::update(rename_service_container).with_description(
                    |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                        format!(
                            "rename container for service '{service_name}' for release '{commit}'"
                        )
                    },
                ),
            ],
        )
        .job(
            "/apps/{app_uuid}/releases/{commit}/services/{service_name}/image",
            job::update(store_service_image_uri).with_description(
                |Args((_, commit, service_name)): Args<(Uuid, Uuid, String)>| {
                    format!(
                        "update image metadata for service '{service_name}' of release '{commit}'"
                    )
                },
            ),
        )
}
