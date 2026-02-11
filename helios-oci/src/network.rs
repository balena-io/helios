use std::collections::HashMap;

use bollard::models::{Ipam, IpamConfig, NetworkCreateRequest, NetworkInspect};
use bollard::query_parameters::ListNetworksOptions;
use helios_util::network::{DEFAULT_IPAM_DRIVER, DEFAULT_NETWORK_DRIVER};

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

        if let Err(e) = self.0.inner().create_network(request).await {
            if let bollard::errors::Error::DockerResponseServerError { status_code, .. } = e {
                // do not fail if the network already exists
                if status_code != 409 {
                    return Err(
                        Error::from(e).context(format!("failed to create network {name}"))
                    );
                }
            } else {
                return Err(Error::from(e).context(format!("failed to create network {name}")));
            }
        }

        Ok(())
    }

    /// Remove a network by name
    pub async fn remove(&self, name: &str) -> Result<()> {
        if let Err(e) = self.0.inner().remove_network(name).await {
            if let bollard::errors::Error::DockerResponseServerError { status_code, .. } = e {
                // do not fail if the network doesn't exist
                if status_code != 404 {
                    return Err(Error::from(e).context(format!("failed to remove network {name}")));
                }
            } else {
                return Err(Error::from(e).context(format!("failed to remove network {name}")));
            }
        }
        Ok(())
    }

    /// Returns low-level information about a network.
    pub async fn inspect(&self, name: &str) -> Result<LocalNetwork> {
        let network_info = self
            .0
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
            .0
            .inner()
            .list_networks(Some(opts))
            .await
            .map_err(Error::with_context("failed to list networks"))?;

        Ok(networks
            .into_iter()
            .flat_map(|n| n.name.into_iter())
            .collect())
    }
}

/// Network configuration used to create a Docker network
#[derive(Debug, Clone, Default)]
pub struct NetworkConfig {
    pub driver: String,
    pub driver_opts: HashMap<String, String>,
    pub enable_ipv6: bool,
    pub internal: bool,
    pub labels: HashMap<String, String>,
    pub config_only: bool,
    pub ipam: NetworkIpamConfig,
}

/// IPAM configuration for a Docker network
#[derive(Debug, Clone, Default)]
pub struct NetworkIpamConfig {
    pub driver: String,
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

/// Information about a network on the local Docker engine
#[derive(Debug, Clone)]
pub struct LocalNetwork {
    pub name: String,
    pub driver: String,
    pub driver_opts: HashMap<String, String>,
    pub enable_ipv6: bool,
    pub internal: bool,
    pub labels: HashMap<String, String>,
    pub config_only: bool,
    pub ipam: NetworkIpamConfig,
}

impl TryFrom<NetworkInspect> for LocalNetwork {
    type Error = Error;

    fn try_from(value: NetworkInspect) -> Result<Self> {
        let name = value.name.ok_or("network name should not be nil")?;
        let driver = value.driver.unwrap_or_else(|| DEFAULT_NETWORK_DRIVER.to_string());
        let driver_opts = value.options.unwrap_or_default();
        let enable_ipv6 = value.enable_ipv6.unwrap_or(false);
        let internal = value.internal.unwrap_or(false);
        let labels = value.labels.unwrap_or_default();
        let config_only = value.config_only.unwrap_or(false);

        let ipam = match value.ipam {
            Some(ipam) => NetworkIpamConfig {
                driver: ipam.driver.unwrap_or_else(|| DEFAULT_IPAM_DRIVER.to_string()),
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
            enable_ipv6,
            internal,
            labels,
            config_only,
            ipam,
        })
    }
}

impl From<NetworkConfig> for NetworkCreateRequest {
    fn from(config: NetworkConfig) -> Self {
        let ipam = config.ipam;
        let ipam = Some(Ipam {
            driver: Some(ipam.driver),
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
            driver: Some(config.driver),
            enable_ipv6: Some(config.enable_ipv6),
            internal: Some(config.internal),
            labels: Some(config.labels),
            config_only: Some(config.config_only),
            options: Some(config.driver_opts),
            ipam,
            ..Default::default()
        }
    }
}
