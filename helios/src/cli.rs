use clap::Parser;
use std::num::ParseIntError;
use std::time::Duration;

use crate::api::LocalAddress;
use crate::util::http::Uri;
use crate::util::types::{ApiKey, OperatingSystem, Uuid};

fn parse_duration(s: &str) -> Result<Duration, ParseIntError> {
    let millis: u64 = s.parse()?;
    Ok(Duration::from_millis(millis))
}

#[derive(Clone, Debug, Parser)]
#[command(version, about, long_about = None)] // read from Cargo.toml
pub struct Cli {
    /// Unique identifier for this device
    #[arg(env = "HELIOS_UUID", long = "uuid", value_name = "uuid")]
    pub uuid: Option<Uuid>,

    /// Host OS name and version with metadata, eg. "balenaOS 6.5.39+rev1"
    #[arg(env = "HELIOS_OS_VERSION", long = "os-version", value_name = "str")]
    pub os: Option<OperatingSystem>,

    /// Local API listen address
    #[arg(
        env = "HELIOS_LOCAL_API_ADDRESS",
        long = "local-api-address",
        value_name = "addr"
    )]
    pub local_api_address: Option<LocalAddress>,

    /// Remote API endpoint URI
    #[arg(
        env = "HELIOS_REMOTE_API_ENDPOINT",
        long = "remote-api-endpoint",
        value_name = "uri"
    )]
    pub remote_api_endpoint: Option<Uri>,

    /// API key for authentication with remote
    #[arg(
        env = "HELIOS_REMOTE_API_KEY",
        long = "remote-api-key",
        value_name = "key",
        requires = "uuid",
        requires = "remote_api_endpoint"
    )]
    pub remote_api_key: Option<ApiKey>,

    /// Remote request timeout in milliseconds
    #[arg(
        env = "HELIOS_REMOTE_REQUEST_TIMEOUT_MS",
        long = "remote-request-timeout-ms",
        value_name = "ms",
        value_parser = parse_duration,
        requires = "remote_api_endpoint"
    )]
    pub remote_request_timeout: Option<Duration>,

    /// Remote target state poll interval in milliseconds
    #[arg(
        env = "HELIOS_REMOTE_POLL_INTERVAL_MS",
        long = "remote-poll-interval-ms",
        value_name = "ms",
        value_parser = parse_duration,
        requires = "remote_api_endpoint"
    )]
    pub remote_poll_interval: Option<Duration>,

    /// Remote rate limiting interval in milliseconds
    #[arg(
        env = "HELIOS_REMOTE_POLL_MIN_INTERVAL_MS",
        long = "remote-poll-min-interval-ms",
        value_name = "ms",
        value_parser = parse_duration,
        requires = "remote_api_endpoint"
    )]
    pub remote_poll_min_interval: Option<Duration>,

    /// Remote target state poll max jitter in milliseconds
    #[arg(
        env = "HELIOS_REMOTE_POLL_MAX_JITTER_MS",
        long = "remote-poll-max-jitter-ms",
        value_name = "ms",
        value_parser = parse_duration,
        requires = "remote_api_endpoint"
    )]
    pub remote_poll_max_jitter: Option<Duration>,

    /// URI of legacy Supervisor API
    #[arg(
        env = "HELIOS_LEGACY_API_ENDPOINT",
        long = "legacy-api-endpoint",
        value_name = "uri",
        requires = "legacy_api_key"
    )]
    pub legacy_api_endpoint: Option<Uri>,

    /// API key for authentication with legacy Supervisor API
    #[arg(
        env = "HELIOS_LEGACY_API_KEY",
        long = "legacy-api-key",
        value_name = "key",
        requires = "legacy_api_endpoint"
    )]
    pub legacy_api_key: Option<ApiKey>,

    /// Provisioning key to use for authenticating with remote during registration
    #[arg(
        env = "HELIOS_PROVISIONING_KEY",
        long = "provisioning-key",
        value_name = "key",
        requires = "remote_api_endpoint",
        requires = "provisioning_fleet",
        requires = "provisioning_device_type"
    )]
    pub provisioning_key: Option<String>,

    /// ID of the fleet to provision this device into
    #[arg(
        env = "HELIOS_PROVISIONING_FLEET",
        long = "provisioning-fleet",
        value_name = "int",
        requires = "provisioning_key"
    )]
    pub provisioning_fleet: Option<u32>, // FIXME: should fleet_uuid

    /// Device type
    #[arg(
        env = "HELIOS_PROVISIONING_DEVICE_TYPE",
        long = "provisioning-device-type",
        value_name = "slug",
        requires = "provisioning_key"
    )]
    pub provisioning_device_type: Option<String>,

    /// Host OS name and version, eg. "balenaOS 6.5.39+rev1"
    #[arg(
        env = "HELIOS_PROVISIONING_OS_VERSION",
        long = "provisioning-os-version",
        value_name = "str",
        requires = "provisioning_key"
    )]
    pub provisioning_os_version: Option<String>,

    /// Supervisor version
    #[arg(
        env = "HELIOS_PROVISIONING_SUPERVISOR_VERSION",
        long = "provisioning-supervisor-version",
        value_name = "str",
        requires = "provisioning_key"
    )]
    pub provisioning_supervisor_version: Option<String>,
}

pub fn parse() -> Cli {
    Parser::parse()
}
