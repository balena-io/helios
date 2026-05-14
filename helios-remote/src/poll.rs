use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use std::{collections::HashMap, future::Future, pin::Pin};
use tokio::sync::watch::{Receiver, Sender};
use tokio::time::Instant;
use tracing::{error, info, instrument, trace, warn};

use crate::state::{SeekRequest, TargetState, UpdateOpts};
use crate::util::http::Uri;
use crate::util::interrupt::Interrupt;
use crate::util::request::{self, Get};
use crate::util::types::Uuid;

use super::config::{RemoteConfig, RequestConfig};
use super::model::Device as RemoteDeviceTarget;

async fn get_poll_client(uuid: &Uuid, remote: &RemoteConfig) -> (Get, Option<Value>) {
    let uri = remote.api_endpoint.clone();
    let endpoint = Uri::from_parts(uri, format!("/device/v3/{uuid}/state").as_str(), None)
        .expect("remote API endpoint must be a valid URI")
        .to_string();

    let client_config = request::GetConfig {
        request: request::RequestConfig {
            timeout: remote.request.timeout,
            min_interval: remote.request.poll_min_interval,
            max_backoff: remote.request.poll_interval,
            auth_token: Some(remote.api_key.to_string()),
        },
        persist_cache: true,
    };

    let mut client = Get::new(endpoint, client_config);
    let cached = client.restore_cache().await.cloned();

    (client, cached)
}

fn next_poll(config: &RequestConfig) -> Duration {
    let max_jitter = &config.poll_max_jitter;
    // FIXME: move rand to helios-util
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

#[derive(Deserialize, Clone, Debug)]
struct RemoteTargetState(HashMap<Uuid, RemoteDeviceTarget>);

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
    Poll(PollRequest),
    Complete {
        target: Option<Value>,
        opts: UpdateOpts,
        next_poll: Instant,
    },
}

// Timeout to re-emit the target state. Configured to 24 hours
const REEMIT_TIMEOUT: Duration = Duration::from_secs(24 * 3600);

#[instrument(name = "poll", skip_all)]
pub async fn start_poll(
    uuid: Uuid,
    remote: RemoteConfig,
    poll_rx: Receiver<PollRequest>,
    seek_tx: Sender<SeekRequest>,
) {
    start_poll_with_reemit(uuid, remote, poll_rx, seek_tx, REEMIT_TIMEOUT).await
}

