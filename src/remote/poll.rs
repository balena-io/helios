use mahler::workflow::Interrupt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use std::{collections::HashMap, future::Future, pin::Pin};
use tokio::sync::watch::{Receiver, Sender};
use tokio::time::Instant;
use tracing::{error, info, instrument, trace, warn};

use crate::state::models::TargetDevice;
use crate::state::{SeekRequest, UpdateOpts};
use crate::types::Uuid;
use crate::util::http::Uri;

use super::config::{RemoteConfig, RequestConfig};
use super::request::{Get, RequestConfig as GetConfig};

async fn get_poll_client(uuid: &Uuid, remote: &RemoteConfig) -> (Get, Option<Value>) {
    let uri = remote.api_endpoint.clone();
    let endpoint = Uri::from_parts(uri, format!("/device/v3/{uuid}/state").as_str(), None)
        .expect("remote API endpoint must be a valid URI")
        .to_string();

    let client_config = GetConfig {
        timeout: remote.request.timeout,
        min_interval: remote.request.poll_min_interval,
        max_backoff: remote.request.poll_interval,
        api_token: Some(remote.api_key.to_string()),
    };

    let mut client = Get::new(endpoint, client_config);
    let cached = client.restore_cache().await.cloned();

    (client, cached)
}

fn next_poll(config: &RequestConfig) -> Duration {
    let max_jitter = &config.poll_max_jitter;
    let jitter_ms = rand::random_range(0..=max_jitter.as_millis() as u64);
    let jitter = Duration::from_millis(jitter_ms);
    config.poll_interval + jitter
}

// Return type from poll_remote with metadata
type PollResult = (Option<Value>, UpdateOpts, Instant);

/// Poll the remote target returning the metadata back
/// to the caller after the request succeeds
async fn poll_remote(
    config: &RequestConfig,
    poll_client: &mut Get,
    req: PollRequest,
    interrupt: Interrupt,
) -> PollResult {
    // poll if we have a client
    let value = match poll_client.get(Some(interrupt)).await {
        Ok(res) if req.reemit || res.modified => res.value,
        Ok(_) => None,
        Err(e) => {
            error!("poll failed: {e}");
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

#[derive(Debug, Clone)]
enum PollAction {
    Terminate,
    Poll(PollRequest),
    Complete {
        target: Option<Value>,
        opts: UpdateOpts,
        next_poll: Instant,
    },
}

#[instrument(name = "poll", skip_all)]
pub async fn start_poll(
    uuid: Uuid,
    remote: RemoteConfig,
    mut poll_rx: Receiver<PollRequest>,
    seek_tx: Sender<SeekRequest>,
) {
    info!("starting");

    let (mut poll_client, initial_target) = get_poll_client(&uuid, &remote).await;

    // Poll trigger variables
    let mut next_poll_time = Instant::now() + next_poll(&remote.request);
    let mut poll_future: Pin<Box<dyn Future<Output = PollResult>>>;
    let mut interrupt = Interrupt::new();

    // Use the client cached target if any as the result of the first poll
    if let Some(target_state) = initial_target {
        poll_future = Box::pin(async move {
            (
                Some(target_state),
                UpdateOpts::default(),
                Instant::now() + remote.request.poll_min_interval,
            )
        });
    } else {
        poll_future = Box::pin(poll_remote(
            &remote.request,
            &mut poll_client,
            PollRequest {
                opts: UpdateOpts::default(),
                reemit: false,
            },
            interrupt.clone(),
        ));
    }

    loop {
        let action: PollAction = tokio::select! {
            // Wake up on poll if not applying changes
            _ = tokio::time::sleep_until(next_poll_time) => {
                PollAction::Poll(PollRequest {
                    opts: UpdateOpts::default(),
                        reemit: false,
                })
            }

            // Handle poll completion
            response = &mut poll_future => {
                let (target, opts, next_poll) = response;
                PollAction::Complete {
                    target,
                    opts,
                    next_poll,
                }
            }

            // Handle a poll request
            update_requested = poll_rx.changed() => {
                if update_requested.is_err() {
                    // Not really an error, it just means the API closed
                    trace!("request channel closed");
                    PollAction::Terminate
                } else {
                    let update_req = poll_rx.borrow_and_update().clone();
                    PollAction::Poll(update_req)
                }
            }
        };

        match action {
            PollAction::Terminate => {
                break;
            }

            PollAction::Poll(update_req) => {
                // Interrupt the poll in progress
                interrupt.trigger();

                // Wait for the poll to return
                poll_future.await;

                // Trigger a new poll cancelling any pending apply if a new poll is received
                interrupt = Interrupt::new();
                poll_future = Box::pin(poll_remote(
                    &remote.request,
                    &mut poll_client,
                    update_req,
                    interrupt.clone(),
                ));

                // Reset the poll interval to avoid busy waiting
                next_poll_time = Instant::now() + next_poll(&remote.request);
            }

            PollAction::Complete {
                target,
                opts,
                next_poll,
            } => {
                // Reset the polling state
                interrupt = Interrupt::new();
                poll_future = {
                    let interrupt = interrupt.clone();
                    // create a future that just waits for an interrupt
                    Box::pin(async move {
                        interrupt.wait().await;
                        (None, UpdateOpts::default(), next_poll)
                    })
                };
                next_poll_time = next_poll;

                // If there is a new target
                if let Some(target_state) = target {
                    // put the poll back on the channel
                    match serde_json::from_value::<TargetState>(target_state.clone()) {
                        Ok(TargetState(mut map)) => {
                            if let Some(target) = map.remove(&uuid) {
                                let _ = seek_tx.send(SeekRequest {
                                    target,
                                    raw_target: Some(target_state),
                                    opts,
                                });
                            } else {
                                error!("no target for uuid {uuid} found on target state");
                            }
                        }
                        Err(e) => {
                            // FIXME: we'll need to reject the target if it cannot be deserialized
                            warn!("failed to deserialize target state: {e}");
                        }
                    };
                }
            }
        }
    }
}
