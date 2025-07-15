use axum::http::Uri;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use uuid::Uuid;

/// Local API configurations
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LocalConfig {
    pub port: u16,
    pub address: IpAddr,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            port: 48484,
            address: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        }
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

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
/// Fallback API configurations
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
    #[serde(default = "generate_uuid")]
    pub uuid: String,
    #[serde(default)]
    pub local: LocalConfig,
    #[serde(default)]
    pub remote: RemoteConfig,
    #[serde(default)]
    pub fallback: FallbackConfig,
}

fn generate_uuid() -> String {
    Uuid::new_v4().simple().to_string()
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
