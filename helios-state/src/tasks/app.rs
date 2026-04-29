use mahler::extract::{Args, RawTarget, Res, System, SystemTarget, Target, View};
use mahler::job;
use mahler::state::Map;
use mahler::task::prelude::*;
use mahler::worker::{Uninitialized, Worker};

use crate::common_types::{ImageUri, Uuid};
use crate::models::{
    App, AppMap, AppTarget, Container, ContainerStatus, Device, ImageRef, Network, Release,
    Service, ServiceTarget, Volume,
};
use crate::store::{self, DocumentStore};

use crate::oci::{Client as Docker, Error as OciError, WithContext};

use super::image::create_image;
use super::utils::{
    find_future_network, find_future_service, find_future_volume, find_installed_service,
};

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Store(#[from] store::Error),
    #[error(transparent)]
    Oci(#[from] OciError),
}

/// Initialize the app and store its local data
fn create_app(
    maybe_app: View<Option<App>>,
    Target(tgt_app): Target<App>,
    Args(app_uuid): Args<Uuid>,
    store: Res<DocumentStore>,
) -> IO<App, store::Error> {
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
            .put(format!("apps/{app_uuid}/id"), &app.id)
            .await?;
        local_store
            .put(format!("apps/{app_uuid}/name"), &app.name)
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
    store: Res<DocumentStore>,
) -> IO<u32, store::Error> {
    *id = tgt;
    with_io(id, async move |id| {
        let local_store = store.as_ref().expect("store should be available");
        local_store.put(format!("apps/{app_uuid}/id"), &*id).await?;
        Ok(id)
    })
}

/// Update the local app name
fn store_app_name(
    mut name: View<Option<String>>,
    Target(tgt): Target<Option<String>>,
    Args(app_uuid): Args<String>,
    store: Res<DocumentStore>,
) -> IO<Option<String>, store::Error> {
    *name = tgt;
    with_io(name, async move |name| {
        let local_store = store.as_ref().expect("store should be available");
        local_store
            .put(format!("apps/{app_uuid}/name"), &*name)
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
                            .is_none_or(|svc| svc.oci.is_some())
                        {
                            // the service already has a container, ignore
                            // the image
                            return None;
                        }

                        // only use the target image ref if it has not been downloaded yet
                        if let ImageRef::Uri(t_img_uri) = &t_svc.image
                            && !device.images.contains_key(t_img_uri)
                        {
                            Some(t_img_uri)
                        } else {
                            None
                        }
                    })
            })
        })
        // remove duplicate digests and tags
        .fold(Vec::<&ImageUri>::new(), |mut acc, svc_img| {
            if acc.iter().all(|img| {
                img != &svc_img && (img.digest().is_none() || img.digest() != svc_img.digest())
            }) {
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
        volumes: Map::new(),
    })
}

/// If modifying a release, make sure `release.installed` is set to false to
/// ensure the release gets finished afterwards
fn ensure_release_is_finalized(
    mut rel: View<Release>,
    Target(tgt_rel): Target<Release>,
) -> View<Release> {
    if tgt_rel.services.iter().any(|(svc_name, tgt_svc)| {
        rel.services
            .get(svc_name)
            .map(|svc| tgt_svc != &ServiceTarget::from(svc.clone()))
            // target service does not exist yet or has a different config
            .unwrap_or(true)
    }) || tgt_rel.networks.iter().any(|(net_name, tgt_net)| {
        rel.networks
            .get(net_name)
            .map(|net| tgt_net.config != net.config)
            // target network does not exist yet or has a different config
            .unwrap_or(true)
    }) || tgt_rel.volumes.iter().any(|(vol_name, tgt_vol)| {
        rel.volumes
            .get(vol_name)
            .map(|vol| tgt_vol.config != vol.config)
            // target volume does not exist yet or has a different config
            .unwrap_or(true)
    }) {
        // We only modify the release in memory to avoid writing to disk.
        // If something interrupts the update, services/network/volumes won't match
        // so this task will be executed again
        rel.installed = false;
    }

    rel
}

