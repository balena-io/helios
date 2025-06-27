//! API uplink component. It regularly polls target state from the remote backend and passes these
//! directives to the control core

use super::request::{Get, RequestConfig};
use crate::config::Config;
use anyhow::Result;
use axum::http::uri::PathAndQuery;
use hyper::Uri;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tracing::{instrument, warn};

/// Request containing target state and control flags sent to subscribers
#[derive(Debug, Clone)]
pub struct Directive {
    /// The target state value from the API
    pub target: Value,
    /// Whether the target state should be applied ignoring locks
    pub force: bool,
    /// Whether any in-progress target state processing should be cancelled
    pub cancel: bool,
}

/// Options for querying target state
#[derive(Debug, Clone, Default)]
pub struct FetchOpts {
    /// Whether to emit a new Directive even if the state hasn't changed
    pub reemit: bool,
    /// Value for the force flag in the resulting Directive
    pub force: bool,
    /// Value for the cancel flag in the resulting Directive
    pub cancel: bool,
}

/// Service that polls target state from the API and notifies a single subscriber of changes
pub struct UplinkService {
    command_tx: mpsc::Sender<FetchOpts>,
    shutdown_tx: broadcast::Sender<()>,
}

impl UplinkService {
    /// Creates a new uplink service
    ///
    /// The service automatically starts polling the target state endpoint and will
    /// notify the subscriber when the state changes. The polling interval includes
    /// random jitter to reduce load spikes.
    ///
    /// # Arguments
    /// * `config` - Device configuration containing UUID and remote endpoint settings
    /// * `subscriber` - Channel to send target state updates to
    ///
    /// # Example
    /// ```rust,ignore
    /// use crate::config::Config;
    /// use tokio::sync::mpsc;
    ///
    /// let config = Config::from_env()?;
    /// let (tx, mut rx) = mpsc::unbounded_channel();
    /// let service = UplinkService::new(config, tx).await?;
    ///
    /// while let Some(request) = rx.recv().await {
    ///     println!("New target state: {:?}", request.target);
    /// }
    /// ```
    pub async fn new(config: Config, subscriber: mpsc::UnboundedSender<Directive>) -> Result<Self> {
        let (command_tx, command_rx) = mpsc::channel(1); // Lossy channel with capacity 1
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);

        // Build the target state endpoint by replacing any existing path
        let endpoint_uri = config.remote.api_endpoint.clone();
        let mut endpoint_parts = endpoint_uri.into_parts();
        endpoint_parts.path_and_query = Some(PathAndQuery::from_maybe_shared(format!(
            "/device/v3/{}/state",
            config.uuid
        ))?);
        let endpoint = Uri::from_parts(endpoint_parts)?.to_string();

        let request_config = RequestConfig::from_config(&config.remote)
            .with_api_token(config.remote.api_key.unwrap_or_default());

        let poll_interval = Duration::from_millis(config.remote.poll_interval_ms);
        let max_jitter = Duration::from_millis(config.remote.max_jitter_delay_ms);

        tokio::spawn(Self::background_task(
            endpoint,
            request_config,
            poll_interval,
            max_jitter,
            command_rx,
            shutdown_rx,
            subscriber,
        ));

