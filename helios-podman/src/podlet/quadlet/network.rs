// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Adapted from https://github.com/containers/podlet/
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    ops::{Not, Range},
    str::FromStr,
};

use ipnet::IpNet;
use serde::{Serialize, Serializer};
use thiserror::Error;

use crate::podlet::serde::quadlet::seq_quote_whitespace;

use super::{Downgrade, DowngradeError, PodmanVersion, push_arg};

#[derive(Serialize, Debug, Default, Clone, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct Network {
    /// The (optional) name of the Podman volume.
    #[allow(clippy::struct_field_names)]
    pub network_name: Option<String>,

    /// If enabled, disables the DNS plugin for this network.
    #[serde(rename = "DisableDNS", skip_serializing_if = "Not::not")]
    pub disable_dns: bool,

    /// Set network-scoped DNS resolver/nameserver for containers in this network.
    #[serde(rename = "DNS")]
    pub dns: Vec<String>,

    /// Driver to manage the network.
    pub driver: Option<String>,

    /// Define a gateway for the subnet.
    pub gateway: Vec<IpAddr>,

    /// Maps to the `network_interface` option in the network config.
    pub interface_name: Option<String>,

    /// Restrict external access of this network.
    #[serde(skip_serializing_if = "Not::not")]
    pub internal: bool,

    /// Set the ipam driver (IP Address Management Driver) for the network.
    #[serde(rename = "IPAMDriver")]
    pub ipam_driver: Option<String>,

    /// Allocate container IP from a range.
    #[serde(rename = "IPRange")]
    pub ip_range: Vec<IpRange>,

    /// Enable IPv6 (Dual Stack) networking.
    #[serde(rename = "IPv6", skip_serializing_if = "Not::not")]
    pub ipv6: bool,

    /// Set one or more OCI labels on the network.
    #[serde(serialize_with = "seq_quote_whitespace")]
    pub label: Vec<String>,

    /// Set driver specific options.
    pub options: Vec<String>,

    /// This key contains a list of arguments passed directly to the end of the `podman network create`
    /// command in the generated file, right before the name of the network in the command line.
    pub podman_args: Option<String>,

    /// The subnet in CIDR notation.
    pub subnet: Vec<IpNet>,
}

impl Network {
    /// Add `--{flag} {arg}` to `PodmanArgs=`.
    fn push_arg(&mut self, flag: &str, arg: &str) {
        let podman_args = self.podman_args.get_or_insert_default();
        push_arg(podman_args, flag, arg);
    }
}

impl Downgrade for Network {
    fn downgrade(&mut self, version: PodmanVersion) -> Result<(), DowngradeError> {
        if version < PodmanVersion::V5_6
            && let Some(interface_name) = self.interface_name.take()
        {
            self.push_arg("interface-name", &interface_name);
        }

        if version < PodmanVersion::V4_7 {
            for dns in std::mem::take(&mut self.dns) {
                self.push_arg("dns", &dns);
            }
        }

        if version < PodmanVersion::V4_6
            && let Some(podman_args) = self.podman_args.take()
        {
            return Err(DowngradeError::Option {
                quadlet_option: "PodmanArgs",
                value: podman_args,
                supported_version: PodmanVersion::V4_6,
            });
        }

        Ok(())
    }
}

/// Valid forms for `IPRange=` Quadlet [`Network`] option values.
#[derive(Debug, Clone, PartialEq)]
pub enum IpRange {
    Cidr(IpNet),
    Ipv4Range(Range<Ipv4Addr>),
    Ipv6Range(Range<Ipv6Addr>),
}

impl From<IpNet> for IpRange {
    fn from(value: IpNet) -> Self {
        Self::Cidr(value)
    }
}

impl Serialize for IpRange {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Cidr(subnet) => subnet.serialize(serializer),
            Self::Ipv4Range(Range { start, end }) => format!("{start}-{end}").serialize(serializer),
            Self::Ipv6Range(Range { start, end }) => format!("{start}-{end}").serialize(serializer),
        }
    }
}

impl FromStr for IpRange {
    type Err = ParseIpRangeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some((start, end)) = s.split_once('-') {
            let start = start.parse().map_err(|source| ParseIpRangeError::IpAddr {
                source,
                ip_address: start.into(),
            })?;
            match start {
                IpAddr::V4(start) => {
                    let end = end.parse().map_err(|source| ParseIpRangeError::Ipv4Addr {
                        source,
                        ip_address: end.into(),
                    })?;
                    Ok(Self::Ipv4Range(start..end))
                }
                IpAddr::V6(start) => {
                    let end = end.parse().map_err(|source| ParseIpRangeError::Ipv6Addr {
                        source,
                        ip_address: end.into(),
                    })?;
                    Ok(Self::Ipv6Range(start..end))
                }
            }
        } else {
            Ok(Self::Cidr(s.parse()?))
        }
    }
}

/// Error which can be returned when parsing an [`IpRange`].
/// It must be in CIDR notation or in `<start-IP>-<end-IP>` syntax.
#[derive(Error, Debug)]
pub enum ParseIpRangeError {
    #[error("invalid subnet, must be in CIDR notation or in `<start-IP>-<end-IP>` syntax")]
    Cidr(#[from] ipnet::AddrParseError),
    #[error("invalid IP address: {ip_address}")]
    IpAddr {
        source: std::net::AddrParseError,
        ip_address: String,
    },
    #[error("invalid IPv4 address: {ip_address}")]
    Ipv4Addr {
        source: std::net::AddrParseError,
        ip_address: String,
    },
    #[error("invalid IPv6 address: {ip_address}")]
    Ipv6Addr {
        source: std::net::AddrParseError,
        ip_address: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_default_empty() -> Result<(), crate::podlet::serde::quadlet::Error> {
        let network = Network::default();
        assert_eq!(
            crate::podlet::serde::quadlet::to_string_join_all(network)?,
            "[Network]\n"
        );
        Ok(())
    }
}
