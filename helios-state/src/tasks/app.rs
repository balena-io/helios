use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use mahler::extract::{Args, RawTarget, Res, System, SystemTarget, Target, View};
use mahler::job;
use mahler::state::Map;
use mahler::task::prelude::*;
use mahler::worker::{Uninitialized, Worker};
use tracing::warn;

use crate::common_types::{HostRuntimeDir, ImageUri, Uuid};
use crate::models::{
    App, AppMap, AppTarget, Container, ContainerStatus, Device, ImageRef, Network, Release,
    ReleaseTarget, Service, ServiceTarget, Volume,
};
use crate::oci::{Client as Docker, Error as OciError, Mount, WithContext};
use crate::store::{self, DocumentStore};
use crate::util::dirs::runtime_dir;
use crate::util::fs::run_async;
use crate::util::locking::{self, ForceAcquireLocks, LockSet};

use super::helpers::{
    any_images_are_pending_download, find_future_network, find_future_service, find_future_volume,
    find_installed_network, find_installed_service, find_installed_volume, service_matches_target,
    services_need_stopping,
};
use super::image::create_image;

/// Maximum number of images to pull concurrently per planning cycle.
const MAX_CONCURRENT_IMAGE_PULLS: usize = 3;

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
        locked: false,
        lockfiles: Vec::new(),
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
        .map(|a| !a.locked && a.releases.is_empty())
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

/// If a target service is staged (exists in state without a container yet)
/// and its image has not been pulled, return the URI to pull. Otherwise None.
fn service_needs_image_download<'a>(
    device: &Device,
    app_uuid: &Uuid,
    rel_uuid: &Uuid,
    svc_name: &str,
    tgt_svc: &'a ServiceTarget,
) -> Option<&'a ImageUri> {
    // service must exist in current state without a container yet
    let svc = device
        .apps
        .get(app_uuid)
        .and_then(|app| app.releases.get(rel_uuid))
        .and_then(|rel| rel.services.get(svc_name))?;
    if svc.oci.is_some() {
        return None;
    }

    // image must be a URI not yet pulled
    let ImageRef::Uri(uri) = &tgt_svc.image else {
        return None;
    };
    if device.images.contains_key(uri) {
        return None;
    }
    Some(uri)
}

/// True if `candidate` is already represented in `queued` — either by full
/// URI equality or by a matching non-empty digest. Two URIs sharing a digest
/// resolve to the same content, so pulling either is sufficient.
fn already_queued_for_pull(queued: &[&ImageUri], candidate: &ImageUri) -> bool {
    queued.iter().any(|img| {
        *img == candidate || (img.digest().is_some() && img.digest() == candidate.digest())
    })
}

/// Install all new images for all target apps
fn fetch_apps_images(
    System(device): System<Device>,
    Target(tgt_apps): Target<AppMap>,
) -> Vec<Task> {
    let images_to_install = tgt_apps
        .iter()
        .flat_map(|(app_uuid, t_app)| {
            t_app.releases.iter().flat_map(|(rel_uuid, t_rel)| {
                t_rel.services.iter().filter_map(|(svc_name, t_svc)| {
                    service_needs_image_download(&device, app_uuid, rel_uuid, svc_name, t_svc)
                })
            })
        })
        .fold(Vec::<&ImageUri>::new(), |mut acc, candidate| {
            if !already_queued_for_pull(&acc, candidate) {
                acc.push(candidate);
            }
            acc
        });

    images_to_install
        .into_iter()
        .take(MAX_CONCURRENT_IMAGE_PULLS)
        .map(|image| create_image.with_arg("image_name", image.clone()))
        .collect()
}

