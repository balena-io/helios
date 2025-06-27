use anyhow::Result;
use clap::Parser;
use hyper::Uri;

fn parse_uri(s: &str) -> Result<Uri> {
    let uri = s.parse()?;
    Ok(uri)
}

#[derive(Clone, Debug, Parser)]
/// Local API configurations
pub struct Local {
    #[arg(
        long = "local-port",
        env = "LOCAL_PORT",
        default_value = "48484",
        value_name = "port",
        help = "Local API listen port"
    )]
    pub port: u16,
}

#[derive(Clone, Debug, Parser)]
/// Remote API configurations
pub struct Remote {
    #[arg(long = "remote-api-endpoint", env = "REMOTE_API_ENDPOINT", default_value = "https://api.balena-cloud.com", value_name = "uri", help = "Remote API endpoint", value_parser = parse_uri)]
    pub api_endpoint: Uri,

    #[arg(
        long = "remote-api-key",
        env = "REMOTE_API_KEY",
        value_name = "key",
        help = "Remote API key"
    )]
    pub api_key: Option<String>,

    #[arg(
        long = "remote-poll-interval-ms",
        env = "REMOTE_POLL_INTERVAL",
        default_value = "900000",
        value_name = "poll_interval_ms",
        help = "Remote API poll interval in milliseconds"
    )]
    pub poll_interval_ms: u64,

    #[arg(
        long = "remote-request-timeout-ms",
        env = "REMOTE_REQUEST_TIMEOUT",
        default_value = "59000",
        value_name = "request_timeout_ms",
        help = "Remote API request timeout in milliseconds"
    )]
    pub request_timeout_ms: u64,

    #[arg(
        long = "remote-min-interval-ms",
        default_value = "10000",
        value_name = "min_interval_ms",
        help = "API rate limiting interval in milliseconds"
    )]
    pub min_interval_ms: u64,

    #[arg(
        long = "remote-max-jitter-delay-ms",
        default_value = "60000",
        value_name = "max_jitter_delay_ms",
        help = "API target state poll max jitter delay in milliseconds"
    )]
    pub max_jitter_delay_ms: u64,
}

#[derive(Clone, Debug, Parser)]
#[command(about = "Next-gen experimental balenaSupervisor")]
pub struct Config {
    #[arg(
        long = "uuid",
        env = "DEVICE_UUID",
        value_name = "uuid",
        help = "Device UUID"
    )]
    pub uuid: String,

    #[command(flatten)]
    pub local: Local,

    #[command(flatten)]
    pub remote: Remote,

    #[arg(long = "fallback-address", env = "FALLBACK_ADDRESS", value_name = "uri", help = "Fallback URI to redirect unsupported API requests", value_parser = parse_uri)]
    pub fallback_address: Option<Uri>,
}
