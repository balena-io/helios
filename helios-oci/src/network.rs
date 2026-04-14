use std::collections::HashMap;
use std::fmt;

use bollard::config::{EndpointSettings, NetworkConnectRequest, NetworkDisconnectRequest};
use bollard::models::{Ipam, IpamConfig, NetworkCreateRequest, NetworkInspect};
use bollard::query_parameters::ListNetworksOptions;
use serde::{Deserialize, Serialize};

use super::{Client, Error, LocalNamespace, Namespace, Result, WithContext};

#[derive(Debug, Clone)]
pub struct Network<'a, N> {
    client: &'a Client,
    __: std::marker::PhantomData<N>,
}

impl<'a, N> Network<'a, N> {
    pub fn new(client: &'a Client) -> Self {
        Self {
            client,
            __: std::marker::PhantomData::<N>,
        }
    }
}

impl<N: Namespace> Network<'_, N> {
    /// Create a network with the given name and configuration
    pub async fn create(
        &self,
        network: &str,
        namespace: impl Into<N>,
        config: NetworkConfig,
    ) -> Result<String> {
        let id = namespace.into().to_identifier(network);

        // look for the network first
        match self.inspect(&id).await {
            Ok(_) => return Ok(id),
            Err(e) if e.is_not_found() => {}
            Err(e) => Err(e)?,
        }

        let mut request: NetworkCreateRequest = config.into();
        request.name = id.clone();

        match self.client.inner().create_network(request).await {
            Ok(_) => Ok(id),
            // ignore if the error if the network already exists
            // (not sure this error can happen according to the docker API)
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 409, ..
            }) => Ok(id),
            Err(e) => {
                Err(Error::from(e)).with_context(|| format!("failed to create network {network}"))
            }
        }
    }

    /// Remove a network by name
    pub async fn remove(&self, name: &str) -> Result<()> {
        match self.client.inner().remove_network(name).await {
            Ok(_) => Ok(()),
            // do not fail if the network doesn't exist
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(e) => {
                Err(Error::from(e)).with_context(|| format!("failed to remove network {name}"))
            }
        }
    }

    /// Returns low-level information about a network.
    pub async fn inspect(&self, name: &str) -> Result<LocalNetwork<N>> {
        let network_info = self
            .client
            .inner()
            .inspect_network(
                name,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
            .map_err(|e| Error::from(e).context(format!("failed to inspect network '{name}'")))?;

        let network = network_info
            .try_into()
            .with_context(|| format!("failed to inspect network '{name}'"))?;

        Ok(network)
    }

    /// Returns the list of network names on the server
    /// matching the given labels
    pub async fn list_with_labels(&self, labels: Vec<&str>) -> Result<Vec<String>> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            labels.into_iter().map(|s| s.to_owned()).collect(),
        );

        let opts = ListNetworksOptions {
            filters: Some(filters),
        };

        let networks = self
            .client
            .inner()
            .list_networks(Some(opts))
            .await
            .map_err(|e| Error::from(e).context("failed to list networks".to_string()))?;

        Ok(networks
            .into_iter()
            .flat_map(|n| n.name.into_iter())
            .collect())
    }

    /// Connect a container to a network with the given endpoint configuration
    pub async fn connect(
        &self,
        name: &str,
        container: &str,
        endpoint_config: EndpointSettings,
    ) -> Result<()> {
        let config = NetworkConnectRequest {
            container: container.to_string(),
            endpoint_config: Some(endpoint_config),
        };

        self.client
            .inner()
            .connect_network(name, config)
            .await
            .map_err(|e| {
                Error::from(e).context(format!(
                    "failed to connect container '{container}' to network '{name}'"
                ))
            })
    }

    /// Disconnect a container from a network
    pub async fn disconnect(
        &self,
        network: &str,
        namespace: impl Into<N>,
        container: &str,
    ) -> Result<()> {
        let id = namespace.into().to_identifier(network);
        let config = NetworkDisconnectRequest {
            container: container.to_string(),
            force: Some(false),
        };

        match self.client.inner().disconnect_network(&id, config).await {
            Ok(_) => Ok(()),
            // do not fail if the container is not connected
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(e) => Err(Error::from(e).context(format!(
                "failed to disconnect container '{container}' from network '{network}'"
            ))),
        }
    }
}

/// Newtype for a Docker network driver name, defaulting to "bridge"
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkDriver(String);

impl Default for NetworkDriver {
    fn default() -> Self {
        Self("bridge".to_string())
    }
}