        Ok(Self {
            command_tx,
            shutdown_tx,
        })
    }

    /// Triggers a new fetch of the target state and re-set the poll timer
    ///
    /// If there is a fetch in progress, a new fetch will be performed immediately after that query
    /// finishes (respecting the min poll interval). This method does not block the caller. Only the
    /// most recent update request is kept if multiple are sent rapidly (lossy behavior).
    ///
    /// # Arguments
    /// * `opts` - Fetch options including reemit, force, and cancel flags
    ///
    /// # Example
    /// ```rust,ignore
    /// // Trigger immediate update with force flag
    /// service.trigger_fetch(FetchOpts { force: true, ..Default::default() });
    ///
    /// // Re-emit current state with cancel flag
    /// service.trigger_fetch(FetchOpts { reemit: true, cancel: true, ..Default::default() });
    /// ```
    pub fn trigger_fetch(&self, opts: FetchOpts) {
        let _ = self.command_tx.try_send(opts);
    }

    async fn background_task(
        endpoint: String,
        request_config: RequestConfig,
        poll_interval: Duration,
        max_jitter: Duration,
        mut command_rx: mpsc::Receiver<FetchOpts>,
        mut shutdown_rx: broadcast::Receiver<()>,
        subscriber: mpsc::UnboundedSender<Directive>,
    ) {
        let mut client = Get::new(endpoint, request_config);

        // Perform initial poll before starting the timer
        // XXX: on the legacy supervisor the first poll depends on the instant updates
        // configuration. Should we replicate that here? An alternative would be to cache the
        // target state/etag outside the process memory, so a supervisor restart only pull the target if it has
        // changed
        Self::fetch_target_state(&mut client, &subscriber, None).await;

        // Use sleep instead of interval to control timing precisely
        let mut next_poll_time = tokio::time::Instant::now() + poll_interval;

        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    break;
                }

                cmd = command_rx.recv() => {
                    match cmd {
                        Some(FetchOpts { reemit, force, cancel }) => {
                            // Process last pending command even if a poll was just made
                            Self::fetch_target_state(&mut client, &subscriber, Some((reemit, force, cancel))).await;
                            // Reset poll timer after manual command
                            next_poll_time = tokio::time::Instant::now() + poll_interval;
                        }
                        None => break, // Channel closed
                    }
                }

                _ = tokio::time::sleep_until(next_poll_time) => {
                    // Add jitter to each poll
                    let jitter = Self::calculate_jitter(Duration::ZERO, max_jitter);
                    tokio::time::sleep(jitter).await;

                    Self::fetch_target_state(&mut client, &subscriber, None).await;

                    // Reset timer only after successful poll completion
                    next_poll_time = tokio::time::Instant::now() + poll_interval;
                }
            }
        }
    }

    #[instrument(skip_all)]
    async fn fetch_target_state(
        client: &mut Get,
        subscriber: &mpsc::UnboundedSender<Directive>,
        update_flags: Option<(bool, bool, bool)>, // (reemit, force, cancel)
    ) {
        match client.get().await {
            Ok(response) => {
                if let Some((reemit, force, cancel)) = update_flags {
                    // For update requests, notify if reemit is true or state changed
                    if reemit || response.modified {
                        if let Some(target) = response.value {
                            let _ = subscriber.send(Directive {
                                target,
                                force,
                                cancel,
                            });
                        }
                    }
                } else {
                    // For regular polls, only notify if state changed
                    if response.modified {
                        if let Some(target) = response.value {
                            let _ = subscriber.send(Directive {
                                target,
                                force: false,
                                cancel: false,
                            });
                        }
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "failed to fetch target state");
            }
        }
    }

    fn calculate_jitter(base_interval: Duration, max_jitter: Duration) -> Duration {
        let jitter_ms = rand::random_range(0..=max_jitter.as_millis() as u64);
        base_interval + Duration::from_millis(jitter_ms)
    }
}

