use axum::http::{uri::InvalidUri, Uri};
use clap::{Args, Parser, Subcommand};
use std::net::IpAddr;

fn parse_uri(s: &str) -> Result<Uri, InvalidUri> {
    s.parse()
}

#[derive(Clone, Debug, Args)]
pub struct RemoteArgs {
    #[arg(
        long = "remote-poll-interval-ms",
        value_name = "poll_interval_ms",
        help = "Remote API poll interval in milliseconds",
        env = "HELIOS_REMOTE_POLL_INTERVAL_MS"
    )]
    pub remote_poll_interval_ms: Option<u64>,

    #[arg(
        long = "remote-request-timeout-ms",
        value_name = "request_timeout_ms",
        help = "Remote API request timeout in milliseconds",
        env = "HELIOS_REMOTE_REQUEST_TIMEOUT_MS"
    )]
    pub remote_request_timeout_ms: Option<u64>,

    #[arg(
        long = "remote-min-interval-ms",
        value_name = "min_interval_ms",
        help = "API rate limiting interval in milliseconds",
        env = "HELIOS_REMOTE_MIN_INTERVAL_MS"
    )]
    pub remote_min_interval_ms: Option<u64>,

    #[arg(
        long = "remote-max-poll-jitter-ms",
        value_name = "max_poll_jitter_ms",
        help = "API target state poll max jitter in milliseconds",
        env = "HELIOS_REMOTE_MAX_POLL_JITTER_MS"
    )]
    pub remote_max_poll_jitter_ms: Option<u64>,
}

#[derive(Clone, Debug, Parser)]
#[command(about = "Next-gen experimental balenaSupervisor")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    #[arg(
        long = "uuid",
        value_name = "uuid",
        help = "Device UUID",
        env = "HELIOS_UUID"
    )]
    pub uuid: Option<String>,

    #[arg(
        long = "local-port",
        value_name = "port",
        help = "Local API listen port",
        env = "HELIOS_LOCAL_PORT"
    )]
    pub local_port: Option<u16>,

    #[arg(
        long = "local-address",
        value_name = "address",
        help = "Local API listen address",
        env = "HELIOS_LOCAL_ADDRESS"
    )]
    pub local_address: Option<IpAddr>,

    #[arg(
        long = "remote-api-endpoint", 
        value_name = "uri", 
        help = "Remote API endpoint", 
        value_parser = parse_uri,
        env = "HELIOS_REMOTE_API_ENDPOINT"
    )]
    pub remote_api_endpoint: Option<Uri>,

    #[arg(
        long = "remote-api-key",
        value_name = "key",
        help = "Remote API key",
        env = "HELIOS_REMOTE_API_KEY"
    )]
    pub remote_api_key: Option<String>,

    #[command(flatten)]
    pub remote: RemoteArgs,

    #[arg(
        long = "fallback-address", 
        value_name = "uri", 
        help = "Fallback URI to redirect unsupported API requests", 
        value_parser = parse_uri,
        env = "HELIOS_FALLBACK_ADDRESS"
    )]
    pub fallback_address: Option<Uri>,

    #[arg(
        long = "fallback-api-key",
        value_name = "key",
        help = "API key to perform requests to fallback URI",
        env = "HELIOS_FALLBACK_API_KEY"
    )]
    pub fallback_api_key: Option<String>,
}

#[derive(Clone, Debug, Subcommand)]
pub enum Commands {
    /// Provision device to remote endpoint (TODO!)
    Register {
        #[command(flatten)]
        remote: RemoteArgs,

        #[arg(
            long = "provisioning-key",
            value_name = "key",
            help = "Provisioning key"
        )]
        provisioning_key: String,
    },
}