/// Finalize an installed release
fn finish_release(
    mut release: View<Release>,
    Target(target): Target<Release>,
    Args((app_uuid, commit)): Args<(Uuid, Uuid)>,
    store: Res<DocumentStore>,
) -> IO<Release, store::Error> {
    // all target services have been installed
    enforce!(
        target.services.iter().all(|(svc_name, tgt_svc)| {
            release
                .services
                .get(svc_name)
                .map(|svc| tgt_svc == &ServiceTarget::from(svc.clone()))
                .unwrap_or_default()
        }),
        "all services should have the correct configuration"
    );

    // all target networks have been created
    enforce!(
        target.networks.iter().all(|(net_name, tgt_net)| {
            release
                .networks
                .get(net_name)
                .map(|net| tgt_net.config == net.config)
                .unwrap_or_default()
        }),
        "all networks should have the correct configuration"
    );

    // all target volumes have been created
    enforce!(
        target.volumes.iter().all(|(vol_name, tgt_vol)| {
            release
                .volumes
                .get(vol_name)
                .map(|vol| tgt_vol.config == vol.config)
                .unwrap_or_default()
        }),
        "all volumes should have the correct configuration"
    );

    release.installed = true;
    with_io(release, async move |release| {
        // mark the release as installed on the local store
        let local_store = store.as_ref().expect("store should be available");
        local_store
            .put(
                format!("apps/{app_uuid}/releases/{commit}/installed"),
                &true,
            )
            .await?;

        Ok(release)
    })
}

/// Remove an empty release
fn remove_release(mut release: View<Option<Release>>) -> View<Option<Release>> {
    // remove the release if it has no services, no networks, and no volumes
    if release
        .as_ref()
        .map(|r| r.services.is_empty() && r.networks.is_empty() && r.volumes.is_empty())
        .unwrap_or_default()
    {
        release.take();
    }
    release
}

/// Create or migrate a network
///
/// If the network already exists in Docker with the same config, migrate it
/// from the previous release. If it doesn't exist, create it fresh. If it
/// exists with different config, skip and let the planner retry after the old
/// network is uninstalled.
fn create_network(
    net: View<Option<Network>>,
    Target(tgt): Target<Network>,
    docker: Res<Docker>,
    Args((app_uuid, _, net_name)): Args<(Uuid, Uuid, String)>,
) -> IO<Network, Error> {
    let net = net.create(Network {
        // use a mock name for planning only
        oci_name: String::default(),
        config: tgt.config,
    });

    with_io(net, async move |mut net| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        let network_config = std::mem::take(&mut net.config).into_oci_config(&net_name);

        // create the network namespaced by app-uuid
        let network_name = docker
            .network()
            .create(&net_name, app_uuid.as_str(), network_config)
            .await?;

        let local_network = docker
            .network()
            .inspect(&network_name)
            .await
            .with_context(|| format!("failed to inspect network '{network_name}'"))?;
        *net = Network::from(local_network);

        Ok(net)
    })
}

/// Reconfigure a network by uninstalling it when the config has changed
///
/// After uninstall, the planner will re-create the network with the new config.
fn reconfigure_network(
    net: View<Network>,
    System(device): System<Device>,
    Target(tgt): Target<Network>,
    Args((app_uuid, _, net_name)): Args<(Uuid, Uuid, String)>,
) -> Vec<Task> {
    if net.config != tgt.config {
        let mut tasks = Vec::new();
        let services_depending_on_network = device
            .apps
            .get(&app_uuid)
            .map(|app| {
                app.releases
                    .iter()
                    .flat_map(|(rel_uuid, rel)| {
                        rel.services
                            .iter()
                            // find any services referencing the network that have a container
                            .filter(|(_, svc)| {
                                svc.oci.is_some() && svc.config.networks.contains_key(&net_name)
                            })
                            .map(move |(svc_name, _)| (rel_uuid, svc_name))
                    })
                    .collect::<Vec<(&Uuid, &String)>>()
            })
            .unwrap_or_default();

        // uninstall any services depending on the network first
        for (rel_uuid, svc_name) in services_depending_on_network {
            tasks.push(
                stop_service_when_requirements_are_met
                    .with_arg("commit", rel_uuid.as_str())
                    .with_arg("service_name", svc_name),
            );
            tasks.push(
                remove_service_container
                    .with_arg("commit", rel_uuid.as_str())
                    .with_arg("service_name", svc_name),
            );
        }

        if tasks.is_empty() {
            tasks.push(uninstall_network.into_task());
        }

        tasks
    } else {
        Vec::new()
    }
}

