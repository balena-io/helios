use helios_util::types::{OperatingSystem, Uuid};
use thiserror::Error;

use crate::models::Device;
use crate::oci::{Client as Docker, RegistryAuthClient};
use crate::read::{ReadStateError, read};
use crate::util::dirs::state_dir;
use crate::util::store::Store;

pub struct Resources {
    pub(crate) docker: Docker,
    pub(crate) registry_auth_client: Option<RegistryAuthClient>,
    pub(crate) local_store: Store,
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
    os: Option<OperatingSystem>,
    registry_auth_client: Option<RegistryAuthClient>,
) -> Result<(Resources, Device), StatePrepareError> {
    let docker = Docker::connect().await?;

    // Create a store for local state
    let local_store = Store::new(state_dir());

    let initial_state = read(&docker, &local_store, uuid, os).await?;
    let runtime = Resources {
        docker,
        registry_auth_client,
        local_store,
    };

    Ok((runtime, initial_state))
}
