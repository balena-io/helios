use crate::common_types::Uuid;
use crate::models::{Device, DeviceTarget, Network, Service, ServiceTarget, Volume};

/// Find an installed network for a different commit
pub fn find_installed_network<'a>(
    device: &'a Device,
    app_uuid: &'a Uuid,
    commit: &'a Uuid,
    network_name: &'a String,
) -> Option<&'a Network> {
    device.apps.get(app_uuid).and_then(|app| {
        app.releases
            .iter()
            .filter(|(c, _)| c != &commit)
            .flat_map(|(_, r)| r.networks.iter().find(|(k, _)| k == &network_name))
            .map(|(_, n)| n)
            .next()
    })
}

/// Find an installed volume for a different commit
pub fn find_installed_volume<'a>(
    device: &'a Device,
    app_uuid: &'a Uuid,
    commit: &'a Uuid,
    volume_name: &'a String,
) -> Option<&'a Volume> {
    device.apps.get(app_uuid).and_then(|app| {
        app.releases
            .iter()
            .filter(|(c, _)| c != &commit)
            .flat_map(|(_, r)| r.volumes.iter().find(|(k, _)| k == &volume_name))
            .map(|(_, v)| v)
            .next()
    })
}

/// Find an installed service for a different commit
pub fn find_installed_service<'a>(
    device: &'a Device,
    app_uuid: &'a Uuid,
    commit: &'a Uuid,
    service_name: &'a String,
) -> Option<&'a Service> {
    device.apps.get(app_uuid).and_then(|app| {
        app.releases
            .iter()
            .filter(|(c, _)| c != &commit)
            .flat_map(|(_, r)| r.services.iter().find(|(k, _)| k == &service_name))
            .map(|(_, s)| s)
            .next()
    })
}

/// Find a new network for a different commit
pub fn find_future_network<'a>(
    t_device: &'a DeviceTarget,
    app_uuid: &'a Uuid,
    commit: &'a Uuid,
    network_name: &'a String,
) -> Option<(&'a Uuid, &'a Network)> {
    t_device.apps.get(app_uuid).and_then(|app| {
        app.releases
            .iter()
            .filter(|(c, _)| c != &commit)
            .flat_map(|(c, r)| {
                r.networks
                    .iter()
                    .find(|(k, _)| k == &network_name)
                    .map(|(_, n)| (c, n))
            })
            .next()
    })
}

/// Find a new volume for a different commit
pub fn find_future_volume<'a>(
    t_device: &'a DeviceTarget,
    app_uuid: &'a Uuid,
    commit: &'a Uuid,
    volume_name: &'a String,
) -> Option<(&'a Uuid, &'a Volume)> {
    t_device.apps.get(app_uuid).and_then(|app| {
        app.releases
            .iter()
            .filter(|(c, _)| c != &commit)
            .flat_map(|(c, r)| {
                r.volumes
                    .iter()
                    .find(|(k, _)| k == &volume_name)
                    .map(|(_, v)| (c, v))
            })
            .next()
    })
}

/// Find an new service for a different commit
pub fn find_future_service<'a>(
    t_device: &'a DeviceTarget,
    app_uuid: &'a Uuid,
    commit: &'a Uuid,
    service_name: &'a String,
) -> Option<(&'a Uuid, &'a ServiceTarget)> {
    t_device.apps.get(app_uuid).and_then(|app| {
        app.releases
            .iter()
            .filter(|(c, _)| c != &commit)
            .flat_map(|(c, r)| {
                r.services
                    .iter()
                    .find(|(k, _)| k == &service_name)
                    .map(|(_, s)| (c, s))
            })
            .next()
    })
}
