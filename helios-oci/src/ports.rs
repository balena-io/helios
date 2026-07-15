//! Container port publishing configuration (Compose `ports`).
//!
//! A [`PortMapping`] holds a single container (target) port using the Compose
//! long-syntax fields. Port order carries no meaning,
//! so mappings are kept sorted by container port then protocol to guarantee a
//! deterministic serialized form for state comparison.

use std::fmt;
use std::str::FromStr;

#[cfg(any(test, feature = "test-helpers"))]
use std::net::IpAddr;

use bollard::models::{PortBinding, PortMap};
use serde::{Deserialize, Serialize};

use super::{Error, Result};

/// Transport protocol of a published port.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PortProtocol {
    #[default]
    Tcp,
    Udp,
}

impl fmt::Display for PortProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        })
    }
}

impl FromStr for PortProtocol {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "tcp" => Ok(Self::Tcp),
            "udp" => Ok(Self::Udp),
            other => Err(format!("unsupported port protocol `{other}`")),
        }
    }
}

/// A single published container port.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PortMapping {
    /// Container port
    pub target: u16,
    /// Port protocol (tcp/udp)
    #[serde(default)]
    pub protocol: PortProtocol,
    /// Host port. `None` publishes to an ephemeral port.
    #[serde(default)]
    pub published: Option<u16>,
    /// Host IP to bind to. `None` binds to all interfaces.
    #[serde(default)]
    pub host_ip: Option<String>,
}

impl PortMapping {
    /// Engine key for the container side of the mapping, e.g. `80/tcp`.
    fn engine_key(&self) -> String {
        format!("{}/{}", self.target, self.protocol)
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl fmt::Display for PortMapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ip) = &self.host_ip {
            write!(f, "{ip}:")?;
            if let Some(published) = &self.published {
                write!(f, "{published}")?;
            }
            write!(f, ":")?;
        } else if let Some(published) = &self.published {
            write!(f, "{published}:")?;
        }
        write!(f, "{}/{}", self.target, self.protocol)
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl FromStr for PortMapping {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        let (spec, protocol) = match s.rsplit_once('/') {
            Some((spec, proto)) => (spec, proto.parse()?),
            None => (s, PortProtocol::default()),
        };

        // Split from the right so an unbracketed IPv6 host address (which
        // itself contains `:`) is left intact as the remainder.
        let mut parts = spec.rsplitn(3, ':');
        let target_part = parts.next().unwrap_or_default();
        let published_part = parts.next();
        let host_ip_part = parts.next();

        let target = parse_port(target_part)?;

        let published = match published_part {
            None => None,
            // an empty host port is only valid together with a host IP
            // (`127.0.0.1::80`), meaning publish to an ephemeral port
            Some("") if host_ip_part.is_none() => {
                return Err(format!("invalid port mapping `{s}`"));
            }
            Some("") => None,
            Some(published) => Some(parse_port(published)?),
        };

        let host_ip = host_ip_part.map(parse_host_ip).transpose()?;

        Ok(Self {
            target,
            published,
            host_ip,
            protocol,
        })
    }
}

fn parse_port(s: &str) -> std::result::Result<u16, String> {
    s.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| format!("invalid port number `{s}`"))
}

/// Parse an engine host port, which may be a single port (`8080`) or a range
/// (`8000-9000`), into its individual ports.
fn parse_host_port_range(s: &str) -> std::result::Result<Vec<u16>, String> {
    match s.split_once('-') {
        Some((start, end)) => {
            let start = parse_port(start)?;
            let end = parse_port(end)?;
            if start > end {
                return Err(format!("invalid port range `{s}`"));
            }
            Ok((start..=end).collect())
        }
        None => Ok(vec![parse_port(s)?]),
    }
}

