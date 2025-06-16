use anyhow::{Context, Result};
use hyper::Uri;
use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub local_api_port: u16,
    pub balena_api_uri: Uri,
    pub legacy_supervisor_uri: Uri,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let local_api_port = env::var("BALENA_SUPERVISOR_PORT")
            .unwrap_or_else(|_| "48484".to_string())
            .parse::<u16>()?;
        let balena_api_uri = env::var("BALENA_API_ENDPOINT")
            .unwrap_or_else(|_| "https://api.balena-cloud.com".to_string())
            .parse()?;
        let legacy_supervisor_uri = env::var("LEGACY_SUPERVISOR_ADDRESS")
            .with_context(|| "LEGACY_SUPERVISOR_ADDRESS undefined")?
            .parse()?;

        Ok(Self {
            local_api_port,
            balena_api_uri,
            legacy_supervisor_uri,
        })
    }
}
