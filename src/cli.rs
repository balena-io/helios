use axum::http::Uri;
use clap::Parser;
use std::num::ParseIntError;
use std::time::Duration;

use crate::api::LocalAddress;
use crate::state::models::Uuid;

fn parse_duration(s: &str) -> Result<Duration, ParseIntError> {
    let millis: u64 = s.parse()?;
    Ok(Duration::from_millis(millis))
}

#[derive(Clone, Debug, Parser)]
#[command(version, about, long_about = None)] // read from Cargo.toml
pub struct Cli {
    /// Device UUID
    #[arg(long = "uuid", value_name = "uuid", env = "HELIOS_UUID")]
    pub uuid: Option<Uuid>,

    /// Local API listen address
    #[arg(
        long = "local-api-address",
        value_name = "address",
        env = "HELIOS_LOCAL_API_ADDRESS"
    )]
    pub local_api_address: Option<LocalAddress>,

    /// Remote API endpoint
    #[arg(
        long = "remote-api-endpoint",
        value_name = "uri",
        env = "HELIOS_REMOTE_API_ENDPOINT"
    )]
    pub remote_api_endpoint: Option<Uri>,

    /// API key to use for authentication with remote
    #[arg(
        long = "remote-api-key",
        value_name = "key",
        env = "HELIOS_REMOTE_API_KEY",
        requires = "remote_api_endpoint"
    )]
    pub remote_api_key: Option<String>,

    /// Remote request timeout in milliseconds
    #[arg(
        long = "remote-request-timeout-ms",
        value_name = "request_timeout_ms",
        value_parser = parse_duration,
        env = "HELIOS_REMOTE_REQUEST_TIMEOUT_MS",
        requires = "remote_api_endpoint"
    )]
    pub remote_request_timeout_ms: Option<Duration>,

    /// Remote poll interval in milliseconds
    #[arg(
        long = "remote-poll-interval-ms",
        value_name = "poll_interval_ms",
        value_parser = parse_duration,
        env = "HELIOS_REMOTE_POLL_INTERVAL_MS",
        requires = "remote_api_endpoint"
    )]
    pub remote_poll_interval_ms: Option<Duration>,

    /// Remote rate limiting interval in milliseconds
    #[arg(
        long = "remote-min-interval-ms",
        value_name = "min_interval_ms",
        value_parser = parse_duration,
        env = "HELIOS_REMOTE_MIN_INTERVAL_MS",
        requires = "remote_api_endpoint"
    )]
    pub remote_min_interval_ms: Option<Duration>,

    /// Remote target state poll max jitter in milliseconds
    #[arg(
        long = "remote-max-poll-jitter-ms",
        value_name = "max_poll_jitter_ms",
        value_parser = parse_duration,
        env = "HELIOS_REMOTE_MAX_POLL_JITTER_MS",
        requires = "remote_api_endpoint"
    )]
    pub remote_max_poll_jitter_ms: Option<Duration>,

    /// Legacy Supervisor API endpoint URI to redirect unsupported API requests
    #[arg(
        long = "legacy-api-endpoint",
        value_name = "uri",
        env = "HELIOS_LEGACY_API_ENDPOINT",
        requires = "legacy_api_key"
    )]
    pub legacy_api_endpoint: Option<Uri>,

    /// API key to use for authentication with legacy Supervisor
    #[arg(
        long = "legacy-api-key",
        value_name = "key",
        env = "HELIOS_LEGACY_API_KEY",
        requires = "legacy_api_endpoint"
    )]
    pub legacy_api_key: Option<String>,

    /// Provisioning key to use for authenticating with remote during registration
    #[arg(
        long = "provisioning-key",
        value_name = "key",
        env = "HELIOS_PROVISIONING_KEY",
        requires = "remote_api_endpoint",
        requires = "provisioning_fleet",
        requires = "provisioning_device_type"
    )]
    pub provisioning_key: Option<String>,

    /// ID of the fleet to provision this device into
    #[arg(
        long = "provisioning-fleet",
        value_name = "num",
        env = "HELIOS_PROVISIONING_FLEET",
        requires = "provisioning_key"
    )]
    pub provisioning_fleet: Option<u32>, // FIXME: should fleet_uuid

    /// Device type
    #[arg(
        long = "provisioning-device-type",
        value_name = "slug",
        env = "HELIOS_PROVISIONING_DEVICE_TYPE",
        requires = "provisioning_key"
    )]
    pub provisioning_device_type: Option<String>,

    /// Supervisor version
    #[arg(
        long = "provisioning-supervisor-version",
        value_name = "string",
        env = "HELIOS_PROVISIONING_SUPERVISOR_VERSION",
        requires = "provisioning_key"
    )]
    pub provisioning_supervisor_version: Option<String>,

    /// Host OS name and version, eg. "balenaOS 6.5.39+rev1"
    #[arg(
        long = "provisioning-os-version",
        value_name = "string",
        env = "HELIOS_PROVISIONING_OS_VERSION",
        requires = "provisioning_key"
    )]
    pub provisioning_os_version: Option<String>,

    /// Host OS variant, eg. "prod", "dev"
    #[arg(
        long = "provisioning-os-variant",
        value_name = "string",
        env = "HELIOS_PROVISIONING_OS_VARIANT",
        requires = "provisioning_key"
    )]
    pub provisioning_os_variant: Option<String>,

    /// MAC address
    #[arg(
        long = "provisioning-mac-address",
        value_name = "string",
        env = "HELIOS_PROVISIONING_MAC_ADDRESS",
        requires = "provisioning_key"
    )]
    pub provisioning_mac_address: Option<String>,
}

pub fn parse() -> Cli {
    Parser::parse()
}