/// Validate a host IP, accepting bracketed IPv6 (`[::1]`) but returning the
/// unbracketed form.
#[cfg(any(test, feature = "test-helpers"))]
fn parse_host_ip(s: &str) -> std::result::Result<String, String> {
    let ip = s
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(s);
    ip.parse::<IpAddr>()
        .map_err(|_| format!("invalid host IP address `{s}`"))?;
    Ok(ip.to_string())
}

/// Build the engine `ExposedPorts` keys and `HostConfig.PortBindings` map
/// for a container create request. Multiple mappings of the same container
/// port/protocol are grouped into a single binding list.
pub(crate) fn to_oci_port_maps(ports: Vec<PortMapping>) -> (Vec<String>, PortMap) {
    let mut bindings = PortMap::new();
    for mapping in ports {
        bindings
            .entry(mapping.engine_key())
            .or_insert_with(|| Some(Vec::new()))
            .get_or_insert_with(Vec::new)
            .push(PortBinding {
                host_ip: mapping.host_ip,
                host_port: mapping.published.map(|p| p.to_string()),
            });
    }
    let mut exposed: Vec<String> = bindings.keys().cloned().collect();
    exposed.sort();
    (exposed, bindings)
}

/// Read port mappings back from the engine's `HostConfig.PortBindings`,
/// normalizing empty-string host IP/port (the engine's "unset") to absent.
pub(crate) fn from_oci_port_map(map: PortMap) -> Result<Vec<PortMapping>> {
    let mut ports = Vec::new();
    for (key, bindings) in map {
        let (target, protocol) = parse_engine_key(&key)
            .map_err(|e| Error::other(format!("invalid engine port binding `{key}`: {e}")))?;

        let bindings = bindings.unwrap_or_default();
        if bindings.is_empty() {
            ports.push(PortMapping {
                target,
                published: None,
                host_ip: None,
                protocol,
            });
            continue;
        }

        for binding in bindings {
            let host_ip = binding
                .host_ip
                .filter(|ip| !ip.is_empty() && ip != "0.0.0.0");
            match binding.host_port.filter(|port| !port.is_empty()) {
                None => {
                    ports.push(PortMapping {
                        target,
                        published: None,
                        host_ip,
                        protocol,
                    });
                }
                // The engine may report a range (`8000-9000`) as the host port
                // so we expand it to one mapping per host port
                Some(host_port) => {
                    let published = parse_host_port_range(&host_port).map_err(|e| {
                        Error::other(format!("invalid engine port binding `{key}`: {e}"))
                    })?;
                    for host_port in published {
                        ports.push(PortMapping {
                            target,
                            published: Some(host_port),
                            host_ip: host_ip.clone(),
                            protocol,
                        });
                    }
                }
            }
        }
    }
    // `PortMap` is a hash map, so iteration order is unstable. Sort
    // after the fact to match the canonical order the target state uses,
    // so a reordering never triggers reconfiguration.
    ports.sort();
    Ok(ports)
}

