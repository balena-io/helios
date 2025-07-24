use serde::{Deserialize, Serialize};
use std::fmt::{self, Display};
use std::net::{AddrParseError, IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::legacy::LegacyConfig;
use crate::remote::RemoteConfig;
use crate::state::models::Uuid;

/// Local API listen address
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum LocalAddress {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

impl Display for LocalAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LocalAddress::Tcp(socket_addr) => socket_addr.fmt(f),
            LocalAddress::Unix(path) => path.as_path().display().fmt(f),
        }
    }
}

impl FromStr for LocalAddress {
    type Err = AddrParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<SocketAddr>()
            .map(LocalAddress::Tcp)
            .or_else(|_| Ok(LocalAddress::Unix(Path::new(s).to_path_buf())))
    }
}

impl Default for LocalAddress {
    fn default() -> Self {
        LocalAddress::Tcp(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            48484,
        ))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct Config {
    #[serde(default)]
    pub uuid: Uuid,
    #[serde(default)]
    pub local_address: LocalAddress,
    #[serde(default)]
    pub remote: RemoteConfig,
    #[serde(default)]
    pub legacy: LegacyConfig,
}
