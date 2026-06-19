//! Container port publishing configuration (Compose `ports`).
//!
//! A [`PortMapping`] holds a single container (target) port using the Compose
//! long-syntax fields, but serializes as the canonical short-syntax string
//! `[HOST_IP:][HOST_PORT:]CONTAINER_PORT/PROTOCOL` (protocol always explicit)
//! so ports stay readable in serialized state. Mappings are kept in a
//! `BTreeSet` since port order carries no meaning; the set guarantees a
//! deterministic serialized form for state comparison.

use std::collections::BTreeSet;
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

/// Host side of a port mapping: a fixed port, or a range from which the
/// engine picks a free port at container start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HostPort {
    Single(u16),
    Range(u16, u16),
}

impl fmt::Display for HostPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Single(port) => write!(f, "{port}"),
            Self::Range(start, end) => write!(f, "{start}-{end}"),
        }
    }
}

impl FromStr for HostPort {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s.split_once('-') {
            Some((start, end)) => {
                let start = parse_port(start)?;
                let end = parse_port(end)?;
                if start > end {
                    return Err(format!("invalid port range `{s}`"));
                }
                Ok(Self::Range(start, end))
            }
            None => Ok(Self::Single(parse_port(s)?)),
        }
    }
}

impl Serialize for HostPort {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for HostPort {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// A single published container port.
///
/// Field declaration order doubles as the derived `Ord` sort key, which
/// determines the serialized order within a config's port set.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PortMapping {
    /// Container port
    pub target: u16,
    /// Host port (or range). `None` publishes to an ephemeral port.
    #[serde(default)]
    pub published: Option<HostPort>,
    /// Host IP to bind to. `None` binds to all interfaces.
    #[serde(default)]
    pub host_ip: Option<String>,
    #[serde(default)]
    pub protocol: PortProtocol,
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
            Some(published) => Some(published.parse()?),
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
pub(crate) fn to_oci_port_maps(ports: BTreeSet<PortMapping>) -> (Vec<String>, PortMap) {
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
pub(crate) fn from_oci_port_map(map: PortMap) -> Result<BTreeSet<PortMapping>> {
    let mut ports = BTreeSet::new();
    for (key, bindings) in map {
        let (target, protocol) = parse_engine_key(&key)
            .map_err(|e| Error::other(format!("invalid engine port binding `{key}`: {e}")))?;

        let bindings = bindings.unwrap_or_default();
        if bindings.is_empty() {
            ports.insert(PortMapping {
                target,
                published: None,
                host_ip: None,
                protocol,
            });
            continue;
        }

        for binding in bindings {
            let host_ip = binding.host_ip.filter(|ip| !ip.is_empty());
            let published = binding
                .host_port
                .filter(|port| !port.is_empty())
                .map(|port| port.parse::<HostPort>())
                .transpose()
                .map_err(|e| Error::other(format!("invalid engine port binding `{key}`: {e}")))?;
            ports.insert(PortMapping {
                target,
                published,
                host_ip,
                protocol,
            });
        }
    }
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
            "8000-9000:80/tcp",
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
                published: Some(HostPort::Single(8080)),
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
        let ports = BTreeSet::from([
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
        assert_eq!(
            from_oci_port_map(map).unwrap(),
            BTreeSet::from([mapping("80")])
        );
    }

    #[test]
    fn from_engine_port_map_handles_missing_bindings() {
        // some engines report a published-without-bindings entry as null
        let map = PortMap::from([("80/udp".to_string(), None)]);
        assert_eq!(
            from_oci_port_map(map).unwrap(),
            BTreeSet::from([mapping("80/udp")])
        );
    }

    #[test]
    fn from_engine_port_map_accepts_host_port_range() {
        let map = PortMap::from([(
            "80/tcp".to_string(),
            Some(vec![PortBinding {
                host_ip: None,
                host_port: Some("8000-9000".to_string()),
            }]),
        )]);
        assert_eq!(
            from_oci_port_map(map).unwrap(),
            BTreeSet::from([mapping("8000-9000:80")])
        );
    }

    #[test]
    fn engine_round_trip() {
        let ports = BTreeSet::from([
            mapping("8080:80"),
            mapping("127.0.0.1:8081:80"),
            mapping("53:53/udp"),
            mapping("443"),
            mapping("8000-9000:3000"),
        ]);
        let (_, bindings) = to_oci_port_maps(ports.clone());
        assert_eq!(from_oci_port_map(bindings).unwrap(), ports);
    }
}
