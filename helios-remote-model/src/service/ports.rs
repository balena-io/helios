//! Service port publishing configuration as defined by the Compose spec.
//!
//! The `ports` field on a service is an array of mappings, each of which is
//! either a short-form string (`"8080:80"`, `"127.0.0.1:8080:80/udp"`), a
//! bare port number, or a long-form object with `target`/`published`/
//! `host_ip`/`protocol`. Port ranges are expanded into individual mappings
//! at parse time. Swarm-only long-form fields (`mode`, `name`,
//! `app_protocol`) are ignored.

use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor, value::MapAccessDeserializer},
};
use std::fmt;
use std::net::IpAddr;

/// Transport protocol of a published port.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PortProtocol {
    #[default]
    Tcp,
    Udp,
}

impl PortProtocol {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "tcp" => Ok(Self::Tcp),
            "udp" => Ok(Self::Udp),
            other => Err(format!("unsupported port protocol `{other}`")),
        }
    }
}

/// A single published container port, after range expansion.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PortMapping {
    /// Container port
    pub target: u16,
    /// Port protocol
    pub protocol: PortProtocol,
    /// Host port. `None` publishes to an ephemeral port.
    pub published: Option<u16>,
    /// Host IP to bind to. `None` binds to all interfaces.
    pub host_ip: Option<String>,
}

/// Service port mapping set. Ordering carries no meaning, so mappings are
/// canonicalized into a set, which also drops duplicates.
#[derive(Debug, Default, PartialEq)]
pub struct Ports(Vec<PortMapping>);

impl Ports {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, PortMapping> {
        self.0.iter()
    }
}

impl IntoIterator for Ports {
    type Item = PortMapping;
    type IntoIter = std::vec::IntoIter<PortMapping>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// Long-form port object used during deserialization. The swarm-only
/// `mode`, `name` and `app_protocol` fields are intentionally not declared,
/// so they are accepted and discarded.
#[derive(Deserialize)]
struct LongPort {
    target: PortOrRange,
    #[serde(default)]
    published: Option<PortOrRange>,
    #[serde(default)]
    host_ip: Option<String>,
    #[serde(default)]
    protocol: Option<String>,
}

/// A port or inclusive port range (`start == end` for a single port),
/// accepted as a number or as a `"80"`/`"8000-9000"` string.
#[derive(Clone, Copy)]
struct PortOrRange(u16, u16);

impl PortOrRange {
    fn len(&self) -> u32 {
        u32::from(self.1) - u32::from(self.0) + 1
    }
}

impl<'de> Deserialize<'de> for PortOrRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PortOrRangeVisitor;

        impl Visitor<'_> for PortOrRangeVisitor {
            type Value = PortOrRange;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a port number or a `port[-port]` string")
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                let port = port_from_number(v).map_err(E::custom)?;
                Ok(PortOrRange(port, port))
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                let port = u64::try_from(v)
                    .map_err(|_| format!("invalid port number `{v}`"))
                    .and_then(port_from_number)
                    .map_err(E::custom)?;
                Ok(PortOrRange(port, port))
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                parse_port_or_range(v).map_err(E::custom)
            }
        }

        deserializer.deserialize_any(PortOrRangeVisitor)
    }
}

enum RawPort {
    Number(u64),
    Short(String),
    Long(LongPort),
}

// Custom Deserialize so that errors inside the long-form object preserve
// their field path (e.g. `ports[0].target`)
impl<'de> Deserialize<'de> for RawPort {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RawPortVisitor;

        impl<'de> Visitor<'de> for RawPortVisitor {
            type Value = RawPort;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a port number, a short-form port string or a long-form port object")
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(RawPort::Number(v))
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                u64::try_from(v)
                    .map(RawPort::Number)
                    .map_err(|_| E::custom(format!("invalid port number `{v}`")))
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(RawPort::Short(v.to_string()))
            }

            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(RawPort::Short(v))
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                LongPort::deserialize(MapAccessDeserializer::new(map)).map(RawPort::Long)
            }
        }

        deserializer.deserialize_any(RawPortVisitor)
    }
}

impl<'de> Deserialize<'de> for Ports {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: Vec<RawPort> = Vec::deserialize(deserializer)?;
        let mut ports = Vec::new();
        for entry in raw {
            ports.extend(parse_raw_port(entry).map_err(serde::de::Error::custom)?);
        }

