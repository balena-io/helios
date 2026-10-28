use mahler::worker::{Uninitialized, Worker};

use crate::common_types::HostRuntimeDir;
use crate::runtime;
use crate::util::locking::{ForceAcquireLocks, LockSet};

use super::models::Device;
#[cfg(feature = "balenahup")]
use super::tasks::with_hostapp_tasks;
use super::tasks::{with_app_tasks, with_device_tasks};
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
        // ignore app releases and locking when planning, only the app entry
        // jobs are available
        use mahler::exception;
        worker = worker
            .exception(
                "/apps/{app_uuid}/releases",
                exception::update(|| true).with_description(|| "app update support is disabled"),
            )
            .exception(
                "/apps/{app_uuid}/locked",
                exception::update(|| true).with_description(|| "app update support is disabled"),
            );
    }

    worker = with_app_tasks(worker);

    worker
}

pub type LocalWorker = Worker<Device, Uninitialized>;

/// Create worker with necessary resources
pub fn create(host_runtime_dir: HostRuntimeDir, resources: runtime::Resources) -> LocalWorker {
    // Create the worker and set-up resources
    let mut worker = worker().resource(resources.docker);

    if let Some(auth_client) = resources.registry_auth_client {
        worker.use_resource(auth_client);
    }

    worker.use_resource(host_runtime_dir);
    worker.use_resource(resources.local_store);
    worker.use_resource(LockSet::new());
    worker.use_resource(ForceAcquireLocks::from(false));
    worker
}

#[cfg(test)]
#[path = "worker/tests/mod.rs"]
mod tests;