impl Drop for UplinkService {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Fallback, Local, Remote};
    use mockito::Server;
    use serde_json::json;
    use std::time::Duration;
    use tokio::time::timeout;

    fn test_config(endpoint: String, api_key: Option<String>) -> Config {
        Config {
            uuid: "test-device-uuid".to_string(),
            local: Local { port: 48484 },
            remote: Remote {
                api_endpoint: endpoint.parse().unwrap(),
                api_key,
                poll_interval_ms: 100, // Short for tests
                request_timeout_ms: 5000,
                min_interval_ms: 10,
                max_jitter_delay_ms: 10, // Minimal jitter for tests
            },
            fallback: Fallback {
                address: "http://fallback.test".parse().unwrap(),
            },
        }
    }

    #[tokio::test]
    async fn test_target_state_service_creation() {
        let server = Server::new_async().await;
        let config = test_config(server.url(), Some("test-token".to_string()));
        let (tx, _rx) = mpsc::unbounded_channel();

        let service = UplinkService::new(config, tx).await;
        assert!(service.is_ok());
    }

    #[tokio::test]
    async fn test_subscription_and_polling() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("GET", "/device/v3/test-device-uuid/state")
            .match_header("authorization", "Bearer test-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"apps": {"app1": {"status": "running"}}}"#)
            .create_async()
            .await;

        let config = test_config(server.url(), Some("test-token".to_string()));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _service = UplinkService::new(config, tx).await.unwrap();

        // Wait for the first poll
        let request = timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("Should receive target state")
            .expect("Should receive request");

        assert_eq!(
            request.target,
            json!({"apps": {"app1": {"status": "running"}}})
        );
        assert!(!request.force);
        assert!(!request.cancel);

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_manual_update_with_flags() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("GET", "/device/v3/test-device-uuid/state")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"apps": {"app2": {"status": "idle"}}}"#)
            .expect_at_least(1)
            .create_async()
            .await;

        let config = test_config(server.url(), Some("test-token".to_string()));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let service = UplinkService::new(config, tx).await.unwrap();

        // First, consume the initial poll result
        let initial_request = timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("Should receive initial target state")
            .expect("Should receive request");
        assert_eq!(
            initial_request.target,
            json!({"apps": {"app2": {"status": "idle"}}})
        );
        assert!(!initial_request.force);
        assert!(!initial_request.cancel);

        // Trigger update with force and cancel flags
        service.trigger_fetch(FetchOpts {
            force: true,
            cancel: true,
            ..Default::default()
        });

        // Should receive notification with the specified flags
        let request = timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("Should receive target state")
            .expect("Should receive request");

        assert_eq!(
            request.target,
            json!({"apps": {"app2": {"status": "idle"}}})
        );
        assert!(request.force);
        assert!(request.cancel);

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_reemit_functionality() {
        let mut server = Server::new_async().await;

        // First request
        let mock1 = server
            .mock("GET", "/device/v3/test-device-uuid/state")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_header("etag", "\"version1\"")
            .with_body(r#"{"apps": {"app3": {"status": "deployed"}}}"#)
            .create_async()
            .await;

        // Second request returns 304 (not modified)
        let mock2 = server
            .mock("GET", "/device/v3/test-device-uuid/state")
            .match_header("if-none-match", "\"version1\"")
            .with_status(304)
            .create_async()
            .await;

        let config = test_config(server.url(), Some("test-token".to_string()));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let service = UplinkService::new(config, tx).await.unwrap();

        // Wait for initial state
        let initial_request = timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("Should receive initial target state")
            .expect("Should not be lagged");

        assert_eq!(
            initial_request.target,
            json!({"apps": {"app3": {"status": "deployed"}}})
        );

        // Trigger reemit - should get the same state again even though not modified
        service.trigger_fetch(FetchOpts {
            reemit: true,
            ..Default::default()
        });

        let reemit_request = timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("Should receive re-emitted target state")
            .expect("Should not be lagged");

        assert_eq!(
            reemit_request.target,
            json!({"apps": {"app3": {"status": "deployed"}}})
        );
        assert!(!reemit_request.force);
        assert!(!reemit_request.cancel);

        mock1.assert_async().await;
        mock2.assert_async().await;
    }

    #[tokio::test]
    async fn test_single_subscriber() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("GET", "/device/v3/test-device-uuid/state")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"apps": {"multi": {"status": "running"}}}"#)
            .create_async()
            .await;

        let config = test_config(server.url(), Some("test-token".to_string()));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _service = UplinkService::new(config, tx).await.unwrap();

        // Should receive the message
        let request = timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("Should receive target state")
            .expect("Should receive request");

        assert_eq!(
            request.target,
            json!({"apps": {"multi": {"status": "running"}}})
        );

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_concurrent_updates() {
        let mut server = Server::new_async().await;

        // Mock will be called multiple times but we only care about the final state
        let mock = server
            .mock("GET", "/device/v3/test-device-uuid/state")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"apps": {"concurrent": {"status": "final"}}}"#)
            .expect_at_least(1)
            .create_async()
            .await;

        let config = test_config(server.url(), Some("test-token".to_string()));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let service = UplinkService::new(config, tx).await.unwrap();

        // Send multiple concurrent updates rapidly - due to lossy channel, some may be dropped
        service.trigger_fetch(FetchOpts::default());
        service.trigger_fetch(FetchOpts {
            force: true,
            ..Default::default()
        });
        service.trigger_fetch(FetchOpts {
            cancel: true,
            ..Default::default()
        });

        // Should receive at least one notification, but flags may vary due to timing
        let request = timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("Should receive target state")
            .expect("Should receive request");

        assert_eq!(
            request.target,
            json!({"apps": {"concurrent": {"status": "final"}}})
        );
        // Due to lossy channel behavior, we may get any of the sent updates
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_service_shutdown() {
        let server = Server::new_async().await;
        let config = test_config(server.url(), Some("test-token".to_string()));

        let (tx, mut rx) = mpsc::unbounded_channel();
        let service = UplinkService::new(config, tx).await.unwrap();

        // Drop the service
        drop(service);

        // Receiver should eventually be closed
        tokio::time::sleep(Duration::from_millis(50)).await;

        // This should not hang indefinitely
        let result = timeout(Duration::from_millis(100), rx.recv()).await;
        // Either we get a timeout or receiver is closed, both are acceptable for shutdown
        assert!(result.is_err() || result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_api_error_handling() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("GET", "/device/v3/test-device-uuid/state")
            .with_status(500)
            .expect_at_least(1)
            .create_async()
            .await;

        let config = test_config(server.url(), Some("test-token".to_string()));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _service = UplinkService::new(config, tx).await.unwrap();

        // Should not receive any notifications due to API errors
        let result = timeout(Duration::from_millis(200), rx.recv()).await;
        assert!(
            result.is_err(),
            "Should not receive notifications on API errors"
        );

        mock.assert_async().await;
    }

    #[test]
    fn test_directive_structure() {
        let state = json!({"test": "value"});

        let request1 = Directive {
            target: state.clone(),
            force: false,
            cancel: false,
        };
        assert_eq!(request1.target, state);
        assert!(!request1.force);
        assert!(!request1.cancel);

        let request2 = Directive {
            target: state.clone(),
            force: true,
            cancel: true,
        };
        assert_eq!(request2.target, state);
        assert!(request2.force);
        assert!(request2.cancel);
    }

    #[tokio::test]
    async fn test_poll_timing_respects_interval() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("GET", "/device/v3/test-device-uuid/state")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"apps": {"timing": {"status": "running"}}}"#)
            .expect_at_least(2)
            .create_async()
            .await;

        // Set a short poll interval (150ms) for testing
        let config = Config {
            uuid: "test-device-uuid".to_string(),
            local: Local { port: 48484 },
            remote: Remote {
                api_endpoint: server.url().parse().unwrap(),
                api_key: Some("test-token".to_string()),
                poll_interval_ms: 150, // 150ms interval
                request_timeout_ms: 5000,
                min_interval_ms: 10,
                max_jitter_delay_ms: 0, // No jitter for precise timing
            },
            fallback: Fallback {
                address: "http://legacy.test".parse().unwrap(),
            },
        };

        let (tx, mut rx) = mpsc::unbounded_channel();
        let _service = UplinkService::new(config, tx).await.unwrap();

        // Record times when we receive notifications
        let start_time = std::time::Instant::now();
        let mut notification_times = Vec::new();

        // Wait for 2 notifications
        for _ in 0..2 {
            if timeout(Duration::from_millis(500), rx.recv()).await.is_ok() {
                notification_times.push(start_time.elapsed());
            }
        }

        // Should have 2 notifications
        assert_eq!(notification_times.len(), 2);

        // Time between notifications should be at least 150ms (the poll interval)
        let time_between = notification_times[1] - notification_times[0];
        assert!(
            time_between >= Duration::from_millis(130), // Allow some tolerance for test timing
            "Time between polls should be at least 150ms, but was {time_between:?}",
        );

        mock.assert_async().await;
    }
}
