use serde_json::Value;
use std::time::Duration;
use tokio::time::Instant;
use tracing::warn;

use crate::config::Config;
use crate::util::uri::make_uri;

use super::request::{Get, RequestConfig};

pub async fn get_poll_client(config: &Config) -> (Option<Get>, Option<Value>) {
    if let Some(uri) = config.remote.api_endpoint.clone() {
        let endpoint = make_uri(
            uri,
            format!("/device/v3/{}/state", config.uuid).as_str(),
            None,
        )
        .expect("remote API endpoint must be a valid URI")
        .to_string();

        let client_config = RequestConfig {
            timeout: config.remote.request_timeout,
            min_interval: config.remote.min_interval,
            max_backoff: config.remote.poll_interval,
            api_token: config.remote.api_key.clone(),
        };

        let mut client = Get::new(endpoint, client_config);
        let cached = client.restore_cache().await.cloned();

        (Some(client), cached)
    } else {
        (None, None)
    }
}

pub fn next_poll(config: &Config) -> Duration {
    let max_jitter = &config.remote.max_poll_jitter;
    let jitter_ms = rand::random_range(0..=max_jitter.as_millis() as u64);
    let jitter = Duration::from_millis(jitter_ms);
    config.remote.poll_interval + jitter
}

// Return type from poll_remote with metadata
pub type PollResult<M> = (Option<Value>, M, Instant);

/// Poll the remote target returning the metadata back
/// to the caller after the request succeeds
pub async fn poll_remote<M>(
    config: &Config,
    poll_client: &mut Get,
    reemit: bool,
    meta: M,
) -> PollResult<M> {
    // poll if we have a client
    let value = match poll_client.get().await {
        Ok(res) if reemit || res.modified => res.value,
        Ok(_) => None,
        Err(e) => {
            warn!("poll failed: {e}");
            None
        }
    };

    (value, meta, Instant::now() + next_poll(config))
}
