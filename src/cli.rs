use axum::http::Uri;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::fmt::{self, Display};
use std::net::{AddrParseError, IpAddr, Ipv4Addr, SocketAddr};
use std::num::ParseIntError;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

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

    /// Device UUID. If not provided, a random value will be generated and used
    /// on each run of this command
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

    /// Legacy Supervisor API endpoint URI to redirect unsupported API requests
    #[arg(
        long = "legacy-address",
        value_name = "uri",
        env = "HELIOS_LEGACY_ADDRESS"
    )]
    legacy_address: Option<Uri>,

    /// API key to use for authentication with legacy Supervisor
    #[arg(
        long = "legacy-api-key",
        value_name = "key",
        env = "HELIOS_LEGACY_API_KEY",
        requires = "legacy_address"
    )]
    legacy_api_key: Option<String>,
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

pub type Args = (
    Option<Uuid>,
    LocalAddress,
    Option<RemoteConfig>,
    Option<LegacyConfig>,
);

pub fn load() -> (Option<Command>, Args) {
    let cli = Cli::parse();

    let uuid = cli.uuid;

    let local_address = cli.local_address;

    let remote = if let Some(api_endpoint) = cli.remote_api_endpoint {
        Some(RemoteConfig {
            api_endpoint,
            api_key: cli.remote_api_key,
            request_timeout: cli.remote_request_timeout_ms,
            poll_interval: cli.remote_poll_interval_ms,
            min_interval: cli.remote_min_interval_ms,
            max_poll_jitter: cli.remote_max_poll_jitter_ms,
        })
    } else {
        None
    };

    let legacy = if let Some(address) = cli.legacy_address {
        Some(LegacyConfig {
            address,
            api_key: cli.legacy_api_key,
        })
    } else {
        None
    };

    (cli.command, (uuid, local_address, remote, legacy))
}
