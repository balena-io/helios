use anyhow::Result;
use hyper::Uri;
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::time::Duration;
use tracing::debug;
use uuid::Uuid;

use crate::cli::Cli;

#[derive(Clone, Debug, Deserialize, Serialize)]
/// Local API configurations
pub struct Local {
    pub port: u16,
    pub address: IpAddr,
}

impl Default for Local {
    fn default() -> Self {
        Self {
            port: 48484,
            address: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
/// Remote API configurations
pub struct Remote {
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

impl Default for Remote {
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
pub struct Fallback {
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
    pub local: Local,
    #[serde(default)]
    pub remote: Remote,
    #[serde(default)]
    pub fallback: Fallback,
}

fn generate_uuid() -> String {
    Uuid::new_v4().simple().to_string()
}

impl Config {
    pub fn load(cli: &Cli) -> Result<Self> {
        let config_path = get_config_path();

        // Start with default config
        let mut config = if config_path.exists() {
            // Load from file if it exists
            debug!("Loading config from {}", config_path.display());
            let contents = fs::read_to_string(&config_path)?;
            serde_json::from_str::<Config>(&contents).unwrap_or_default()
        } else {
            // Create default config
            Config::default()
        };

        // Apply CLI overrides in order of precedence
        if let Some(uuid) = &cli.uuid {
            config.uuid = uuid.clone();
        }
        if let Some(port) = cli.local_port {
            config.local.port = port;
        }
        if let Some(address) = cli.local_address {
            config.local.address = address;
        }
        if let Some(endpoint) = &cli.remote_api_endpoint {
            config.remote.api_endpoint = Some(endpoint.clone());
        }
        if let Some(key) = &cli.remote_api_key {
            config.remote.api_key = Some(key.clone());
        }
        if let Some(interval) = cli.remote.remote_poll_interval_ms {
            config.remote.poll_interval = Duration::from_millis(interval);
        }
        if let Some(timeout) = cli.remote.remote_request_timeout_ms {
            config.remote.request_timeout = Duration::from_millis(timeout);
        }
        if let Some(min_interval) = cli.remote.remote_min_interval_ms {
            config.remote.min_interval = Duration::from_millis(min_interval);
        }
        if let Some(jitter) = cli.remote.remote_max_poll_jitter_ms {
            config.remote.max_poll_jitter = Duration::from_millis(jitter);
        }
        if let Some(address) = &cli.fallback_address {
            config.fallback.address = Some(address.clone());
        }
        if let Some(key) = &cli.fallback_api_key {
            config.fallback.api_key = Some(key.clone());
        }

        // If UUID is still empty after all sources, generate one
        if config.uuid.is_empty() {
            config.uuid = generate_uuid();
        }

        Ok(config)
    }
}

fn get_config_path() -> PathBuf {
    if let Some(config_dir) = dirs::config_dir() {
        config_dir.join(env!("CARGO_PKG_NAME")).join("config.json")
    } else {
        // Fallback to home directory if config dir is not available
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config")
            .join(env!("CARGO_PKG_NAME"))
            .join("config.json")
    }
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