/// Uninstall a network from Docker and the state tree
fn uninstall_network(net: View<Network>, docker: Res<Docker>) -> IO<Option<Network>, Error> {
    let docker_name = net.oci_name.clone();
    let net = net.delete();

    with_io(net, async move |net| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        docker.network().remove(&docker_name).await?;
        Ok(net)
    })
}

/// Create or migrate a volume
///
/// If the volume already exists in Docker with the same config, migrate it
/// from the previous release. If it doesn't exist, create it fresh. If it
/// exists with different config, skip and let the planner retry after the old
/// volume is uninstalled.
fn create_volume(
    vol: View<Option<Volume>>,
    Target(tgt): Target<Volume>,
    Args((app_uuid, _, vol_name)): Args<(Uuid, Uuid, String)>,
    docker: Res<Docker>,
) -> IO<Volume, Error> {
    let vol = vol.create(Volume {
        // use a mock name for planning only
        oci_name: String::new(),
        config: tgt.config,
    });

    with_io(vol, async move |mut vol| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        let volume_config = std::mem::take(&mut vol.config).into_oci_config(&vol_name);

        // create the volume namespaced by app_uuid
        let volume_name = docker
            .volume()
            .create(&vol_name, app_uuid.as_str(), volume_config)
            .await?;

        let local_volume = docker
            .volume()
            .inspect(&volume_name)
            .await
            .with_context(|| format!("failed to inspect volume '{volume_name}'"))?;
        *vol = Volume::from(local_volume);

        Ok(vol)
    })
}

/// Reconfigure a volume by uninstalling it when the config has changed
///
/// After uninstall, the planner will re-create the volume with the new config.
fn reconfigure_volume(vol: View<Volume>, Target(tgt): Target<Volume>) -> Option<Task> {
    if vol.config != tgt.config {
        Some(uninstall_volume.into_task())
    } else {
        None
    }
}

/// Uninstall a volume from Docker and the state tree
fn uninstall_volume(vol: View<Volume>, docker: Res<Docker>) -> IO<Option<Volume>, Error> {
    let docker_name = vol.oci_name.clone();
    let vol = vol.delete();

    with_io(vol, async move |vol| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        docker.volume().remove(&docker_name).await?;
        Ok(vol)
    })
}

/// Migrate or remove a network
///
/// If the same-named network exists in the target with identical config,
/// perform a state-only removal and migration of data. Otherwise do a full
/// Docker removal.
fn remove_network_when_requirements_are_met(
    net: View<Network>,
    System(device): System<Device>,
    SystemTarget(t_device): SystemTarget<Device>,
    Args((app_uuid, rel_uuid, net_name)): Args<(Uuid, Uuid, String)>,
) -> Option<Task> {
    if let Some((t_rel_uuid, future_net)) =
        find_future_network(&t_device, &app_uuid, &rel_uuid, &net_name)
        && net.config == future_net.config
    {
        // Wait until the target release's network has been created in state
        // before emitting migration tasks
        let new_net_exists = device
            .apps
            .get(&app_uuid)
            .and_then(|app| app.releases.get(t_rel_uuid))
            .is_some_and(|rel| rel.networks.contains_key(&net_name));

        if !new_net_exists {
            return None;
        }

        // State-only removal, Docker network preserved for new release to adopt
        Some(remove_network.into_task())
    } else {
        let services_depend_on_network = device.apps.get(&app_uuid).is_some_and(|app| {
            app.releases.values().any(|rel| {
                rel.services
                    .values()
                    .any(|svc| svc.config.networks.contains_key(&net_name))
            })
        });

        // Cannot uninstall a network that has dependent services on any installed release
        if !services_depend_on_network {
            Some(uninstall_network.into_task())
        } else {
            None
        }
    }
}

