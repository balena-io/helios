//! Conversion functions from helios-oci types to Quadlet files.

use thiserror::Error;

use crate::oci;
use crate::podlet::quadlet::{
    self, Container, Downgrade, File, Globals, Install, Network, PodmanVersion, Quadlet, Resource,
    Service, Unit, Volume,
};

mod labels;
use labels::{Dependency, DependsOn, UnknownDependsOnCondition};

/// Error returned when converting OCI types to Quadlet files.
#[derive(Error, Debug)]
pub enum Error {
    /// Error parsing compose labels.
    #[error("failed to parse compose labels: {0}")]
    Labels(#[from] UnknownDependsOnCondition),

    /// Error adding a dependency.
    #[error("failed to add dependency: {0}")]
    Dependency(#[from] quadlet::unit::AddDependencyError),

    /// Error downgrading to target Podman version.
    #[error("failed to downgrade: {0}")]
    Downgrade(#[from] quadlet::DowngradeError),
}

/// Convert container parameters to a quadlet [`File`].
///
/// # Errors
///
/// Returns an error if label parsing or version downgrade fails.
pub fn from_container_config(
    name: &str,
    image: &str,
    config: &mut oci::ContainerConfig,
    version: PodmanVersion,
) -> Result<File, Error> {
    let mut unit = Unit::default();

    // Parse compose labels for dependencies and metadata
    let depends_on = DependsOn::parse(&config.labels)?;

    // Add dependencies from compose labels
    for Dependency { service, config } in depends_on {
        unit.add_dependency(service, &config)?;
    }

    // Build label list, removing special labels
    let mut label_entries: Vec<String> = Vec::new();

    // Remove compose-internal labels
    config
        .labels
        .retain(|key, _| !key.starts_with("com.docker.compose"));
    for (key, value) in config.labels.iter() {
        label_entries.push(format!("{key}={value}"));
    }

    // Build Exec= from cmd
    let exec = config
        .command
        .as_ref()
        .map(crate::podlet::escape::command_join);

    let container = Container {
        image: image.to_owned(),
        // use the same name as the unit instead of `systemd-%N`
        container_name: Some("%N".to_string()),
        exec,
        auto_update: None,
        label: label_entries,
        ..Container::default()
    };

    let install = Install {
        wanted_by: vec!["default.target".to_string()],
        ..Default::default()
    };

    let mut file = File {
        name: name.to_owned(),
        unit,
        resource: Resource::Container(Box::new(container)),
        globals: Globals::default(),
        quadlet: Quadlet::default(),
        service: Service::default(),
        install,
    };

    file.downgrade(version)?;

    Ok(file)
}

/// Convert a [`LocalVolume`](oci::LocalVolume) to a quadlet [`File`].
///
/// # Errors
///
/// Returns an error if version downgrade fails.
pub fn from_volume_config(
    name: &str,
    volume: &oci::VolumeConfig,
    version: PodmanVersion,
) -> Result<File, Error> {
    let driver = if volume.driver.to_string() == "local" {
        None
    } else {
        Some(volume.driver.to_string())
    };

    let options: Vec<String> = volume
        .driver_opts
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();

    let label: Vec<String> = volume
        .labels
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();

    let volume = Volume {
        // use the same name as the unit instead of `systemd-%N`
        volume_name: Some("%N".to_string()),
        driver,
        options: if options.is_empty() {
            None
        } else {
            Some(options.join(","))
        },
        label,
        ..Volume::default()
    };

    let mut file = File {
        // use the same name as the unit instead of `systemd-%N`
        name: name.to_owned(),
        unit: Unit::default(),
        resource: Resource::Volume(volume),
        globals: Globals::default(),
        quadlet: Quadlet::default(),
        service: Service::default(),
        install: Install::default(),
    };

    file.downgrade(version)?;

    Ok(file)
}

/// Convert a [`LocalNetwork`](oci::LocalNetwork) to a quadlet [`File`].
///
/// # Errors
///
/// Returns an error if version downgrade fails.
pub fn from_network_config(
    name: &str,
    network: &oci::NetworkConfig,
    version: PodmanVersion,
) -> Result<File, Error> {
    let driver = if network.driver.to_string() == "bridge" {
        None
    } else {
        Some(network.driver.to_string())
    };

    let options: Vec<String> = network
        .driver_opts
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();

    let label: Vec<String> = network
        .labels
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();

    let ipam_driver = if network.ipam.driver.to_string() == "default" {
        None
    } else {
        Some(network.ipam.driver.to_string())
    };

    let mut subnet = Vec::new();
    let mut gateway = Vec::new();
    let mut ip_range = Vec::new();

    for pool in &network.ipam.config {
        if let Some(s) = &pool.subnet
            && let Ok(net) = s.parse()
        {
            subnet.push(net);
        }
        if let Some(g) = &pool.gateway
            && let Ok(addr) = g.parse()
        {
            gateway.push(addr);
        }
        if let Some(r) = &pool.ip_range
            && let Ok(range) = r.parse()
        {
            ip_range.push(range);
        }
    }

    let network = Network {
        // use the same name as the unit instead of `systemd-%N`
        network_name: Some("%N".to_string()),
        driver,
        internal: network.internal,
        ipv6: network.enable_ipv6,
        ipam_driver,
        subnet,
        gateway,
        ip_range,
        options,
        label,
        ..Network::default()
    };

    let mut file = File {
        name: name.to_owned(),
        unit: Unit::default(),
        resource: Resource::Network(network),
        globals: Globals::default(),
        quadlet: Quadlet::default(),
        service: Service::default(),
        install: Install::default(),
    };

    file.downgrade(version)?;

    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn basic_container_conversion() {
        let mut config = oci::ContainerConfig {
            command: Some(vec!["echo".into(), "hello".into()]),
            ..Default::default()
        };

        let file =
            from_container_config("test", "alpine:latest", &mut config, PodmanVersion::LATEST)
                .unwrap();

        assert_eq!(file.name, "test");
        if let Resource::Container(c) = &file.resource {
            assert_eq!(c.image, "alpine:latest");
            assert_eq!(c.exec.as_deref(), Some("echo hello"));
        } else {
            panic!("expected Container resource");
        }
    }

    #[test]
    fn container_with_dependencies() {
        let mut labels = HashMap::new();
        labels.insert(
            "com.docker.compose.depends_on".to_owned(),
            "db:service_started:true".to_owned(),
        );

        let mut config = oci::ContainerConfig {
            command: None,
            labels,
            ..Default::default()
        };

        let file = from_container_config("web", "nginx:latest", &mut config, PodmanVersion::LATEST)
            .unwrap();

        assert!(!file.unit.binds_to.is_empty());
        assert!(file.unit.binds_to[0].ends_with(".service"));
    }

    #[test]
    fn volume_conversion() {
        let volume = oci::VolumeConfig {
            driver: oci::VolumeDriver::default(),
            labels: HashMap::from([("app".to_owned(), "test".to_owned())]),
            ..Default::default()
        };

        let file = from_volume_config("data", &volume, PodmanVersion::LATEST).unwrap();
        assert_eq!(file.name, "data");
        if let Resource::Volume(v) = &file.resource {
            assert!(v.driver.is_none());
            assert_eq!(v.label, vec!["app=test"]);
        } else {
            panic!("expected Volume resource");
        }
    }

    #[test]
    fn network_conversion() {
        let network = oci::NetworkConfig {
            driver: oci::NetworkDriver::default(),
            driver_opts: HashMap::new(),
            enable_ipv6: true,
            internal: true,
            labels: HashMap::new(),
            ipam: oci::NetworkIpamConfig::default(),
        };

        let file = from_network_config("mynet", &network, PodmanVersion::LATEST).unwrap();
        assert_eq!(file.name, "mynet");
        if let Resource::Network(n) = &file.resource {
            assert!(n.driver.is_none());
            assert!(n.internal);
            assert!(n.ipv6);
        } else {
            panic!("expected Network resource");
        }
    }
}
