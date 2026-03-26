use std::collections::HashMap;
use std::fmt;

use bollard::models::VolumeCreateRequest;
use bollard::query_parameters::ListVolumesOptions;
use serde::{Deserialize, Serialize};

use super::{Client, Error, LocalNamespace, Namespace, Result, WithContext};

#[derive(Debug, Clone)]
pub struct Volume<'a, N> {
    client: &'a Client,
    __: std::marker::PhantomData<N>,
}

impl<'a, N> Volume<'a, N> {
    pub fn new(client: &'a Client) -> Self {
        Self {
            client,
            __: std::marker::PhantomData::<N>,
        }
    }
}

impl<N: Namespace> Volume<'_, N> {
    /// Create a volume with the given name and configuration
    pub async fn create(
        &self,
        name: &str,
        namespace: impl Into<LocalNamespace>,
        config: VolumeConfig,
    ) -> Result<String> {
        let id = namespace.into().to_identifier(name);

        // look for the volume first
        match self.inspect(&id).await {
            Ok(_) => return Ok(id),
            Err(e) if e.is_not_found() => {}
            Err(e) => Err(e)?,
        }

        let mut request: VolumeCreateRequest = config.into();
        request.name = Some(id.clone());

        match self.client.inner().create_volume(request).await {
            Ok(_) => Ok(id),
            // ignore if the error if the volume already exists
            // (not sure this error can happen according to the docker API)
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 409, ..
            }) => Ok(id),
            Err(e) => {
                Err(Error::from(e)).with_context(|| format!("failed to create volume {name}"))
            }
        }
    }

    /// Remove a volume by name
    pub async fn remove(&self, name: &str) -> Result<()> {
        match self
            .client
            .inner()
            .remove_volume(name, None::<bollard::query_parameters::RemoveVolumeOptions>)
            .await
        {
            Ok(_) => Ok(()),
            // do not fail if the volume doesn't exist
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(e) => {
                Err(Error::from(e)).with_context(|| format!("failed to remove volume {name}"))
            }
        }
    }

    /// Returns low-level information about a volume.
    pub async fn inspect(&self, name: &str) -> Result<LocalVolume<N>> {
        let volume_info = self
            .client
            .inner()
            .inspect_volume(name)
            .await
            .map_err(|e| Error::from(e).context(format!("failed to inspect volume '{name}'")))?;

        Ok(volume_info.into())
    }

    /// Returns the list of volume names on the server
    /// matching the given labels
    pub async fn list_with_labels(&self, labels: Vec<&str>) -> Result<Vec<String>> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            labels.into_iter().map(|s| s.to_owned()).collect(),
        );

        let opts = ListVolumesOptions {
            filters: Some(filters),
        };

        let response = self
            .client
            .inner()
            .list_volumes(Some(opts))
            .await
            .map_err(|e| Error::from(e).context("failed to list volumes".to_string()))?;

        Ok(response
            .volumes
            .unwrap_or_default()
            .into_iter()
            .map(|v| v.name)
            .collect())
    }
}

/// Newtype for a Docker volume driver name, defaulting to "local"
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VolumeDriver(String);

impl Default for VolumeDriver {
    fn default() -> Self {
        Self("local".to_string())
    }
}

impl fmt::Display for VolumeDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<String> for VolumeDriver {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Volume configuration used to create a Docker volume
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VolumeConfig {
    pub driver: VolumeDriver,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub driver_opts: HashMap<String, String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,
}

/// Information about a volume on the local Docker engine
#[derive(Debug, Clone)]
pub struct LocalVolume<N = LocalNamespace> {
    pub name: String,
    pub driver: VolumeDriver,
    pub driver_opts: HashMap<String, String>,
    pub labels: HashMap<String, String>,
    #[cfg(any(test, feature = "test-helpers"))]
    pub __: std::marker::PhantomData<N>,
    #[cfg(not(any(test, feature = "test-helpers")))]
    __: std::marker::PhantomData<N>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl<N> Default for LocalVolume<N> {
    fn default() -> Self {
        Self {
            name: String::default(),
            driver: VolumeDriver::default(),
            driver_opts: HashMap::default(),
            labels: HashMap::default(),
            __: std::marker::PhantomData::<N>,
        }
    }
}

impl<N: Namespace> LocalVolume<N> {
    /// Get the namepace from local volume metadata
    pub fn namespace(&self, volume: &str) -> Option<N> {
        N::from_identifier(&self.name, volume)
    }
}

impl<N> From<bollard::models::Volume> for LocalVolume<N> {
    fn from(value: bollard::models::Volume) -> Self {
        LocalVolume {
            name: value.name,
            driver: VolumeDriver::from(value.driver),
            driver_opts: value.options,
            labels: value.labels,
            __: std::marker::PhantomData::<N>,
        }
    }
}

impl From<VolumeConfig> for VolumeCreateRequest {
    fn from(config: VolumeConfig) -> Self {
        VolumeCreateRequest {
            name: None, // set by caller
            driver: Some(config.driver.to_string()),
            driver_opts: Some(config.driver_opts),
            labels: Some(config.labels),
            ..Default::default()
        }
    }
}