/// Remove network from the current release state
fn remove_network(net: View<Network>) -> View<Option<Network>> {
    net.delete()
}

/// Migrate or remove a volume
///
/// If the same-named volume exists in the target with identical config,
/// perform a state-only removal and migration of data. Otherwise do a full
/// Docker removal.
fn remove_volume_when_requirements_are_met(
    vol: View<Volume>,
    System(device): System<Device>,
    SystemTarget(t_device): SystemTarget<Device>,
    Args((app_uuid, rel_uuid, vol_name)): Args<(Uuid, Uuid, String)>,
) -> Option<Task> {
    if let Some((t_rel_uuid, future_vol)) =
        find_future_volume(&t_device, &app_uuid, &rel_uuid, &vol_name)
        && vol.config == future_vol.config
    {
        // Wait until the target release's volume has been created in state
        // before emitting migration tasks
        let new_vol_exists = device
            .apps
            .get(&app_uuid)
            .and_then(|app| app.releases.get(t_rel_uuid))
            .is_some_and(|rel| rel.volumes.contains_key(&vol_name));

        if !new_vol_exists {
            return None;
        }

        // State-only removal, Docker volume preserved for new release to adopt
        Some(remove_volume.into_task())
    } else {
        Some(uninstall_volume.into_task())
    }
}

/// Remove volume from the current release state
fn remove_volume(vol: View<Volume>) -> View<Option<Volume>> {
    vol.delete()
}

/// Create the service in memory before initiating download
fn create_service(maybe_svc: View<Option<Service>>, Target(tgt): Target<Service>) -> View<Service> {
    let ServiceTarget {
        id, image, config, ..
    } = tgt;
    maybe_svc.create(Service {
        id,
        image,
        installing: false,
        started: false,
        oci: None,
        config,
    })
}

/// Migrate a service to the current release from another location
fn migrate_service(
    maybe_svc: View<Option<Service>>,
    RawTarget(t_svc): RawTarget<Service>,
    Args((_, rel_uuid, svc_name)): Args<(Uuid, Uuid, String)>,
    docker: Res<Docker>,
) -> IO<Service, Error> {
    enforce!(t_svc.oci.is_some(), "source service must have a container");
    let svc = maybe_svc.create(t_svc);
    with_io(svc, async move |mut svc| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        let container = svc.oci.as_ref().expect("container must be available");
        let container_id = docker
            .container()
            .migrate(&container.name, &svc_name, rel_uuid.as_str())
            .await?;

        // check that the container was created and generate the Service configuration
        // from the image config and container info
        let local_container = docker
            .container()
            .inspect(&container_id)
            .await
            .context("failed to inspect container for service")?;
        *svc = Service::from(local_container);

        Ok(svc)
    })
}

