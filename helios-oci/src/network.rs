use std::collections::HashMap;
use std::fmt;

use bollard::models::{Ipam, IpamConfig, NetworkCreateRequest};
use bollard::query_parameters::ListNetworksOptions;
use serde::{Deserialize, Serialize};

use super::{Client, Error, Result, WithContext};

#[derive(Debug, Clone)]
pub struct NetworkClient<'a>(&'a Client);

impl<'a> NetworkClient<'a> {
    pub fn new(client: &'a Client) -> Self {
        Self(client)
    }
}

impl NetworkClient<'_> {
    /// Create a network with the given name and configuration
    pub async fn create(&self, name: &str, config: NetworkConfig) -> Result<()> {
        let mut request: NetworkCreateRequest = config.into();
        request.name = name.to_owned();

        match self.0.inner().create_network(request).await {
            Ok(_) => Ok(()),
            // ignore if the network already exists
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 409, ..
            }) => Ok(()),
            Err(e) => {
                Err(Error::from(e)).with_context(|| format!("failed to create network {name}"))
            }
        }
    }

    /// Remove a network by name
    pub async fn remove(&self, name: &str) -> Result<()> {
        match self.0.inner().remove_network(name).await {
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
            .0
            .inner()
            .list_networks(Some(opts))
            .await
            .map_err(|e| Error::from(e).context("failed to list networks".to_string()))?;

        Ok(networks
            .into_iter()
            .flat_map(|n| n.name.into_iter())
            .collect())
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
#[derive(Debug, Clone, Default)]
pub struct NetworkConfig {
    pub driver: NetworkDriver,
    pub driver_opts: HashMap<String, String>,
    pub enable_ipv6: bool,
    pub internal: bool,
    pub labels: HashMap<String, String>,
    pub ipam: NetworkIpamConfig,
}

/// IPAM configuration for a Docker network
#[derive(Debug, Clone, Default)]
pub struct NetworkIpamConfig {
    pub driver: NetworkIpamDriver,
    pub config: Vec<NetworkIpamPoolConfig>,
    pub options: HashMap<String, String>,
}

/// IPAM pool configuration for a Docker network
#[derive(Debug, Clone, Default)]
pub struct NetworkIpamPoolConfig {
    pub subnet: Option<String>,
    pub gateway: Option<String>,
    pub ip_range: Option<String>,
    pub aux_addresses: Option<HashMap<String, String>>,
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