        // Canonicalize order and drop duplicates so the result is stable
        // against reorderings in the remote composition.
        ports.sort();
        ports.dedup();

        Ok(Ports(ports))
    }
}

fn parse_raw_port(raw: RawPort) -> Result<Vec<PortMapping>, String> {
    match raw {
        RawPort::Number(n) => {
            let port = port_from_number(n)?;
            Ok(vec![PortMapping {
                target: port,
                published: None,
                host_ip: None,
                protocol: PortProtocol::default(),
            }])
        }
        RawPort::Short(s) => parse_short(&s),
        RawPort::Long(l) => parse_long(l),
    }
}

/// Parse a short-form mapping `[HOST_IP:][HOST_SPEC:]CONTAINER_SPEC[/PROTOCOL]`
/// where the port specs may be ranges.
fn parse_short(s: &str) -> Result<Vec<PortMapping>, String> {
    let (spec, protocol) = match s.rsplit_once('/') {
        Some((spec, proto)) => (spec, PortProtocol::parse(proto)?),
        None => (s, PortProtocol::default()),
    };

    // Split from the right so an unbracketed IPv6 host address (which
    // itself contains `:`) is left intact as the remainder.
    let mut parts = spec.rsplitn(3, ':');
    let target_part = parts.next().unwrap_or_default();
    let published_part = parts.next();
    let host_ip_part = parts.next();

    let target = parse_port_or_range(target_part)?;

    let published = match published_part {
        None => None,
        // an empty host port is only valid together with a host IP
        // (`127.0.0.1::80`), meaning publish to an ephemeral port
        Some("") if host_ip_part.is_none() => {
            return Err(format!("invalid port mapping `{s}`"));
        }
        Some("") => None,
        Some(published) => Some(parse_port_or_range(published)?),
    };

    let host_ip = host_ip_part.map(parse_host_ip).transpose()?.flatten();

    expand(target, published, host_ip, protocol)
}

fn parse_long(l: LongPort) -> Result<Vec<PortMapping>, String> {
    let LongPort {
        target,
        published,
        host_ip,
        protocol,
    } = l;

    let protocol = protocol
        .as_deref()
        .map(PortProtocol::parse)
        .transpose()?
        .unwrap_or_default();
    let host_ip = host_ip.as_deref().map(parse_host_ip).transpose()?.flatten();

    expand(target, published, host_ip, protocol)
}

/// Expand a range mapping into individual single-port mappings.
/// - single target + host range: publish the container port on every host
///   port in the range
/// - target range + host range: matched pairwise (lengths must agree)
fn expand(
    target: PortOrRange,
    published: Option<PortOrRange>,
    host_ip: Option<String>,
    protocol: PortProtocol,
) -> Result<Vec<PortMapping>, String> {
    let published = match published {
        None => {
            return Ok((target.0..=target.1)
                .map(|port| PortMapping {
                    target: port,
                    published: None,
                    host_ip: host_ip.clone(),
                    protocol,
                })
                .collect());
        }
        Some(published) => published,
    };

    if target.len() == 1 {
        return Ok((published.0..=published.1)
            .map(|host_port| PortMapping {
                target: target.0,
                published: Some(host_port),
                host_ip: host_ip.clone(),
                protocol,
            })
            .collect());
    }

    if published.len() != target.len() {
        return Err(format!(
            "invalid port mapping: container range {}-{} and host range {}-{} must have the same length",
            target.0, target.1, published.0, published.1
        ));
    }

    Ok((target.0..=target.1)
        .zip(published.0..=published.1)
        .map(|(target, published)| PortMapping {
            target,
            published: Some(published),
            host_ip: host_ip.clone(),
            protocol,
        })
        .collect())
}

fn port_from_number(n: u64) -> Result<u16, String> {
    u16::try_from(n)
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| format!("invalid port number `{n}`"))
}

fn parse_port(s: &str) -> Result<u16, String> {
    s.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| format!("invalid port number `{s}`"))
}

fn parse_port_or_range(s: &str) -> Result<PortOrRange, String> {
    match s.split_once('-') {
        Some((start, end)) => {
            let start = parse_port(start)?;
            let end = parse_port(end)?;
            if start > end {
                return Err(format!("invalid port range `{s}`"));
            }
            Ok(PortOrRange(start, end))
        }
        None => {
            let port = parse_port(s)?;
            Ok(PortOrRange(port, port))
        }
    }
}