/// Install the service when requirements are met for this operation
///
/// A service can be installed after
/// - the image has been pulled
/// - any networks referenced by the service exist in the release and have been created
/// - any volumes referenced by the service exist in the release and have been created
/// - if upgrading between releases, and there is an identically named service in
///   a previous release, the services are for different images and have different
///   configurations, otherwise the old service just requires as migration
///
/// These requirements may vary a little depending on the update strategy
fn install_service_when_requirements_are_met(
    svc: View<Service>,
    System(device): System<Device>,
    SystemTarget(tgt_device): SystemTarget<Device>,
    Target(tgt): Target<Service>,
    Args((app_uuid, rel_uuid, svc_name)): Args<(Uuid, Uuid, String)>,
) -> Option<Task> {
    // do not install a container that already exists
    if svc.oci.is_some() {
        return None;
    }

    // Check that all required networks have been created in the release
    let networks_ready = if !tgt.config.networks.is_empty() {
        let release = device
            .apps
            .get(&app_uuid)
            .and_then(|app| app.releases.get(&rel_uuid));

        let t_release = tgt_device
            .apps
            .get(&app_uuid)
            .and_then(|app| app.releases.get(&rel_uuid));

        release.is_some_and(|release| {
            // for every configured network
            tgt.config.networks.keys().all(|net_name| {
                // the network has been created
                if release.networks.contains_key(net_name)
                    && let Some(net) = release.networks.get(net_name)
                    && let Some(t_net) = t_release.and_then(|t_rel| t_rel.networks.get(net_name))
                    // and the installed network configuration matches its target
                    && net.config == t_net.config
                {
                    return true;
                }
                false
            })
        })
    } else {
        true
    };

    // Skip the task if the image for the service doesn't exist yet
    // or there is an identically named service with the same image and
    // config. In the last scenario, the service will be created by the `uninstall_service`
    // operation
    if networks_ready
        && let ImageRef::Uri(tgt_img) = &tgt.image
        && device.images.contains_key(tgt_img)
        && find_installed_service(&device, &app_uuid, &rel_uuid, &svc_name)
            .is_none_or(|svc| !svc.image.is_same_artifact(&tgt.image) || svc.config != tgt.config)
    {
        return Some(install_service.with_target(tgt));
    }

    None
}

/// Install the service
fn install_service(
    mut svc: View<Service>,
    Target(tgt): Target<Service>,
    Args((app_uuid, rel_uuid, svc_name)): Args<(Uuid, Uuid, String)>,
    docker: Res<Docker>,
    store: Res<DocumentStore>,
) -> IO<Service, Error> {
    enforce!(svc.oci.is_none(), "service container already exists");

    // simulate a service install by creating a mock container
    // the mock will never be seen by users
    svc.oci.replace(Container::mock());
    svc.started = false;
    svc.config = tgt.config;
    with_io(svc, async move |mut svc| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");
        let local_store = store.as_ref().expect("store should be available");
        svc.installing = true;
        let _ = svc.flush().await;

        let mut container_config =
            std::mem::take(&mut svc.config).into_oci_config(svc.id, &svc_name, &app_uuid);

        // Extract networks to connect later
        let mut networks = std::mem::take(&mut container_config.networks);

        // remove only the first network so it can be used as the main container network,
        // the rest of the networks will be configured via connect() to ensure priority order
        // is preserved
        if let Some((net_name, net_config)) = networks.shift_remove_index(0) {
            container_config.networks.insert(net_name, net_config);
        }

        // create the container namespaced by release uuid
        let container_id = docker
            .container()
            .create(&svc_name, rel_uuid.as_str(), &svc.image, container_config)
            .await?;

        // Connect the container to each network in priority order
        for (net_name, endpoint_config) in networks {
            let endpoint_settings = endpoint_config.into();
            docker
                .network()
                .connect(&net_name, &container_id, endpoint_settings)
                .await?;
        }

        // check that the container was created and generate the Service configuration
        // from the image config and container info
        let local_container = docker
            .container()
            .inspect(&container_id)
            .await
            .context("failed to inspect container for service")?;
        *svc = Service::from(local_container);
        svc.image = tgt.image;

        // store the image uri that corresponds to the current release service
        local_store
            .put(
                format!("apps/{app_uuid}/releases/{rel_uuid}/services/{svc_name}/image"),
                &svc.image,
            )
            .await?;

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
        svc.oci
            .as_ref()
            .is_some_and(|c| c.status != ContainerStatus::Running),
        "service container should exist and should not be running"
    );

    // creating the container will not fail if the container already exists, however
    // that doesn't guarantee the configuration will be the same. In that case we'll
    // need to loop again to re-create the container
    enforce!(
        svc.config == tgt_svc.config,
        "service configuration should match the target before start: {:?}, {:?}",
        svc.config,
        tgt_svc.config
    );

    svc.started = true;
    with_io(svc, async move |mut svc| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        // this is guaranteed by the enforce above
        let container_id = svc
            .oci
            .as_ref()
            .map(|c| &c.name)
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

        svc.oci.replace(Container::from((
            local_container.name.as_ref(),
            local_container.state,
        )));

        Ok(svc)
    })
}