/// Take locks for the running app once all target services have been installed
fn take_locks(
    mut app: View<App>,
    host_runtime_dir: Res<HostRuntimeDir>,
    locks: Res<LockSet>,
    force_acquire_locks: Res<ForceAcquireLocks>,
) -> IO<App, io::Error> {
    app.locked = true;
    with_io(app, async move |mut app| {
        let host_runtime_dir = host_runtime_dir
            .as_ref()
            .expect("host_runtime_dir resource should be available");
        let force_acquire_locks = force_acquire_locks
            .as_ref()
            .expect("force_acquire_locks should be available")
            .enabled();

        // resolve a lock file for each running service whose /tmp/balena bind
        // mount lives under the host runtime directory, grouping the services
        // that share each lock path so each lock is taken at most once.
        let service_locks: HashMap<PathBuf, Vec<String>> = app
            .releases
            .iter()
            .flat_map(|(_, rel)| rel.services.iter())
            .filter(|(_, svc)| svc.oci.is_some())
            .filter_map(|(svc_name, svc)| {
                let bind_source = svc.config.volumes.iter().find_map(|mount| match mount {
                    Mount::Bind { source, target, .. } if target == "/tmp/balena" => Some(source),
                    _ => None,
                })?;
                let suffix = Path::new(bind_source)
                    .strip_prefix(host_runtime_dir.as_path())
                    .ok()?;
                Some((
                    runtime_dir().join(suffix).join("updates.lock"),
                    svc_name.clone(),
                ))
            })
            .fold(HashMap::new(), |mut acc, (path, svc_name)| {
                acc.entry(path).or_default().push(svc_name);
                acc
            });

        if force_acquire_locks && !service_locks.is_empty() {
            warn!("taking locks by force per-user request");
        }

        let acquired = run_async(move || {
            let locks = locks.as_ref().expect("locks resource should be available");
            let mut acquired: Vec<PathBuf> = Vec::with_capacity(service_locks.len());
            for (lock_path, svc_names) in service_locks {
                if let Err(e) = locks.try_lock(lock_path.clone(), force_acquire_locks) {
                    for p in acquired {
                        let _ = locks.unlock(p);
                    }
                    return match e {
                        locking::Error::WouldBlock => Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            format!("services locked: {}", svc_names.join(", ")),
                        )),
                        locking::Error::IO(e) => Err(e),
                    };
                }
                acquired.push(lock_path);
            }
            Ok(acquired)
        })
        .await?;

        app.lockfiles = acquired;
        Ok(app)
    })
}

