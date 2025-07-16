use axum::http::Uri;
use serde::{Deserialize, Serialize};
use std::fmt::{self, Display};
use std::net::{AddrParseError, IpAddr, Ipv4Addr, SocketAddr};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

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

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct Uuid(String);

impl Deref for Uuid {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Default for Uuid {
    fn default() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }
}

impl Display for Uuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<String> for Uuid {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<Uuid> for String {
    fn from(value: Uuid) -> Self {
        value.0
    }
}

/// Remote API configurations
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RemoteConfig {
    #[serde(
        deserialize_with = "deserialize_optional_uri",
        serialize_with = "serialize_optional_uri",
        default
    )]
    pub api_endpoint: Option<Uri>,
    pub api_key: Option<String>,
    #[serde(
        deserialize_with = "deserialize_duration_from_ms",
        serialize_with = "serialize_duration_to_ms"
    )]
    pub poll_interval: Duration,
    #[serde(
        deserialize_with = "deserialize_duration_from_ms",
        serialize_with = "serialize_duration_to_ms"
    )]
    pub request_timeout: Duration,
    #[serde(
        deserialize_with = "deserialize_duration_from_ms",
        serialize_with = "serialize_duration_to_ms"
    )]
    pub min_interval: Duration,
    #[serde(
        deserialize_with = "deserialize_duration_from_ms",
        serialize_with = "serialize_duration_to_ms"
    )]
    pub max_poll_jitter: Duration,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            api_endpoint: None,
            api_key: None,
            poll_interval: Duration::from_millis(900000),
            request_timeout: Duration::from_millis(59000),
            min_interval: Duration::from_millis(10000),
            max_poll_jitter: Duration::from_millis(60000),
        }
    }
}

/// Fallback API configurations
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FallbackConfig {
    #[serde(
        deserialize_with = "deserialize_optional_uri",
        serialize_with = "serialize_optional_uri",
        default
    )]
    pub address: Option<Uri>,
    pub api_key: Option<String>,
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
    pub fallback: FallbackConfig,
}

fn deserialize_optional_uri<'de, D>(deserializer: D) -> std::result::Result<Option<Uri>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let s: Option<String> = Option::deserialize(deserializer)?;
    match s {
        Some(s) => s.parse().map(Some).map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

fn serialize_optional_uri<S>(
    uri: &Option<Uri>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match uri {
        Some(uri) => serializer.serialize_some(&uri.to_string()),
        None => serializer.serialize_none(),
    }
}

fn deserialize_duration_from_ms<'de, D>(deserializer: D) -> std::result::Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let ms: u64 = serde::Deserialize::deserialize(deserializer)?;
    Ok(Duration::from_millis(ms))
}

fn serialize_duration_to_ms<S>(
    duration: &Duration,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_u64(duration.as_millis() as u64)
}
