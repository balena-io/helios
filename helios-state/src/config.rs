use std::ops::Deref;
use std::path::PathBuf;

use thiserror::Error;

use crate::common_types::{OperatingSystem, Uuid};
use crate::models::Device;
use crate::oci::{self, Client as Docker, RegistryAuth};
use crate::read::{ReadStateError, read};
use crate::store::DocumentStore;
use crate::util::dirs;

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
    pub(crate) local_store: DocumentStore,
    pub(crate) host_runtime_dir: HostRuntimeDir,
}

#[derive(Debug, Error)]
pub enum StatePrepareError {
    #[error(transparent)]
    Docker(#[from] oci::Error),

    #[error(transparent)]
    ReadState(#[from] ReadStateError),

    #[error("failed to initialize store: {0}")]
    StoreInit(std::io::Error),
}

pub async fn prepare(
    uuid: Uuid,
    host_config: StateConfig,
    registry_auth_client: Option<RegistryAuth>,
) -> Result<(Resources, Device), StatePrepareError> {
    let docker = Docker::connect().await?;

    // Create a store for local state
    let local_store = DocumentStore::with_root(dirs::state_dir())
        .await
        .map_err(StatePrepareError::StoreInit)?;

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
