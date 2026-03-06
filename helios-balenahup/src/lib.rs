use std::ops::Deref;
use std::path::PathBuf;

mod models;
pub mod read;
mod tasks;

pub use models::{Host, HostRelease, HostReleaseStatus, HostReleaseTarget, HostTarget};
pub use tasks::{HostCleanupError, cleanup_hostapp, with_hostapp_tasks};

use helios_oci as oci;
use helios_remote_model as remote_model;
use helios_store as store;
use helios_util as util;
use helios_util::types as common_types;

// prefix for container/dir and files for balenahup
const BALENAHUP: &str = "balenahup";

/// Directory on the host filesystem used by balenahup at runtime.
#[derive(Debug, Clone)]
pub struct HostRuntimeDir(pub PathBuf);

impl Deref for HostRuntimeDir {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
