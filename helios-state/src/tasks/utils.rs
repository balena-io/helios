use crate::common_types::Uuid;
use crate::models::{Device, Service};

/// Find an installed service for a different commit
pub fn find_installed_service(
    device: &Device,
    app_uuid: Uuid,
    commit: Uuid,
    service_name: String,
) -> Option<&Service> {
    device.apps.get(&app_uuid).and_then(|app| {
        app.releases
            .iter()
            .filter(|(c, _)| c != &&commit)
            .flat_map(|(_, r)| r.services.iter().find(|(k, _)| k == &&service_name))
            .map(|(_, s)| s)
            .next()
    })
}
