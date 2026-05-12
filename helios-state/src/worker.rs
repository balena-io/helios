use mahler::worker::{Uninitialized, Worker};

use helios_store::DocumentStore;

use crate::common_types::HostRuntimeDir;
use crate::oci::{Client as Docker, RegistryAuth};
use crate::util::locking::LockSet;

use super::models::Device;
use super::tasks::with_device_tasks;
#[cfg(feature = "balenahup")]
use super::tasks::with_hostapp_tasks;
#[cfg(feature = "userapps")]
use super::tasks::{with_image_tasks, with_userapp_tasks};

/// Configure the worker jobs
///
/// This is mostly used for tests
fn worker() -> Worker<Device, Uninitialized> {
    let mut worker = Worker::new();

    worker = with_device_tasks(worker);

    #[cfg(feature = "balenahup")]
    {
        worker = with_hostapp_tasks(worker);
    }

    #[cfg(feature = "userapps")]
    {
        worker = with_image_tasks(worker);
        worker = with_userapp_tasks(worker);
    }
    #[cfg(not(feature = "userapps"))]
    {
        // ignore user apps when planning
        use mahler::exception;
        worker = worker.exception(
            "/apps",
            exception::update(|| true).with_description(|| "app update support is disabled"),
        );
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
    worker.use_resource(LockSet::new());
    worker
}

#[cfg(test)]
#[path = "worker/tests/mod.rs"]
mod tests;
