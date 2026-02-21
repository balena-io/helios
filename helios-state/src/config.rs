use std::ops::Deref;
use std::path::PathBuf;

use helios_util::types::{OperatingSystem, Uuid};
use thiserror::Error;

use crate::models::Device;
use crate::oci::{Client as Docker, RegistryAuth};
use crate::read::{ReadStateError, read};
use crate::util::dirs;
use crate::util::store::Store;

pub struct StateConfig {
    pub host_os: Option<OperatingSystem>,
    pub host_runtime_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct HostRuntimeDir(pub PathBuf);

impl Deref for HostRuntimeDir {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct Resources {
    pub(crate) docker: Docker,
    pub(crate) registry_auth_client: Option<RegistryAuth>,
    pub(crate) local_store: Store,
    pub(crate) host_runtime_dir: HostRuntimeDir,
}

#[derive(Debug, Error)]
pub enum StatePrepareError {
    #[error(transparent)]
    Docker(#[from] helios_oci::Error),

    #[error(transparent)]
    ReadState(#[from] ReadStateError),
}

pub async fn prepare(
    uuid: Uuid,
    host_config: StateConfig,
    registry_auth_client: Option<RegistryAuth>,
) -> Result<(Resources, Device), StatePrepareError> {
    let docker = Docker::connect().await?;

    // Create a store for local state
    let local_store = Store::new(dirs::state_dir());

    let StateConfig {
        host_os,
        host_runtime_dir,
    } = host_config;

    let initial_state = read(&docker, &local_store, uuid, host_os).await?;
    let runtime = Resources {
        docker,
        registry_auth_client,
        local_store,
        host_runtime_dir: HostRuntimeDir(host_runtime_dir),
    };

    Ok((runtime, initial_state))
}