/// Validate a host IP, accepting bracketed IPv6 (`[::1]`) but returning the
/// unbracketed form. The IP `0.0.0.0` is normalized to `None` as is equivalent to
/// "bind all interfaces"
fn parse_host_ip(s: &str) -> Result<Option<String>, String> {
    let ip = s
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(s);
    let ip = ip
        .parse::<IpAddr>()
        .map_err(|_| format!("invalid host IP address `{s}`"))?;
    if ip.is_unspecified() && ip.is_ipv4() {
        return Ok(None);
    }
    Ok(Some(ip.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ports(value: serde_json::Value) -> Ports {
        serde_json::from_value(value).unwrap()
    }

    fn mapping(
        target: u16,
        published: Option<u16>,
        host_ip: Option<&str>,
        protocol: PortProtocol,
    ) -> PortMapping {
        PortMapping {
            target,
            published,
            host_ip: host_ip.map(str::to_string),
            protocol,
        }
    }

    #[test]
    fn short_form_container_port_only() {
        let p = ports(json!(["3000"]));
        assert_eq!(
            p.0,
            Vec::from([mapping(3000, None, None, PortProtocol::Tcp)])
        );
    }

    #[test]
    fn number_form() {
        assert_eq!(ports(json!([8080])), ports(json!(["8080"])));
    }

    #[test]
    fn short_form_host_and_container() {
        let p = ports(json!(["8080:80"]));
        assert_eq!(
            p.0,
            Vec::from([mapping(80, Some(8080), None, PortProtocol::Tcp)])
        );
    }

    #[test]
    fn short_form_with_protocol() {
        let p = ports(json!(["6060:6060/udp", "1234:1234/tcp"]));
        // Canonicalized by container port: 1234 sorts before 6060.
        assert_eq!(
            p.0,
            Vec::from([
                mapping(1234, Some(1234), None, PortProtocol::Tcp),
                mapping(6060, Some(6060), None, PortProtocol::Udp),
            ])
        );
    }

    #[test]
    fn short_form_with_host_ip() {
        let p = ports(json!(["127.0.0.1:8001:8001"]));
        assert_eq!(
            p.0,
            Vec::from([mapping(
                8001,
                Some(8001),
                Some("127.0.0.1"),
                PortProtocol::Tcp
            )])
        );
    }

    #[test]
    fn short_form_ephemeral_with_host_ip() {
        let p = ports(json!(["127.0.0.1::80"]));
        assert_eq!(
            p.0,
            Vec::from([mapping(80, None, Some("127.0.0.1"), PortProtocol::Tcp)])
        );
    }

    #[test]
    fn short_form_ipv6_host_ip() {
        // unbracketed and bracketed IPv6 addresses parse to the same mapping
        assert_eq!(
            ports(json!(["::1:8080:80"])),
            ports(json!(["[::1]:8080:80"]))
        );
        let p = ports(json!(["[::1]:8080:80"]));
        assert_eq!(
            p.0,
            Vec::from([mapping(80, Some(8080), Some("::1"), PortProtocol::Tcp)])
        );
    }

    #[test]
    fn explicit_ipv6_binding_to_all_interfaces() {
        // unbracketed and bracketed IPv6 addresses parse to the same mapping
        assert_eq!(ports(json!([":::8080:80"])), ports(json!(["[::]:8080:80"])));
        let p = ports(json!([":::8080:80"]));
        assert_eq!(
            p.0,
            Vec::from([mapping(80, Some(8080), Some("::"), PortProtocol::Tcp)])
        );
    }

    #[test]
    fn ipv4_catch_all_host_ip_normalizes_to_none() {
        let p = ports(json!(["0.0.0.0:8080:80"]));
        assert_eq!(
            p.0,
            Vec::from([mapping(80, Some(8080), None, PortProtocol::Tcp)])
        );
        assert_eq!(p, ports(json!(["8080:80"])));
    }

    #[test]
    fn short_form_container_range_expands() {
        let p = ports(json!(["3000-3002"]));
        assert_eq!(
            p.0,
            Vec::from([
                mapping(3000, None, None, PortProtocol::Tcp),
                mapping(3001, None, None, PortProtocol::Tcp),
                mapping(3002, None, None, PortProtocol::Tcp),
            ])
        );
    }

    #[test]
    fn short_form_matched_ranges_expand_pairwise() {
        let p = ports(json!(["9090-9091:8080-8081"]));
        assert_eq!(
            p.0,
            Vec::from([
                mapping(8080, Some(9090), None, PortProtocol::Tcp),
                mapping(8081, Some(9091), None, PortProtocol::Tcp),
            ])
        );
    }

    #[test]
    fn short_form_range_with_host_ip_expands() {
        let p = ports(json!(["127.0.0.1:5000-5001:5000-5001"]));
        assert_eq!(
            p.0,
            Vec::from([
                mapping(5000, Some(5000), Some("127.0.0.1"), PortProtocol::Tcp),
                mapping(5001, Some(5001), Some("127.0.0.1"), PortProtocol::Tcp),
            ])
        );
    }

    #[test]
    fn short_form_host_range_to_single_port_expands() {
        let p = ports(json!(["8000-8002:80"]));
        assert_eq!(
            p.0,
            Vec::from([
                mapping(80, Some(8000), None, PortProtocol::Tcp),
                mapping(80, Some(8001), None, PortProtocol::Tcp),
                mapping(80, Some(8002), None, PortProtocol::Tcp),
            ])
        );
    }

    #[test]
    fn long_form_full() {
        let p = ports(json!([{
            "target": 80,
            "published": "8080",
            "host_ip": "127.0.0.1",
            "protocol": "udp"
        }]));
        assert_eq!(
            p.0,
            Vec::from([mapping(
                80,
                Some(8080),
                Some("127.0.0.1"),
                PortProtocol::Udp
            )])
        );
    }

    #[test]
    fn long_form_defaults() {
        let p = ports(json!([{ "target": 80 }]));
        assert_eq!(p.0, Vec::from([mapping(80, None, None, PortProtocol::Tcp)]));
    }

    #[test]
    fn long_form_published_range_expands() {
        let p = ports(json!([{ "target": 80, "published": "8000-8002" }]));
        assert_eq!(
            p.0,
            Vec::from([
                mapping(80, Some(8000), None, PortProtocol::Tcp),
                mapping(80, Some(8001), None, PortProtocol::Tcp),
                mapping(80, Some(8002), None, PortProtocol::Tcp),
            ])
        );
    }

    #[test]
    fn long_form_target_range_expands() {
        let p = ports(json!([{ "target": "80-81", "published": "8080-8081" }]));
        assert_eq!(
            p.0,
            Vec::from([
                mapping(80, Some(8080), None, PortProtocol::Tcp),
                mapping(81, Some(8081), None, PortProtocol::Tcp),
            ])
        );
    }

    #[test]
    fn long_form_ignores_swarm_fields() {
        let p = ports(json!([{
            "target": 80,
            "published": 8080,
            "mode": "ingress",
            "name": "web",
            "app_protocol": "http"
        }]));
        assert_eq!(
            p.0,
            Vec::from([mapping(80, Some(8080), None, PortProtocol::Tcp)])
        );
    }

    #[test]
    fn rejects_invalid_mappings() {
        for invalid in [
            json!(["0"]),
            json!([0]),
            json!(["65536"]),
            json!(["8080:0"]),
            json!(["80/icmp"]),
            json!(["9000-8000:80"]),
            json!(["8080-8081:80-82"]),
            json!(["host:8080:80"]),
            json!(["127.0.0.1:8080"]),
            json!([":::8080"]),
            json!([":80"]),
            json!([""]),
            json!([{ "target": 80, "protocol": "icmp" }]),
            json!([{ "target": "80-82", "published": "8080-8081" }]),
            json!([{ "target": 80, "host_ip": "not-an-ip" }]),
            json!([{ "published": 8080 }]),
        ] {
            assert!(
                serde_json::from_value::<Ports>(invalid.clone()).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn rejects_mismatched_range_lengths_with_message() {
        let err = serde_json::from_value::<Ports>(json!(["8080-8081:80-82"])).unwrap_err();
        assert!(err.to_string().contains("must have the same length"));
    }

    #[test]
    fn empty_default() {
        let p = ports(json!([]));
        assert!(p.is_empty());
    }

    #[test]
    fn duplicate_mappings_are_deduplicated() {
        let p = ports(json!(["8080:80", "8080:80", "8080:80/udp"]));
        assert_eq!(
            p.0,
            Vec::from([
                mapping(80, Some(8080), None, PortProtocol::Tcp),
                mapping(80, Some(8080), None, PortProtocol::Udp),
            ])
        );
    }
}
