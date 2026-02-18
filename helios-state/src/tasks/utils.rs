use crate::common_types::Uuid;
use crate::models::{Device, DeviceTarget, Service, ServiceTarget};

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