async fn start_poll_with_reemit(
    uuid: Uuid,
    remote: RemoteConfig,
    mut poll_rx: Receiver<PollRequest>,
    seek_tx: Sender<SeekRequest>,
    reemit_timeout: Duration,
) {
    info!("starting");

    let (mut poll_client, initial_target) = get_poll_client(&uuid, &remote).await;

    // Poll trigger variables
    let mut next_poll_time = Instant::now() + next_poll(&remote.request);
    let mut poll_future: Pin<Box<dyn Future<Output = PollResult>>>;
    let mut interrupt = Interrupt::new();

    let mut daily_reemit_poll = Instant::now() + reemit_timeout;

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

            // Wake up every 24 hours and re-emit the target state to re-try expired
            // abort state
            _ = tokio::time::sleep_until(daily_reemit_poll) => {
                // re-set the timer to avoid busy-waiting after emit
                daily_reemit_poll = Instant::now() + reemit_timeout;
                PollAction::Poll(PollRequest {
                    opts: UpdateOpts::default(),
                    reemit: true,
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
            Ok(_) = poll_rx.changed() => {
                let update_req = poll_rx.borrow_and_update().clone();
                PollAction::Poll(update_req)
            }

            else => {
                trace!("request channel closed");
                break;
            }
        };

        match action {
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
                    // re-set the daily poll timer
                    daily_reemit_poll = Instant::now() + reemit_timeout;

                    // put the poll back on the channel
                    match serde_json::from_value::<RemoteTargetState>(target_state.clone()) {
                        Ok(RemoteTargetState(mut map)) => {
                            if let Some(remote_target) = map.remove(&uuid) {
                                let _ = seek_tx.send(SeekRequest {
                                    target: TargetState::Remote {
                                        target: Some(remote_target.into()),
                                        raw_target: target_state,
                                    },
                                    opts,
                                });
                            } else {
                                error!("no target for uuid {uuid} found on target state");
                            }
                        }
                        Err(e) => {
                            warn!("failed to deserialize target state: {e}");
                            let _ = seek_tx.send(SeekRequest {
                                target: TargetState::Remote {
                                    target: None,
                                    raw_target: target_state,
                                },
                                opts,
                            });
                        }
                    };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::types::ApiKey;
    use mockito::Server;
    use serde_json::json;
    use std::str::FromStr;
    use tokio::sync::watch;

    fn test_remote_config(server_url: &str) -> RemoteConfig {
        RemoteConfig {
            api_endpoint: Uri::from_str(server_url).unwrap(),
            api_key: ApiKey::from("test-token".to_string()),
            request: RequestConfig {
                timeout: Duration::from_secs(5),
                // Long so the regular poll branch does not fire during the test
                poll_interval: Duration::from_secs(10),
                // Zero so back-to-back polls under a paused clock are not blocked
                // by the rate-limit `sleep_until`.
                poll_min_interval: Duration::from_millis(0),
                poll_max_jitter: Duration::from_millis(0),
            },
        }
    }

    /// Wait for a seek update bounded by a real OS-thread timeout
    /// (immune to the paused clock). Panics on hang.
    async fn expect_seek_request(rx: &mut watch::Receiver<SeekRequest>, label: &str) {
        let kill = tokio::task::spawn_blocking(|| {
            std::thread::sleep(Duration::from_millis(1000));
        });
        tokio::select! {
            res = rx.changed() => {
                res.expect("seek channel closed");
                let _ = rx.borrow_and_update();
            }
            _ = kill => panic!("timed out waiting for seek update ({label})"),
        }
    }

    /// Regression test: when the daily reemit timer fires, it must be re-armed.
    /// Otherwise its `sleep_until` deadline stays in the past and the select
    /// arm fires on every loop iteration, cancelling each new poll before it
    /// can complete — a busy cancellation loop where the seek channel never
    /// receives another target after the initial one.
    ///
    /// Runs under `start_paused = true` so the reemit interval is driven by
    /// explicit `tokio::time::advance` calls rather than wall-clock waits.
    /// A real-thread (`spawn_blocking`) kill-switch wraps each wait so the
    /// test fails fast — rather than hanging — when the loop is busy-cancelling.
    #[tokio::test(start_paused = true)]
    async fn daily_reemit_does_not_busy_cancel() {
        let uuid = Uuid::from("testdevice");

        let body = json!({
            uuid.to_string(): {
                "name": "test-device",
                "apps": {}
            }
        })
        .to_string();

        let mut server = Server::new_async().await;
        let path = format!("/device/v3/{uuid}/state");
        let _mock = server
            .mock("GET", path.as_str())
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .expect_at_least(1)
            .create_async()
            .await;

        let remote = test_remote_config(&server.url());
        let (_poll_tx, poll_rx) = watch::channel(PollRequest::default());
        let (seek_tx, mut seek_rx) = watch::channel(SeekRequest::default());

        let reemit_timeout = Duration::from_millis(100);

        // The observer drives virtual time between expected updates. It runs
        // alongside the poll loop in a select! so both share the test task;
        // each `.await` here lets the loop's branch be polled and progress.
        let observer = async {
            // 1. Initial poll — HTTP completes in real time, no advance needed.
            expect_seek_request(&mut seek_rx, "initial").await;

            // 2-3. Each reemit requires advancing virtual time past the timer.
            //      Under the bug, the reemit deadline is never re-armed and
            //      the loop busy-cancels every poll without ever triggering seek,
            //      the kill-switch trips here.
            for n in 2..=3 {
                tokio::time::advance(reemit_timeout + Duration::from_millis(10)).await;
                expect_seek_request(&mut seek_rx, &format!("reemit #{n}")).await;
            }
        };

        tokio::select! {
            _ = start_poll_with_reemit(uuid.clone(), remote, poll_rx, seek_tx, reemit_timeout) => {
                unreachable!("start_poll_with_reemit should not return on its own");
            }
            _ = observer => {}
        }
    }

    /// An API-triggered poll must carry the caller's `UpdateOpts` through to the
    /// resulting `SeekRequest`. The local API relies on this contract for force/
    /// cancel flags; a refactor that drops `update_req.opts` would silently break
    /// it without any other test catching it.
    #[tokio::test]
    async fn api_request_propagates_opts_to_seek() {
        let uuid = Uuid::from("testdevice");

        let body = json!({
            uuid.to_string(): {
                "name": "test-device",
                "apps": {}
            }
        })
        .to_string();

        let mut server = Server::new_async().await;
        let path = format!("/device/v3/{uuid}/state");
        let _mock = server
            .mock("GET", path.as_str())
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .expect_at_least(2)
            .create_async()
            .await;

        let remote = test_remote_config(&server.url());
        let (poll_tx, poll_rx) = watch::channel(PollRequest::default());
        let (seek_tx, mut seek_rx) = watch::channel(SeekRequest::default());

        let observer = async {
            // Initial poll completes from the network.
            expect_seek_request(&mut seek_rx, "initial").await;

            // Drive an API-triggered poll with non-default opts.
            poll_tx
                .send(PollRequest {
                    opts: UpdateOpts {
                        force: true,
                        cancel: true,
                    },
                    reemit: false,
                })
                .expect("poll channel closed");

            // The new poll must produce a seek update carrying the supplied opts.
            expect_seek_request(&mut seek_rx, "api-triggered").await;

            let seek = seek_rx.borrow();
            assert!(seek.opts.force, "force flag must propagate to SeekRequest");
            assert!(
                seek.opts.cancel,
                "cancel flag must propagate to SeekRequest"
            );
        };

        tokio::select! {
            _ = start_poll_with_reemit(
                uuid.clone(),
                remote,
                poll_rx,
                seek_tx,
                Duration::from_secs(3600),
            ) => {
                unreachable!("start_poll_with_reemit should not return on its own");
            }
            _ = observer => {}
        }
    }

    /// A `PollRequest` with `reemit: true` must produce a seek update even when
    /// the server returns 304 Not Modified. This is the contract that lets the
    /// API replay an aborted apply — without it, subsequent polls all see 304
    /// and the target can never be re-delivered to the seek loop.
    #[tokio::test]
    async fn reemit_true_emits_target_on_304() {
        let uuid = Uuid::from("testdevice");

        let body = json!({
            uuid.to_string(): {
                "name": "test-device",
                "apps": {}
            }
        })
        .to_string();

        let mut server = Server::new_async().await;
        let path = format!("/device/v3/{uuid}/state");

        // First request: 200 with ETag — populates the in-memory cache and etag.
        let _mock_200 = server
            .mock("GET", path.as_str())
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_header("etag", "\"v1\"")
            .with_body(body)
            .expect(1)
            .create_async()
            .await;

        // Subsequent requests send If-None-Match and receive 304.
        let _mock_304 = server
            .mock("GET", path.as_str())
            .match_header("if-none-match", "\"v1\"")
            .with_status(304)
            .expect_at_least(1)
            .create_async()
            .await;

        let remote = test_remote_config(&server.url());
        let (poll_tx, poll_rx) = watch::channel(PollRequest::default());
        let (seek_tx, mut seek_rx) = watch::channel(SeekRequest::default());

        let observer = async {
            // Initial 200 → seek update with the target.
            expect_seek_request(&mut seek_rx, "initial").await;

            // API request with reemit=true; server will respond 304.
            poll_tx
                .send(PollRequest {
                    opts: UpdateOpts::default(),
                    reemit: true,
                })
                .expect("poll channel closed");

            // Without reemit, the 304 would suppress the seek update; with reemit
            // the cached target must be re-delivered.
            expect_seek_request(&mut seek_rx, "reemit-on-304").await;

            let seek = seek_rx.borrow();
            match &seek.target {
                TargetState::Remote { target, .. } => {
                    assert!(
                        target.is_some(),
                        "reemit on 304 must re-deliver the cached target"
                    );
                }
                _ => panic!("expected Remote target state"),
            }
        };

        tokio::select! {
            _ = start_poll_with_reemit(
                uuid.clone(),
                remote,
                poll_rx,
                seek_tx,
                Duration::from_secs(3600),
            ) => {
                unreachable!("start_poll_with_reemit should not return on its own");
            }
            _ = observer => {}
        }
    }
}