impl fmt::Display for NetworkDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<String> for NetworkDriver {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Newtype for a Docker IPAM driver name, defaulting to "default"
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkIpamDriver(String);

impl Default for NetworkIpamDriver {
    fn default() -> Self {
        Self("default".to_string())
    }
}

impl fmt::Display for NetworkIpamDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<String> for NetworkIpamDriver {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Network configuration used to create a Docker network
#[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct NetworkConfig {
    pub driver: NetworkDriver,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub driver_opts: HashMap<String, String>,
    pub enable_ipv6: bool,
    pub internal: bool,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,
    pub ipam: NetworkIpamConfig,
}

/// IPAM configuration for a Docker network
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct NetworkIpamConfig {
    pub driver: NetworkIpamDriver,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub config: Vec<NetworkIpamPoolConfig>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub options: HashMap<String, String>,
}

/// IPAM pool configuration for a Docker network
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct NetworkIpamPoolConfig {
    pub subnet: Option<String>,
    pub gateway: Option<String>,
    pub ip_range: Option<String>,
    pub aux_addresses: Option<HashMap<String, String>>,
}

/// Information about a network on the local Docker engine
#[derive(Debug, Clone)]
pub struct LocalNetwork<N = LocalNamespace> {
    pub name: String,
    pub driver: NetworkDriver,
    pub driver_opts: HashMap<String, String>,
    pub enable_ipv4: bool,
    pub enable_ipv6: bool,
    pub internal: bool,
    pub labels: HashMap<String, String>,
    pub ipam: NetworkIpamConfig,
    #[cfg(any(test, feature = "test-helpers"))]
    pub __: std::marker::PhantomData<N>,
    #[cfg(not(any(test, feature = "test-helpers")))]
    __: std::marker::PhantomData<N>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl<N> Default for LocalNetwork<N> {
    fn default() -> Self {
        Self {
            name: String::default(),
            driver: NetworkDriver::default(),
            driver_opts: HashMap::default(),
            enable_ipv4: false,
            enable_ipv6: false,
            internal: false,
            labels: HashMap::default(),
            ipam: NetworkIpamConfig::default(),
            __: std::marker::PhantomData::<N>,
        }
    }
}

impl<N: Namespace> LocalNetwork<N> {
    /// Get the namepace from the local network metadata
    pub fn namespace(&self, network: &str) -> Option<N> {
        N::from_identifier(&self.name, network)
    }
}

impl<N> TryFrom<NetworkInspect> for LocalNetwork<N> {
    type Error = Error;

    fn try_from(value: NetworkInspect) -> Result<Self> {
        let name = value.name.ok_or("network name should not be nil")?;
        let driver = value.driver.map(NetworkDriver::from).unwrap_or_default();
        let driver_opts = value.options.unwrap_or_default();
        let enable_ipv4 = value.enable_ipv4.unwrap_or(true);
        let enable_ipv6 = value.enable_ipv6.unwrap_or(false);
        let internal = value.internal.unwrap_or(false);
        let labels = value.labels.unwrap_or_default();

        let ipam = match value.ipam {
            Some(ipam) => NetworkIpamConfig {
                driver: ipam.driver.map(NetworkIpamDriver::from).unwrap_or_default(),
                config: ipam
                    .config
                    .unwrap_or_default()
                    .into_iter()
                    .map(|pool| NetworkIpamPoolConfig {
                        subnet: pool.subnet,
                        gateway: pool.gateway,
                        ip_range: pool.ip_range,
                        aux_addresses: pool.auxiliary_addresses,
                    })
                    .collect(),
                options: ipam.options.unwrap_or_default(),
            },
            None => NetworkIpamConfig::default(),
        };

        Ok(LocalNetwork {
            name,
            driver,
            driver_opts,
            enable_ipv4,
            enable_ipv6,
            internal,
            labels,
            ipam,
            __: std::marker::PhantomData::<N>,
        })
    }
}

impl From<NetworkConfig> for NetworkCreateRequest {
    fn from(config: NetworkConfig) -> Self {
        let ipam = config.ipam;
        let ipam = Some(Ipam {
            driver: Some(ipam.driver.to_string()),
            config: Some(
                ipam.config
                    .into_iter()
                    .map(|pool| IpamConfig {
                        subnet: pool.subnet,
                        gateway: pool.gateway,
                        ip_range: pool.ip_range,
                        auxiliary_addresses: pool.aux_addresses,
                    })
                    .collect(),
            ),
            options: Some(ipam.options),
        });

        NetworkCreateRequest {
            name: String::new(), // set by caller
            driver: Some(config.driver.to_string()),
            enable_ipv6: Some(config.enable_ipv6),
            internal: Some(config.internal),
            labels: Some(config.labels),
            options: Some(config.driver_opts),
            ipam,
            ..Default::default()
        }
    }
}