/// Change a service configuration by uninstalling and re-installing the service
fn reconfigure_service(svc: View<Service>, Target(tgt): Target<Service>) -> Vec<Task> {
    let mut tasks = Vec::new();
    if svc.config != tgt.config {
        if let Some(container) = svc.oci.as_ref()
            && container.status == ContainerStatus::Running
        {
            tasks.push(stop_service_when_requirements_are_met.with_target(&tgt));
        }
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
    let container_id = if let Some(container) = svc.oci.as_mut()
        && container.status == ContainerStatus::Running
    {
        container.status = ContainerStatus::Stopped;
        container.name.clone()
    } else {
        return IO::abort("service container should exist and should be running");
    };

    with_io(svc, async move |mut svc| {
        let docker = docker
            .as_ref()
            .expect("docker resource should be available");

        if let Some(container) = svc.oci.as_mut() {
            // set the container status before stopping
            container.status = ContainerStatus::Stopping;
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

        svc.oci.replace(Container::from((
            local_container.name.as_ref(),
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
        && svc.image.is_same_artifact(&tgt_svc.image)
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
            .oci
            .as_ref()
            .is_some_and(|c| c.status == ContainerStatus::Running)
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
    let container_id = if let Some(container) = svc.oci.take()
        && container.status != ContainerStatus::Running
    {
        container.name
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
    let container_id = if let Some(container) = svc.oci.take()
        && container.status != ContainerStatus::Running
    {
        container.name
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
    store: Res<DocumentStore>,
) -> IO<ImageRef, store::Error> {
    *img = ImageRef::Uri(tgt);

    with_io(img, async move |img| {
        let local_store = store.as_ref().expect("store should be available");
        // store the image uri that corresponds to the current release service
        local_store
            .put(
                format!("apps/{app_uuid}/releases/{commit}/services/{svc_name}/image"),
                &*img,
            )
            .await?;
        Ok(img)
    })
}

/// Update worker with user app tasks
pub fn with_userapp_tasks<O>(worker: Worker<O, Uninitialized>) -> Worker<O, Uninitialized> {
    worker
        .jobs("/apps", [job::update(fetch_apps_images)])
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
                job::update(ensure_release_is_finalized).with_description(
                    |Args((uuid, commit)): Args<(Uuid, Uuid)>| {
                        format!("prepare release '{commit}' for app with uuid '{uuid}'")
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
                        format!("setup network '{network_name}' for app '{app_uuid}'")
                    },
                ),
                job::update(reconfigure_network),
                job::none(uninstall_network).with_description(
                    |Args((app_uuid, _, network_name)): Args<(Uuid, Uuid, String)>| {
                        format!("remove network '{network_name}' for app '{app_uuid}'")
                    },
                ),
                job::delete(remove_network_when_requirements_are_met),
                job::none(remove_network).with_description(
                    |Args((_, commit, network_name)): Args<(Uuid, Uuid, String)>| {
                        format!("remove data for network '{network_name}' from release '{commit}'")
                    },
                ),
            ],
        )
        .jobs(
            "/apps/{app_uuid}/releases/{commit}/volumes/{volume_name}",
            [
                job::create(create_volume).with_description(
                    |Args((app_uuid, _, volume_name)): Args<(Uuid, Uuid, String)>| {
                        format!("setup volume '{volume_name}' for app '{app_uuid}'")
                    },
                ),
                job::update(reconfigure_volume),
                job::none(uninstall_volume).with_description(
                    |Args((app_uuid, _, volume_name)): Args<(Uuid, Uuid, String)>| {
                        format!("remove volume '{volume_name}' for app '{app_uuid}'")
                    },
                ),
                job::delete(remove_volume_when_requirements_are_met),
                job::none(remove_volume).with_description(
                    |Args((_, commit, volume_name)): Args<(Uuid, Uuid, String)>| {
                        format!("remove data for volume '{volume_name}' from release '{commit}'")
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
