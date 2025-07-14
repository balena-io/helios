use axum::http::Uri;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;
use std::{fs, io};
use tracing::debug;

use crate::config::{Config, LocalAddress};

#[derive(Clone, Debug, Parser)]
#[command(version, about, long_about = None)] // read from Cargo.toml
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[arg(
        long = "uuid",
        value_name = "uuid",
        help = "Device UUID",
        env = "HELIOS_UUID"
    )]
    uuid: Option<String>,

    #[arg(
        long = "local-address",
        value_name = "address",
        help = "Local API listen address",
        env = "HELIOS_LOCAL_ADDRESS"
    )]
    local_address: Option<LocalAddress>,

    #[arg(
        long = "remote-api-endpoint",
        value_name = "uri",
        help = "Remote API endpoint",
        env = "HELIOS_REMOTE_API_ENDPOINT"
    )]
    remote_api_endpoint: Option<Uri>,

    #[arg(
        long = "remote-api-key",
        value_name = "key",
        help = "Remote API key",
        env = "HELIOS_REMOTE_API_KEY"
    )]
    remote_api_key: Option<String>,

    #[arg(
        long = "remote-poll-interval-ms",
        value_name = "poll_interval_ms",
        help = "Remote API poll interval in milliseconds",
        env = "HELIOS_REMOTE_POLL_INTERVAL_MS"
    )]
    remote_poll_interval_ms: Option<u64>,

    #[arg(
        long = "remote-request-timeout-ms",
        value_name = "request_timeout_ms",
        help = "Remote API request timeout in milliseconds",
        env = "HELIOS_REMOTE_REQUEST_TIMEOUT_MS"
    )]
    remote_request_timeout_ms: Option<u64>,

    #[arg(
        long = "remote-min-interval-ms",
        value_name = "min_interval_ms",
        help = "API rate limiting interval in milliseconds",
        env = "HELIOS_REMOTE_MIN_INTERVAL_MS"
    )]
    remote_min_interval_ms: Option<u64>,

    #[arg(
        long = "remote-max-poll-jitter-ms",
        value_name = "max_poll_jitter_ms",
        help = "API target state poll max jitter in milliseconds",
        env = "HELIOS_REMOTE_MAX_POLL_JITTER_MS"
    )]
    remote_max_poll_jitter_ms: Option<u64>,

    #[arg(
        long = "fallback-address",
        value_name = "uri",
        help = "Fallback URI to redirect unsupported API requests",
        env = "HELIOS_FALLBACK_ADDRESS"
    )]
    fallback_address: Option<Uri>,

    #[arg(
        long = "fallback-api-key",
        value_name = "key",
        help = "API key to perform requests to fallback URI",
        env = "HELIOS_FALLBACK_API_KEY"
    )]
    fallback_api_key: Option<String>,
}

#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Provision device to remote endpoint (TODO!)
    Register {
        #[arg(
            long = "provisioning-key",
            value_name = "key",
            help = "Provisioning key"
        )]
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
        config.uuid = uuid.into();
    }
    if let Some(address) = cli.local_address {
        config.local_address = address;
    }
    if let Some(endpoint) = &cli.remote_api_endpoint {
        config.remote.api_endpoint = Some(endpoint.clone());
    }
    if let Some(key) = &cli.remote_api_key {
        config.remote.api_key = Some(key.clone());
    }
    if let Some(interval) = cli.remote_poll_interval_ms {
        config.remote.poll_interval = Duration::from_millis(interval);
    }
    if let Some(timeout) = cli.remote_request_timeout_ms {
        config.remote.request_timeout = Duration::from_millis(timeout);
    }
    if let Some(min_interval) = cli.remote_min_interval_ms {
        config.remote.min_interval = Duration::from_millis(min_interval);
    }
    if let Some(jitter) = cli.remote_max_poll_jitter_ms {
        config.remote.max_poll_jitter = Duration::from_millis(jitter);
    }
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