fn parse_engine_key(key: &str) -> std::result::Result<(u16, PortProtocol), String> {
    let (port, protocol) = match key.split_once('/') {
        Some((port, proto)) => (port, proto.parse()?),
        None => (key, PortProtocol::default()),
    };
    Ok((parse_port(port)?, protocol))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(s: &str) -> PortMapping {
        s.parse().unwrap()
    }

    #[test]
    fn display_round_trips_canonical_forms() {
        for canonical in [
            "80/tcp",
            "8080:80/tcp",
            "127.0.0.1:8080:80/tcp",
            "127.0.0.1::80/tcp",
            "6060:6060/udp",
            "::1:8080:80/tcp",
        ] {
            assert_eq!(mapping(canonical).to_string(), canonical);
        }
    }

    #[test]
    fn parse_defaults_to_tcp() {
        assert_eq!(
            mapping("8080:80"),
            PortMapping {
                target: 80,
                published: Some(8080),
                host_ip: None,
                protocol: PortProtocol::Tcp,
            }
        );
    }

    #[test]
    fn parse_bracketed_ipv6_normalizes_to_unbracketed() {
        assert_eq!(mapping("[::1]:8080:80"), mapping("::1:8080:80"));
        assert_eq!(mapping("[::1]:8080:80").to_string(), "::1:8080:80/tcp");
    }

    #[test]
    fn parse_ephemeral_with_host_ip() {
        assert_eq!(
            mapping("127.0.0.1::80"),
            PortMapping {
                target: 80,
                published: None,
                host_ip: Some("127.0.0.1".to_string()),
                protocol: PortProtocol::Tcp,
            }
        );
    }

    #[test]
    fn parse_rejects_invalid_mappings() {
        for invalid in [
            "",
            ":80",
            "0",
            "8080:0",
            "65536",
            "80/icmp",
            "host:8080:80",
            "9000-8000:80",
            "[::1:8080:80",
        ] {
            assert!(invalid.parse::<PortMapping>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn to_engine_port_maps_groups_by_container_port() {
        let ports = Vec::from([
            mapping("8080:80"),
            mapping("127.0.0.1:8081:80"),
            mapping("53:53/udp"),
            mapping("443"),
        ]);
        let (exposed, bindings) = to_oci_port_maps(ports);

        assert_eq!(exposed, vec!["443/tcp", "53/udp", "80/tcp"]);
        assert_eq!(
            bindings["80/tcp"],
            Some(vec![
                PortBinding {
                    host_ip: None,
                    host_port: Some("8080".to_string()),
                },
                PortBinding {
                    host_ip: Some("127.0.0.1".to_string()),
                    host_port: Some("8081".to_string()),
                },
            ])
        );
        assert_eq!(
            bindings["443/tcp"],
            Some(vec![PortBinding {
                host_ip: None,
                host_port: None,
            }])
        );
    }

    #[test]
    fn from_engine_port_map_normalizes_empty_strings() {
        let map = PortMap::from([(
            "80/tcp".to_string(),
            Some(vec![PortBinding {
                host_ip: Some("".to_string()),
                host_port: Some("".to_string()),
            }]),
        )]);
        assert_eq!(from_oci_port_map(map).unwrap(), Vec::from([mapping("80")]));
    }

    #[test]
    fn from_engine_port_map_normalizes_ipv4_catch_all_host_ip() {
        // Podman reports an unset host IP as `0.0.0.0`; it must round-trip to a
        // target with no host IP so the service configuration settles
        let map = PortMap::from([(
            "80/tcp".to_string(),
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some("8080".to_string()),
            }]),
        )]);
        assert_eq!(
            from_oci_port_map(map).unwrap(),
            Vec::from([mapping("8080:80")])
        );
    }

    #[test]
    fn from_engine_port_map_expands_host_port_range() {
        // an engine-reported host range expands into one mapping per host port
        let map = PortMap::from([(
            "80/tcp".to_string(),
            Some(vec![PortBinding {
                host_ip: None,
                host_port: Some("8000-8002".to_string()),
            }]),
        )]);
        assert_eq!(
            from_oci_port_map(map).unwrap(),
            Vec::from([mapping("8000:80"), mapping("8001:80"), mapping("8002:80"),])
        );
    }

    #[test]
    fn from_engine_port_map_handles_missing_bindings() {
        // some engines report a published-without-bindings entry as null
        let map = PortMap::from([("80/udp".to_string(), None)]);
        assert_eq!(
            from_oci_port_map(map).unwrap(),
            Vec::from([mapping("80/udp")])
        );
    }

    #[test]
    fn engine_round_trip() {
        // In canonical order (sorted by container port then protocol), so the
        // round trip through the engine representation is the identity.
        let ports = Vec::from([
            mapping("53:53/udp"),
            mapping("8080:80"),
            mapping("127.0.0.1:8081:80"),
            mapping("443"),
            mapping("9000:3000"),
        ]);
        let (_, bindings) = to_oci_port_maps(ports.clone());
        assert_eq!(from_oci_port_map(bindings).unwrap(), ports);
    }
}
