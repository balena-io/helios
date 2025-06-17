use anyhow::{Context, Result};
use hyper::Uri;
use std::env;

#[derive(Clone, Debug)]
/// Local API configurations
pub struct Local {
    /// Local listen port
    pub port: u16,
}

impl Local {
    pub fn from_env() -> Result<Self> {
        let port = env::var("BALENA_SUPERVISOR_PORT")
            .unwrap_or_else(|_| "48484".to_string())
            .parse::<u16>()?;

        Ok(Self { port })
    }
}

#[derive(Clone, Debug)]
/// Remote API configurations
pub struct Remote {
    /// Remote API endpoint.
    ///
    /// Defaults to https://api.balena-cloud.com
    pub uri: Uri,

    /// Remote API key
    pub api_key: Option<String>,

    //// Remote API poll interval in milliseconds
    ///
    /// Defaults to 15 mins (900000ms)
    pub poll_interval_ms: u32,

    /// Remote API request timeout
    ///
    /// Defaults to 59 secs
    pub request_timeout_ms: u32,
}

impl Remote {
    /// Load configurations from environment
    pub fn from_env() -> Result<Self> {
        let uri = env::var("BALENA_API_ENDPOINT")
            .unwrap_or_else(|_| "https://api.balena-cloud.com".to_string())
            .parse()?;
        let api_key = env::var("BALENA_API_KEY").ok();
        let poll_interval_ms = env::var("BALENA_POLL_INTERVAL")
            .unwrap_or_else(|_| "900000".to_string())
            .parse::<u32>()?;
        let request_timeout_ms = env::var("BALENA_REQUEST_TIMEOUT")
            .unwrap_or_else(|_| "59000".to_string())
            .parse::<u32>()?;

        Ok(Self {
            uri,
            api_key,
            poll_interval_ms,
            request_timeout_ms,
        })
    }
}
#[derive(Clone, Debug)]
/// Legacy proxy configurations
pub struct Legacy {
    pub uri: Uri,
}

impl Legacy {
    pub fn from_env() -> Result<Self> {
        let uri = env::var("LEGACY_SUPERVISOR_ADDRESS")
            .with_context(|| "LEGACY_SUPERVISOR_ADDRESS undefined")?
            .parse()?;
        Ok(Self { uri })
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub uuid: String,
    pub local: Local,
    pub balena: Remote,
    pub legacy: Legacy,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let uuid = env::var("BALENA_UUID").with_context(|| "Device UUID should be provided")?;
        let local = Local::from_env()?;
        let balena = Remote::from_env()?;
        let legacy = Legacy::from_env()?;

        Ok(Self {
            uuid,
            local,
            balena,
            legacy,
        })
    }
}