/// Release locks for the running app once all current services have been removed
fn release_locks(
    mut app: View<App>,
    Target(tgt_app): Target<Option<App>>,
    locks: Res<LockSet>,
) -> IO<App, io::Error> {
    // if there is no target app, wait until releases are deleted
    enforce!(tgt_app.is_some() || app.releases.is_empty());

    // if there is a target app with a target release, wait until the release is installed
    if let Some((t_rel_uuid, _)) = tgt_app
        .as_ref()
        .and_then(|t_app| t_app.releases.first_key_value())
        && app
            .releases
            .get(t_rel_uuid)
            .is_none_or(|rel| !rel.installed)
    {
        return IO::abort(format!("target release {t_rel_uuid} is not installed"));
    }

    app.locked = false;
    with_io(app, async move |mut app| {
        let to_release = std::mem::take(&mut app.lockfiles);
        run_async(move || {
            let locks = locks.as_ref().expect("locks resource should be available");
            for path in to_release {
                locks.unlock(path)?;
            }
            Ok(())
        })
        .await?;

        Ok(app)
    })
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

/// True if any target service is missing in the current release or has a
/// different image, config, or started state.
fn any_service_differs(rel: &Release, tgt_rel: &ReleaseTarget) -> bool {
    tgt_rel.services.iter().any(|(name, tgt_svc)| {
        rel.services.get(name).is_none_or(|svc| {
            svc.started != tgt_svc.started
                || svc.image != tgt_svc.image
                || svc.config != tgt_svc.config
        })
    })
}

/// True if any target network is missing in the current release or has a
/// different config.
fn any_network_differs(rel: &Release, tgt_rel: &ReleaseTarget) -> bool {
    tgt_rel.networks.iter().any(|(name, tgt_net)| {
        rel.networks
            .get(name)
            .is_none_or(|net| net.config != tgt_net.config)
    })
}

/// True if any target volume is missing in the current release or has a
/// different config.
fn any_volume_differs(rel: &Release, tgt_rel: &ReleaseTarget) -> bool {
    tgt_rel.volumes.iter().any(|(name, tgt_vol)| {
        rel.volumes
            .get(name)
            .is_none_or(|vol| vol.config != tgt_vol.config)
    })
}

/// If modifying a release, make sure `release.installed` is set to false to
/// ensure the release gets finished afterwards
fn ensure_release_is_finalized(
    mut rel: View<Release>,
    Target(tgt_rel): Target<Release>,
) -> View<Release> {
    if any_service_differs(&rel, &tgt_rel)
        || any_network_differs(&rel, &tgt_rel)
        || any_volume_differs(&rel, &tgt_rel)
    {
        // We only modify the release in memory to avoid writing to disk.
        // If something interrupts the update, services/network/volumes won't match
        // so this task will be executed again
        rel.installed = false;
    }

    rel
}

/// Finalize an installed release
fn finish_release(
    mut rel: View<Release>,
    Target(t_rel): Target<Release>,
    Args((app_uuid, rel_uuid)): Args<(Uuid, Uuid)>,
    store: Res<DocumentStore>,
) -> IO<Release, store::Error> {
    enforce!(
        !any_service_differs(&rel, &t_rel),
        "all services should have the correct configuration"
    );
    enforce!(
        !any_network_differs(&rel, &t_rel),
        "all networks should have the correct configuration"
    );
    enforce!(
        !any_volume_differs(&rel, &t_rel),
        "all volumes should have the correct configuration"
    );

    rel.installed = true;
    with_io(rel, async move |rel| {
        // mark the release as installed on the local store
        let local_store = store.as_ref().expect("store should be available");
        local_store
            .put(
                format!("apps/{app_uuid}/releases/{rel_uuid}/installed"),
                &true,
            )
            .await?;

        Ok(rel)
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

fn create_network_when_requirements_are_met(
    System(device): System<Device>,
    Target(tgt): Target<Network>,
    Args((app_uuid, rel_uuid, net_name)): Args<(Uuid, Uuid, String)>,
) -> Option<Task> {
    // Do not create a new network if there is an installed network from a different release
    // as that network needs to be removed first
    if let Some(cur_net) = find_installed_network(&device, &app_uuid, &rel_uuid, &net_name)
        && tgt.config != cur_net.config
    {
        return None;
    }

    Some(create_network.into_task())
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
fn reconfigure_network(net: View<Network>, Target(tgt): Target<Network>) -> Option<Task> {
    if net.config != tgt.config {
        return Some(remove_network_when_requirements_are_met.into_task());
    }
    None
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

fn create_volume_when_requirements_are_met(
    System(device): System<Device>,
    Target(tgt): Target<Volume>,
    Args((app_uuid, rel_uuid, vol_name)): Args<(Uuid, Uuid, String)>,
) -> Option<Task> {
    // Do not create a new volume if there is an installed volume from a different release
    // as that volume needs to be removed first
    if let Some(cur_vol) = find_installed_volume(&device, &app_uuid, &rel_uuid, &vol_name)
        && tgt.config != cur_vol.config
    {
        return None;
    }

    Some(create_volume.into_task())
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
        return Some(remove_volume_when_requirements_are_met.into_task());
    }
    None
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

/// For every running service in `app_uuid` that depends on a resource (per
/// `depends_on`), emit a stop+uninstall task pair. Returns an empty Vec when
/// no services depend on the resource — callers use this to know it's safe
/// to remove the resource itself.
fn uninstall_services_depending_on(
    device: &Device,
    app_uuid: &Uuid,
    depends_on: impl Fn(&Service) -> bool,
) -> Vec<Task> {
    let Some(app) = device.apps.get(app_uuid) else {
        return Vec::new();
    };

    app.releases
        .iter()
        .flat_map(|(rel_uuid, rel)| {
            rel.services
                .iter()
                .filter(|(_, svc)| svc.oci.is_some() && depends_on(svc))
                .flat_map(move |(svc_name, _)| {
                    [
                        stop_service_when_requirements_are_met
                            .with_arg("commit", rel_uuid.as_str())
                            .with_arg("service_name", svc_name),
                        uninstall_service
                            .with_arg("commit", rel_uuid.as_str())
                            .with_arg("service_name", svc_name),
                    ]
                })
        })
        .collect()
}

/// Migrate or remove a network
///
/// If the same-named network exists in the target with identical config,
/// perform a state-only removal so the Docker network is reused. Otherwise
/// stop and uninstall any services using the network, then remove it.
fn remove_network_when_requirements_are_met(
    net: View<Network>,
    System(device): System<Device>,
    SystemTarget(t_device): SystemTarget<Device>,
    Args((app_uuid, rel_uuid, net_name)): Args<(Uuid, Uuid, String)>,
) -> Vec<Task> {
    // Migration path: same-named network in a future release with matching
    // config. Wait for the new release to register the network in state,
    // then perform a state-only removal — the Docker network is preserved
    // for the new release to adopt.
    if let Some((t_rel_uuid, future_net)) =
        find_future_network(&t_device, &app_uuid, &rel_uuid, &net_name)
        && net.config == future_net.config
    {
        let new_release_has_network = device
            .apps
            .get(&app_uuid)
            .and_then(|app| app.releases.get(t_rel_uuid))
            .is_some_and(|rel| rel.networks.contains_key(&net_name));

        return if new_release_has_network {
            vec![remove_network.into_task()]
        } else {
            Vec::new()
        };
    }

    // Otherwise: stop and uninstall any services using the network, then
    // remove it on a subsequent retry once no dependents remain.
    let mut tasks = uninstall_services_depending_on(&device, &app_uuid, |svc| {
        svc.config.networks.contains_key(&net_name)
    });
    if tasks.is_empty() {
        tasks.push(uninstall_network.into_task());
    }
    tasks
}

/// Remove network from the current release state
fn remove_network(net: View<Network>) -> View<Option<Network>> {
    net.delete()
}

/// Migrate or remove a volume
///
/// If the same-named volume exists in the target with identical config,
/// perform a state-only removal so the Docker volume is reused. Otherwise
/// stop and uninstall any services using the volume, then remove it.
fn remove_volume_when_requirements_are_met(
    vol: View<Volume>,
    System(device): System<Device>,
    SystemTarget(t_device): SystemTarget<Device>,
    Args((app_uuid, rel_uuid, vol_name)): Args<(Uuid, Uuid, String)>,
) -> Vec<Task> {
    // Migration path: same-named volume in a future release with matching
    // config. Wait for the new release to register the volume in state,
    // then perform a state-only removal — the Docker volume is preserved
    // for the new release to adopt.
    if let Some((t_rel_uuid, future_vol)) =
        find_future_volume(&t_device, &app_uuid, &rel_uuid, &vol_name)
        && vol.config == future_vol.config
    {
        let new_release_has_volume = device
            .apps
            .get(&app_uuid)
            .and_then(|app| app.releases.get(t_rel_uuid))
            .is_some_and(|rel| rel.volumes.contains_key(&vol_name));

        return if new_release_has_volume {
            vec![remove_volume.into_task()]
        } else {
            Vec::new()
        };
    }

    // Otherwise: stop and uninstall any services mounting the volume, then
    // remove it on a subsequent retry once no dependents remain.
    let mut tasks = uninstall_services_depending_on(&device, &app_uuid, |svc| {
        svc.config
            .volumes
            .iter()
            .any(|m| matches!(m, Mount::Volume { source, .. } if source == &vol_name))
    });
    if tasks.is_empty() {
        tasks.push(uninstall_volume.into_task());
    }
    tasks
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

        let tgt_image = svc.image.clone();

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
        // preserve the target image URI, this prevents the
        // engine image sha from being used spuriously during comparison
        svc.image = tgt_image;

        Ok(svc)
    })
}

/// Emit `install_service` once the service is ready to install:
/// - the service is registered in state but has no container yet,
/// - the image has been pulled,
/// - every linked network and volume exists in the current release with
///   config matching the target, and
/// - no identically-named service in another release could be migrated
///   here instead (that path is handled by `uninstall_service_when_requirements_are_met`).
fn install_service_when_requirements_are_met(
    svc: View<Service>,
    System(device): System<Device>,
    SystemTarget(tgt_device): SystemTarget<Device>,
    Target(tgt): Target<Service>,
    Args((app_uuid, rel_uuid, svc_name)): Args<(Uuid, Uuid, String)>,
) -> Option<Task> {
    if svc.oci.is_some() {
        return None;
    }

    let release = device
        .apps
        .get(&app_uuid)
        .and_then(|app| app.releases.get(&rel_uuid));
    let t_release = tgt_device
        .apps
        .get(&app_uuid)
        .and_then(|app| app.releases.get(&rel_uuid));

    // the service image has already been pulled
    let ImageRef::Uri(tgt_img) = &tgt.image else {
        return None;
    };
    let image_pulled = device.images.contains_key(tgt_img);

    // A linked resource is "ready" if it exists in the current release with
    // config matching its target.
    let network_ready = |name: &String| -> bool {
        matches!(
            (
                release.and_then(|r| r.networks.get(name)),
                t_release.and_then(|r| r.networks.get(name)),
            ),
            (Some(net), Some(t_net)) if net.config == t_net.config,
        )
    };
    let volume_ready = |name: &String| -> bool {
        matches!(
            (
                release.and_then(|r| r.volumes.get(name)),
                t_release.and_then(|r| r.volumes.get(name)),
            ),
            (Some(vol), Some(t_vol)) if vol.config == t_vol.config,
        )
    };

    let networks_ready = tgt.config.networks.keys().all(network_ready);
    let volumes_ready = tgt.config.volumes.iter().all(|m| match m {
        Mount::Volume { source, .. } => volume_ready(source),
        _ => true,
    });

    // If an identically-named service exists in another release matching
    // image/started/config, the migration path in `uninstall_service_when_requirements_are_met`
    // will adopt it — skip a fresh install here.
    let no_migratable_predecessor =
        find_installed_service(&device, &app_uuid, &rel_uuid, &svc_name).is_none_or(|prev| {
            !prev.image.is_same_artifact(&tgt.image)
                || prev.started != tgt.started
                || prev.config != tgt.config
        });

    if networks_ready && volumes_ready && image_pulled && no_migratable_predecessor {
        Some(install_service.with_target(tgt))
    } else {
        None
    }
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
    SystemTarget(t_device): SystemTarget<Device>,
    Target(tgt_svc): Target<Service>,
    Args((app_uuid, rel_uuid, svc_name)): Args<(Uuid, Uuid, String)>,
) -> Option<Task> {
    // only start the service if
    // no services need stopping or the app is already locked
    if (!services_need_stopping(&app_uuid, &device, &t_device)
        || device.apps.get(&app_uuid).is_some_and(|app| app.locked))
        // and any service from a previous release has already been installed
        && find_installed_service(&device, &app_uuid, &rel_uuid, &svc_name).is_none()
    {
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
        "service configuration should match the target before start",
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

        *svc = Service::from(local_container);
        svc.image = tgt_svc.image;

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
/// - Locks are taken
/// - Any services depending on it that have `restart: true` are stopped  (TODO)
fn stop_service_when_requirements_are_met(
    System(device): System<Device>,
    Args((app_uuid, _, _)): Args<(Uuid, Uuid, String)>,
) -> Vec<Task> {
    let mut tasks = Vec::new();
    // the service cannot be stopped until the app is locked
    if let Some(app) = device.apps.get(&app_uuid)
        && !app.locked
    {
        tasks.push(take_locks.into_task());
    }

    tasks.push(stop_service.into_task());
    tasks
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

/// Stop a service if running and remove it, with a fast-path that migrates
/// the running container to a future release instead of recreating it.
///
/// The planner picks this task when:
/// - all target images for *other* releases of the app have already been
///   pulled (so uninstalling the current state won't strand a future install), and
/// - either a matching future service exists and a migration is safe, or
/// - the service can be stopped (if running) and uninstalled.
fn uninstall_service_when_requirements_are_met(
    svc: View<Service>,
    System(device): System<Device>,
    SystemTarget(t_device): SystemTarget<Device>,
    Args((app_uuid, rel_uuid, svc_name)): Args<(Uuid, Uuid, String)>,
) -> Vec<Task> {
    // Defer uninstall until future-release images are pulled. Removing now
    // could free networks/volumes that pending installs depend on.
    if any_images_are_pending_download(&device, &t_device, &app_uuid, &rel_uuid) {
        return Vec::new();
    }

    // Migration path: a future release expects the same service with
    // matching image/config/started state. State-only remove from the
    // current release, then migrate the container into the future release.
    if let Some((t_rel_uuid, tgt_svc)) =
        find_future_service(&t_device, &app_uuid, &rel_uuid, &svc_name)
    {
        let target_release_exists = device
            .apps
            .get(&app_uuid)
            .is_some_and(|app| app.releases.contains_key(t_rel_uuid));
        let can_migrate_as_is = service_matches_target(
            &device, &t_device, &app_uuid, &rel_uuid, &svc, t_rel_uuid, tgt_svc,
        );
        // Either no service in the app needs stopping (lock-free), or locks
        // were already taken upstream.
        let locks_satisfied = !services_need_stopping(&app_uuid, &device, &t_device)
            || device.apps.get(&app_uuid).is_some_and(|app| app.locked);

        if target_release_exists && can_migrate_as_is && locks_satisfied {
            return vec![
                remove_service.into_task(),
                migrate_service
                    .with_arg("commit", t_rel_uuid.as_str())
                    .with_target(&*svc),
            ];
        }
    }

    // Fallback: stop the service if it is still running, then uninstall it.
    let mut tasks = Vec::new();
    if svc
        .oci
        .as_ref()
        .is_some_and(|c| c.status == ContainerStatus::Running)
    {
        tasks.push(stop_service_when_requirements_are_met.into_task());
    }
    tasks.push(uninstall_service.into_task());
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
                job::none(take_locks).with_description(|Args(uuid): Args<Uuid>| {
                    format!("take locks for app with uuid '{uuid}'")
                }),
                job::update(release_locks).with_description(|Args(uuid): Args<Uuid>| {
                    format!("release locks for app with uuid '{uuid}'")
                }),
                job::delete(release_locks).with_description(|Args(uuid): Args<Uuid>| {
                    format!("release locks for app with uuid '{uuid}'")
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
                job::create(create_network_when_requirements_are_met),
                job::none(create_network).with_description(
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
                job::create(create_volume_when_requirements_are_met),
                job::none(create_volume).with_description(
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
