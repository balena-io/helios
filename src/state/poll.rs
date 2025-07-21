use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    future::{self, Future},
    pin::Pin,
};
use tokio::sync::watch::{Receiver, Sender};
use tokio::time::Instant;
use tracing::{error, info, instrument, trace, warn};

use crate::config::Config;
use crate::remote::{get_poll_client, next_poll, poll_remote, PollResult};

use super::models::{TargetDevice, Uuid};
use super::seek::{SeekRequest, UpdateOpts};

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
    let mut poll_future: Pin<Box<dyn Future<Output = PollResult<UpdateOpts>>>> =
        // Use the client cached target if any as the result of the first poll
        if let Some(target_state) = initial_target {
            Box::pin(async move {
                (Some(target_state), UpdateOpts::default(), Instant::now() + config.remote.min_interval)
            })
        } else {
            Box::pin(poll_remote(
                config,
                &mut poll_client,
                false,
                UpdateOpts::default(),
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
                    false,
                    UpdateOpts::default(),
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
                if let (Some(target_state), opts) = (value, opts) {
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
                    update_req.reemit,
                    update_req.opts,
                ));
                next_poll_time = Instant::now() + next_poll(config);
            }
        }
    }
}
