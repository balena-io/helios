use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use std::{
    collections::HashMap,
    future::{self, Future},
    pin::Pin,
};
use tokio::sync::watch::{Receiver, Sender};
use tokio::time::Instant;
use tracing::{error, info, instrument, trace, warn};

use crate::config::Config;
use crate::state::models::{TargetDevice, Uuid};
use crate::state::{SeekRequest, UpdateOpts};
use crate::util::uri::make_uri;

use super::request::{Get, RequestConfig};

async fn get_poll_client(config: &Config) -> (Option<Get>, Option<Value>) {
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

fn next_poll(config: &Config) -> Duration {
    let max_jitter = &config.remote.max_poll_jitter;
    let jitter_ms = rand::random_range(0..=max_jitter.as_millis() as u64);
    let jitter = Duration::from_millis(jitter_ms);
    config.remote.poll_interval + jitter
}

// Return type from poll_remote with metadata
type PollResult = (Option<Value>, UpdateOpts, Instant);

/// Poll the remote target returning the metadata back
/// to the caller after the request succeeds
async fn poll_remote(config: &Config, poll_client: &mut Get, req: PollRequest) -> PollResult {
    // poll if we have a client
    let value = match poll_client.get().await {
        Ok(res) if req.reemit || res.modified => res.value,
        Ok(_) => None,
        Err(e) => {
            warn!("poll failed: {e}");
            None
        }
    };

    (value, req.opts, Instant::now() + next_poll(config))
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct TargetState(HashMap<Uuid, TargetDevice>);

/// An update request coming from the API.
///
/// Defaults to `reemit: true` and `opts: UpdateOpts::default()`. This means a new poll request
/// coming will always re-apply the target even if the API request returns a 304. This is to
/// allow an aborted apply to be re-tried.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PollRequest {
    pub opts: UpdateOpts,
    pub reemit: bool,
}

impl Default for PollRequest {
    fn default() -> Self {
        Self {
            opts: UpdateOpts::default(),
            reemit: true,
        }
    }
}

#[instrument(name = "poll", skip_all)]
pub async fn start_poll(
    config: &Config,
    mut poll_rx: Receiver<PollRequest>,
    seek_tx: Sender<SeekRequest>,
) {
    let (mut poll_client, initial_target) = match get_poll_client(config).await {
        (Some(client), cached) => (client, cached),
        (None, _) => {
            warn!("running in unmanaged mode");
            // just disable this branch
            return future::pending::<()>().await;
        }
    };
    info!("ready");

    // Poll trigger variables
    let mut next_poll_time = Instant::now() + next_poll(config);
    let mut poll_future: Pin<Box<dyn Future<Output = PollResult>>> =
        // Use the client cached target if any as the result of the first poll
        if let Some(target_state) = initial_target {
            Box::pin(async move {
                (Some(target_state), UpdateOpts::default(), Instant::now() + config.remote.min_interval)
            })
        } else {
            Box::pin(poll_remote(
                config,
                &mut poll_client,
                PollRequest {
                    opts: UpdateOpts::default(),
                    reemit: false,
                },
            ))
        };
    loop {
        tokio::select! {
            // Wake up on poll if not applying changes
            _ = tokio::time::sleep_until(next_poll_time) => {
                drop(poll_future);
                // Trigger a new poll cancelling any pending apply if a new state is received
                poll_future = Box::pin(poll_remote(
                    config,
                    &mut poll_client,
                    PollRequest {
                        opts: UpdateOpts::default(),
                        reemit: false,
                    },
                ));
                // Reset the poll interval to avoid busy waiting
                next_poll_time = Instant::now() + next_poll(config);
            }

            // Handle poll completion
            response = &mut poll_future => {
                // Reset the polling state
                poll_future = Box::pin(future::pending());
                let (value, opts, next_poll) = response;
                next_poll_time = next_poll;

                // If there is a new target
                if let Some(target_state) = value {
                    // put the poll back on the channel
                    match serde_json::from_value::<TargetState>(target_state.clone()) {
                        Ok(TargetState(mut map)) => {
                            if let Some(target) = map.remove(&config.uuid) {
                                let _ = seek_tx.send(SeekRequest {
                                    target,
                                    raw_target: Some(target_state),
                                    opts,
                                });
                            } else {
                                error!("no target for uuid {} found on target state", config.uuid);
                            }
                        },
                        Err(e)  => {
                            // FIXME: we'll need to reject the target if it cannot be deserialized
                            warn!("failed to deserialize target state: {e}");
                        }
                    };
                }
            }

            // Handle a poll request
            update_requested = poll_rx.changed() => {
                if update_requested.is_err() {
                    // Not really an error, it just means the API closed
                    trace!("request channel closed");
                    break;
                }

                let update_req = poll_rx.borrow_and_update().clone();

                // Trigger a new poll
                drop(poll_future);
                poll_future = Box::pin(poll_remote(
                    config,
                    &mut poll_client,
                    update_req,
                ));
                next_poll_time = Instant::now() + next_poll(config);
            }
        }
    }
}
