use axum::http::Uri;
use clap::{Parser, Subcommand};
use std::convert::Infallible;
use std::num::ParseIntError;
use std::path::PathBuf;
use std::time::Duration;
use std::{fs, io};
use tracing::debug;

use crate::config::{Config, LocalAddress};
use crate::state::models::Uuid;

fn parse_uuid(s: &str) -> Result<Uuid, Infallible> {
    Ok(s.to_string().into())
}

fn parse_duration(s: &str) -> Result<Duration, ParseIntError> {
    let millis: u64 = s.parse()?;
    Ok(Duration::from_millis(millis))
}

#[derive(Clone, Debug, Parser)]
#[command(version, about, long_about = None)] // read from Cargo.toml
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Device UUID
    #[arg(
        long = "uuid",
        value_name = "uuid",
        value_parser = parse_uuid,
        env = "HELIOS_UUID"
    )]
    uuid: Option<Uuid>,

    /// Local API listen address
    #[arg(
        long = "local-address",
        value_name = "address",
        env = "HELIOS_LOCAL_ADDRESS",
        default_value_t
    )]
    local_address: LocalAddress,

    /// Remote API endpoint
    #[arg(
        long = "remote-api-endpoint",
        value_name = "uri",
        env = "HELIOS_REMOTE_API_ENDPOINT"
    )]
    remote_api_endpoint: Option<Uri>,

    /// API key to use for authentication with remote
    #[arg(
        long = "remote-api-key",
        value_name = "key",
        env = "HELIOS_REMOTE_API_KEY",
        requires = "remote_api_endpoint"
    )]
    remote_api_key: Option<String>,

    /// Remote request timeout in milliseconds
    #[arg(
        long = "remote-request-timeout-ms",
        value_name = "request_timeout_ms",
        value_parser = parse_duration,
        env = "HELIOS_REMOTE_REQUEST_TIMEOUT_MS",
        requires = "remote_api_endpoint",
        default_value = "59000"
    )]
    remote_request_timeout_ms: Duration,

    /// Remote poll interval in milliseconds
    #[arg(
        long = "remote-poll-interval-ms",
        value_name = "poll_interval_ms",
        value_parser = parse_duration,
        env = "HELIOS_REMOTE_POLL_INTERVAL_MS",
        requires = "remote_api_endpoint",
        default_value = "900000"
    )]
    remote_poll_interval_ms: Duration,

    /// Remote rate limiting interval in milliseconds
    #[arg(
        long = "remote-min-interval-ms",
        value_name = "min_interval_ms",
        value_parser = parse_duration,
        env = "HELIOS_REMOTE_MIN_INTERVAL_MS",
        requires = "remote_api_endpoint",
        default_value = "10000"
    )]
    remote_min_interval_ms: Duration,

    /// Remote target state poll max jitter in milliseconds
    #[arg(
        long = "remote-max-poll-jitter-ms",
        value_name = "max_poll_jitter_ms",
        value_parser = parse_duration,
        env = "HELIOS_REMOTE_MAX_POLL_JITTER_MS",
        requires = "remote_api_endpoint",
        default_value = "60000"
    )]
    remote_max_poll_jitter_ms: Duration,

    /// Fallback URI to redirect unsupported API requests
    #[arg(
        long = "fallback-address",
        value_name = "uri",
        env = "HELIOS_FALLBACK_ADDRESS"
    )]
    fallback_address: Option<Uri>,

    /// API key to use for authentication with fallback
    #[arg(
        long = "fallback-api-key",
        value_name = "key",
        requires = "fallback_address",
        env = "HELIOS_FALLBACK_API_KEY"
    )]
    fallback_api_key: Option<String>,
}

#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Provision device to remote endpoint (TODO!)
    Register {
        /// Provisioning key
        #[arg(long = "provisioning-key", value_name = "key")]
        provisioning_key: String,
    },
}

pub fn load() -> io::Result<(Option<Command>, Config)> {
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

    let cli = Cli::parse();

    // Apply CLI overrides in order of precedence
    if let Some(uuid) = cli.uuid {
        config.uuid = uuid;
    }
    config.local_address = cli.local_address;

    if let Some(endpoint) = &cli.remote_api_endpoint {
        config.remote.api_endpoint = Some(endpoint.clone());
    }
    if let Some(key) = &cli.remote_api_key {
        config.remote.api_key = Some(key.clone());
    }
    config.remote.request_timeout = cli.remote_request_timeout_ms;
    config.remote.poll_interval = cli.remote_poll_interval_ms;
    config.remote.min_interval = cli.remote_min_interval_ms;
    config.remote.max_poll_jitter = cli.remote_max_poll_jitter_ms;

    if let Some(address) = &cli.fallback_address {
        config.fallback.address = Some(address.clone());
    }
    if let Some(key) = &cli.fallback_api_key {
        config.fallback.api_key = Some(key.clone());
    }

    Ok((cli.command, config))
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
