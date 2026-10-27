use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

use crate::common_types::{ApiKey, HostRuntimeDir, OperatingSystem, Uuid};
use crate::models::Device;
use crate::oci::{self, Client as Docker, RegistryAuth};
use crate::read::{self, Reader};
use crate::store::DocumentStore;
use crate::util::dirs;
use crate::util::http::Uri;

pub struct HostConfig {
    pub host_os: Option<OperatingSystem>,
    pub host_runtime_dir: PathBuf,
}

/// Remote API configuration
#[derive(Clone, Debug)]
pub struct RemoteConfig {
    pub api_endpoint: Uri,
    pub api_key: ApiKey,
}

// Runtime resources
#[derive(Clone, Debug)]
pub(crate) struct Resources {
    /// Docker instance
    pub(crate) docker: Docker,
    /// An optional registry auth client
    pub(crate) registry_auth_client: Option<RegistryAuth>,
    /// The document store for local state
    pub(crate) local_store: DocumentStore,
}

/// State runtime configuration and resources
pub struct Runtime {
    /// The state loop retry interval
    pub(crate) retry_interval: Duration,
    /// The host path for runtime files
    pub(crate) host_runtime_dir: HostRuntimeDir,
    /// Runtime resources
    pub(crate) resources: Resources,
    /// State reader
    pub(crate) reader: Reader,
}

#[derive(Debug, Error)]
pub enum RuntimePrepareError {
    #[error(transparent)]
    Docker(#[from] oci::Error),

    #[error(transparent)]
    ReadState(#[from] read::Error),

    #[error("failed to initialize store: {0}")]
    StoreInit(std::io::Error),
}

pub async fn prepare(
    uuid: Uuid,
    retry_interval: Duration,
    host_config: HostConfig,
    remote_config: Option<RemoteConfig>,
) -> Result<(Runtime, Device), RuntimePrepareError> {
    let docker = Docker::connect().await?;

    // Create a store for local state
    let local_store = DocumentStore::with_root(dirs::state_dir())
        .await
        .map_err(RuntimePrepareError::StoreInit)?;

    let HostConfig {
        host_os,
        host_runtime_dir,
    } = host_config;

    // Create a registry auth client using the remote credentials
    let registry_auth_client = remote_config.map(
        |RemoteConfig {
             api_endpoint,
             api_key,
             ..
         }| RegistryAuth::new(api_endpoint, api_key),
    );

    // TODO: read OCI context here too, it should replace Host::engine_runtimes

    // initialize state reader
    let reader = Reader {
        os: host_os,
        uuid,
        docker: docker.clone(),
        local_store: local_store.clone(),
    };

    let initial_state = reader.read().await?;
    let runtime = Runtime {
        retry_interval,
        host_runtime_dir: HostRuntimeDir(host_runtime_dir),
        resources: Resources {
            docker,
            registry_auth_client,
            local_store,
        },
        reader,
    };

    Ok((runtime, initial_state))
}
