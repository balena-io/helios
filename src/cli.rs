use axum::http::Uri;
use clap::{CommandFactory, Parser};
use std::num::ParseIntError;
use std::time::Duration;

use crate::api::LocalAddress;
use crate::types::{ApiKey, Uuid};

pub use clap::error::ErrorKind;

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
        env = "HELIOS_REMOTE_REQUEST_TIMEOUT",
        long = "remote-request-timeout",
        value_name = "ms",
        value_parser = parse_duration,
        requires = "remote_api_endpoint"
    )]
    pub remote_request_timeout: Option<Duration>,

    /// Remote target state poll interval in milliseconds
    #[arg(
        env = "HELIOS_REMOTE_POLL_INTERVAL",
        long = "remote-poll-interval",
        value_name = "ms",
        value_parser = parse_duration,
        requires = "remote_api_endpoint"
    )]
    pub remote_poll_interval: Option<Duration>,

    /// Remote rate limiting interval in milliseconds
    #[arg(
        env = "HELIOS_REMOTE_POLL_MIN_INTERVAL",
        long = "remote-poll-min-interval",
        value_name = "ms",
        value_parser = parse_duration,
        requires = "remote_api_endpoint"
    )]
    pub remote_poll_min_interval: Option<Duration>,

    /// Remote target state poll max jitter in milliseconds
    #[arg(
        env = "HELIOS_REMOTE_POLL_MAX_JITTER",
        long = "remote-poll-max-jitter",
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

    /// Supervisor version
    #[arg(
        env = "HELIOS_PROVISIONING_SUPERVISOR_VERSION",
        long = "provisioning-supervisor-version",
        value_name = "str",
        requires = "provisioning_key"
    )]
    pub provisioning_supervisor_version: Option<String>,

    /// Host OS name and version, eg. "balenaOS 6.5.39+rev1"
    #[arg(
        env = "HELIOS_PROVISIONING_OS_VERSION",
        long = "provisioning-os-version",
        value_name = "str",
        requires = "provisioning_key"
    )]
    pub provisioning_os_version: Option<String>,

    /// Host OS variant, eg. "prod", "dev"
    #[arg(
        env = "HELIOS_PROVISIONING_OS_VARIANT",
        long = "provisioning-os-variant",
        value_name = "str",
        requires = "provisioning_key"
    )]
    pub provisioning_os_variant: Option<String>,

    /// MAC address
    #[arg(
        env = "HELIOS_PROVISIONING_MAC_ADDRESS",
        long = "provisioning-mac-address",
        value_name = "str",
        requires = "provisioning_key"
    )]
    pub provisioning_mac_address: Option<String>,
}

impl Cli {
    // this is currently unused but seems useful and was non-obvious
    // how to do this, so i'm keeping it around in case we need it.
    // just make sure to make it `pub` and remove the underscore.
    fn _exit_with_error(&self, kind: ErrorKind, message: &str) -> ! {
        let mut cmd = Self::command();
        cmd.error(kind, message).exit()
    }
}

pub fn parse() -> Cli {
    Parser::parse()
}
