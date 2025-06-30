use reqwest::StatusCode;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use thiserror::Error;
use tracing::{field, instrument, Span};

/// Internal errors that can occur during HTTP GET requests (including retry logic).
#[derive(Debug, Error)]
enum TryGetError {
    /// Could not serialize the response into JSON
    #[error("Failed to serialize response: {0}")]
    Serialization(#[from] reqwest::Error),

    /// HTTP request failed with permanent client error that will not be retried.
    #[error("Request failed with status code {0}")]
    Status(StatusCode),

    /// Request failed but should be retried (used internally)
    #[error("Request failed with: {0} ... will retry in {1:#?}")]
    WillRetry(String, Duration),
}

/// Errors that can occur during HTTP GET requests.
#[derive(Debug, Error)]
pub enum GetError {
    /// Could not serialize the response into JSON
    #[error("Failed to serialize response: {0}")]
    Serialization(#[from] reqwest::Error),

    /// Authentication failed due to an invalid or expired token.
    #[error("Unauthorized")]
    Unauthorized,

    #[error("Not found")]
    NotFound,
}

impl From<TryGetError> for GetError {
    fn from(err: TryGetError) -> Self {
        match err {
            TryGetError::Serialization(e) => GetError::Serialization(e),
            TryGetError::Status(status) => match status {
                StatusCode::UNAUTHORIZED => GetError::Unauthorized,
                StatusCode::NOT_FOUND => GetError::NotFound,
                _ => unreachable!(),
            },
            TryGetError::WillRetry(_, _) => unreachable!(),
        }
    }
}

/// HTTP GET response containing the parsed JSON body and modification status.
#[derive(Debug, Clone)]
pub struct GetResponse {
    /// The JSON response body. None if the response was empty or for 304 Not Modified responses.
    pub value: Option<serde_json::Value>,
    /// Whether the response was modified (false for 304 Not Modified responses).
    pub modified: bool,
}

/// Errors that can occur during HTTP PATCH requests.
#[derive(Debug, Error)]
enum TryPatchError {
    /// Request failed with a 5xx or other recoverable errors that will be retried.
    #[error("Request failed with: {0} ... will retry in {1:#?}")]
    WillRetry(String, Duration),

    /// HTTP request failed with permanent client error that will not be retried.
    #[error("Request failed with status code {0}")]
    Status(StatusCode),
}

#[derive(Debug, Clone, Error)]
pub enum PatchError {
    /// Request was completed with an error
    #[error("Request failed with status code {0}")]
    Status(u16),

    /// Request was interrupted because the Patch client was dropped or a newer patch replaced the
    /// request
    #[error("Request was not completed")]
    Incomplete,
}

/// HTTP PATCH response
pub type PatchResponse = ();

/// A Future that resolves when a PATCH request completes or is interrupted.
///
/// The patch processing continues in the background whether this Future is awaited or not.
/// When awaited, it returns Ok(()) for success or a PatchError for failures.
pub struct PatchRequest {
    result_rx: tokio::sync::oneshot::Receiver<Result<PatchResponse, PatchError>>,
}

impl Future for PatchRequest {
    type Output = Result<(), PatchError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.result_rx).poll(cx) {
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(_)) => Poll::Ready(Err(PatchError::Incomplete)),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Configuration for HTTP request behavior including timeouts, rate limiting, and backoff.
#[derive(Clone)]
pub struct RequestConfig {
    /// Maximum time to wait for a single HTTP request to complete.
    pub timeout: Duration,
    /// Minimum time to wait between consecutive requests (rate limiting).
    pub min_interval: Duration,
    /// Maximum time to wait during exponential backoff after errors.
    pub max_backoff: Duration,
    /// Optional API token for authentication (will be sent as "Bearer {token}").
    pub api_token: Option<String>,
}

impl RequestConfig {
    /// Creates a new RequestConfig from remote configuration settings.
    ///
    /// # Arguments
    /// * `remote` - Remote API configuration containing timeout and request interval settings
    ///
    /// # Example
    /// ```rust,ignore
    /// let config = RequestConfig::from_remote_config(&remote_config)
    ///     .with_api_token("your-api-token");
    /// ```
    pub fn from_config(remote: &crate::config::Remote) -> Self {
        Self {
            timeout: remote.request_timeout,
            min_interval: remote.min_interval,
            max_backoff: remote.poll_interval,
            api_token: None,
        }
    }

    /// Sets the API token for authentication.
    ///
    /// # Arguments
    /// * `token` - API token that will be sent as "Bearer {token}"
    ///
    /// # Example
    /// ```rust,ignore
    /// let config = RequestConfig {
    ///     timeout: Duration::from_secs(30),
    ///     min_interval: Duration::from_secs(5),
    ///     max_backoff: Duration::from_secs(300),
    ///     api_token: None,
    /// }.with_api_token("your-api-token");
    /// ```
    pub fn with_api_token(mut self, token: impl Into<String>) -> Self {
        self.api_token = Some(token.into());
        self
    }
}

/// Metrics tracking the success and failure counts for HTTP requests.
#[derive(Debug, Clone, Copy)]
pub struct RequestMetrics {
    /// Number of successful HTTP requests (2xx status codes).
    pub success_count: u64,
    /// Number of failed HTTP requests (4xx, 5xx status codes, network errors).
    pub error_count: u64,
}

impl RequestMetrics {
    /// Returns the total number of requests made (successful + failed).
    ///
    /// # Example
    /// ```rust,ignore
    /// let metrics = client.metrics().await;
    /// println!("Total requests: {}", metrics.total_requests());
    /// ```
    pub fn total_requests(&self) -> u64 {
        self.success_count + self.error_count
    }

    /// Returns the success rate as a percentage (0.0 to 100.0).
    ///
    /// Returns 0.0 if no requests have been made yet.
    ///
    /// # Example
    /// ```rust,ignore
    /// let metrics = client.metrics().await;
    /// println!("Success rate: {:.1}%", metrics.success_rate());
    /// ```
    pub fn success_rate(&self) -> f64 {
        let total = self.total_requests();
        if total == 0 {
            0.0
        } else {
            (self.success_count as f64 / total as f64) * 100.0
        }
    }

    /// Returns the error rate as a percentage (0.0 to 100.0).
    ///
    /// Returns 0.0 if no requests have been made yet.
    ///
    /// # Example
    /// ```rust,ignore
    /// let metrics = client.metrics().await;
    /// println!("Error rate: {:.1}%", metrics.error_rate());
    /// ```
    pub fn error_rate(&self) -> f64 {
        let total = self.total_requests();
        if total == 0 {
            0.0
        } else {
            (self.error_count as f64 / total as f64) * 100.0
        }
    }
}

#[derive(Clone)]
struct RequestState {
    client: reqwest::Client,
    endpoint: String,
    config: RequestConfig,
    cached_state: Option<serde_json::Value>,
    etag: Option<String>,
    next_retry: Option<Instant>,
    current_backoff: Duration,
    success_count: u64,
    error_count: u64,
}

impl RequestState {
    fn new(endpoint: String, config: RequestConfig) -> Self {
        // Use the min configured interval as initial backoff
        let current_backoff = config.min_interval;
        Self {
            // TODO: we need to add a DNS resolver to support MDNS
            client: reqwest::Client::new(),
            endpoint,
            config,
            cached_state: None,
            etag: None,
            next_retry: None,
            current_backoff,
            success_count: 0,
            error_count: 0,
        }
    }

    fn record_success(&mut self) {
        self.success_count += 1;
        self.current_backoff = self.config.min_interval;
        self.next_retry = Some(Instant::now() + self.config.min_interval);
    }

    fn record_failure(&mut self, retry_after: Option<Duration>) {
        self.error_count += 1;
        let backoff_duration = if let Some(duration) = retry_after {
            duration
        } else {
            self.current_backoff = std::cmp::min(self.current_backoff * 2, self.config.max_backoff);
            self.current_backoff
        };

        self.next_retry = Some(Instant::now() + backoff_duration);
    }

    fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
        response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
    }

    async fn wait_for_rate_limit(&self) {
        if let Some(retry_time) = self.next_retry {
            let now = Instant::now();
            if now < retry_time {
                tokio::time::sleep_until(retry_time.into()).await;
            }
        }
    }
}

/// HTTP GET client with caching, rate limiting, and automatic retries.
///
/// Supports ETag-based caching for efficient polling and includes built-in
/// exponential backoff for error handling. Returns cached responses when
/// the server returns 304 Not Modified or when errors occur.
pub struct Get {
    state: RequestState,
}

impl Get {
    /// Creates a new GET client for the specified endpoint.
    ///
    /// # Arguments
    /// * `endpoint` - The full URL to make GET requests to
    /// * `config` - Request configuration including timeouts, rate limiting, and optional authentication
    ///
    /// # Example
    /// ```rust,ignore
    /// use std::time::Duration;
    ///
    /// // With authentication
    /// let config = RequestConfig {
    ///     timeout: Duration::from_secs(30),
    ///     min_interval: Duration::from_secs(5),
    ///     max_backoff: Duration::from_secs(300),
    ///     api_token: None,
    /// }.with_api_token("your-api-token");
    ///
    /// let client = Get::new("https://api.example.com/status", config);
    ///
    /// // Without authentication
    /// let config = RequestConfig {
    ///     timeout: Duration::from_secs(30),
    ///     min_interval: Duration::from_secs(5),
    ///     max_backoff: Duration::from_secs(300),
    ///     api_token: None,
    /// };
    ///
    /// let client = Get::new("https://public-api.example.com/status", config);
    /// ```
    pub fn new(endpoint: impl Into<String>, config: RequestConfig) -> Self {
        Self {
            state: RequestState::new(endpoint.into(), config),
        }
    }

    #[instrument(skip_all, fields(status=field::Empty) err)]
    async fn try_get(&mut self) -> Result<GetResponse, TryGetError> {
        self.state.wait_for_rate_limit().await;

        let mut request = self
            .state
            .client
            .get(&self.state.endpoint)
            .timeout(self.state.config.timeout)
            .header("Accept-Encoding", "br, gzip, deflate");

        if let Some(api_token) = &self.state.config.api_token {
            request = request.header("Authorization", format!("Bearer {api_token}"));
        }

        if let Some(etag) = &self.state.etag {
            request = request.header("If-None-Match", etag);
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(e) => {
                self.state.record_failure(None);
                return Err(TryGetError::WillRetry(
                    e.to_string(),
                    self.state.current_backoff,
                ));
            }
        };
        let status = response.status();

        Span::current().record("status", status.as_u16());
        match status {
            status if status.is_success() => {
                let new_etag = response
                    .headers()
                    .get("etag")
                    .and_then(|v| v.to_str().ok())
                    .map(String::from);

                let json: serde_json::Value = match response.json().await {
                    Ok(json) => json,
                    Err(e) => {
                        return Err(TryGetError::Serialization(e));
                    }
                };

                self.state.cached_state = Some(json.clone());
                self.state.etag = new_etag;
                self.state.record_success();

                Ok(GetResponse {
                    value: Some(json),
                    modified: true,
                })
            }
            StatusCode::NOT_MODIFIED => {
                self.state.record_success();
                Ok(GetResponse {
                    value: self.state.cached_state.clone(),
                    modified: false,
                })
            }
            StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED => {
                self.state.error_count += 1;
                Err(TryGetError::Status(status))
            }
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                let retry_after = RequestState::parse_retry_after(&response);
                self.state.record_failure(retry_after);
                Err(TryGetError::WillRetry(
                    format!("server responded with {status}",),
                    retry_after.unwrap_or(self.state.current_backoff),
                ))
            }
            _ => {
                self.state.record_failure(None);
                Err(TryGetError::WillRetry(
                    format!("server responded with {status}",),
                    self.state.current_backoff,
                ))
            }
        }
    }

    /// Performs an HTTP GET request with automatic caching and error handling.
    ///
    /// Uses ETag-based caching to avoid downloading unchanged data. When the server
    /// returns 304 Not Modified, the cached response is returned instead. On errors
    /// (rate limiting, server errors), returns cached data if available.
    ///
    /// # Returns
    /// * `Ok(Response)` - Successfully retrieved response or cached data
    /// * `Err(GetError)` - Authentication failed or no cached data available for error
    ///
    /// # Example
    /// ```rust,ignore
    /// let response = client.get().await?;
    ///
    /// if response.modified {
    ///     println!("Fresh data: {:?}", response.value);
    /// } else {
    ///     println!("Cached data: {:?}", response.value);
    /// }
    /// ```
    #[instrument(skip_all, fields(tries=field::Empty), err)]
    pub async fn get(&mut self) -> Result<GetResponse, GetError> {
        let mut tries = 1;
        loop {
            match self.try_get().await {
                Ok(response) => {
                    Span::current().record("tries", tries);
                    return Ok(response);
                }
                Err(TryGetError::WillRetry(_, _)) => {
                    tries += 1;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Returns current request metrics including success and error counts.
    ///
    /// # Example
    /// ```rust,ignore
    /// let metrics = client.metrics();
    /// println!("Success rate: {:.1}%", metrics.success_rate());
    /// println!("Total requests: {}", metrics.total_requests());
    /// ```
    pub fn metrics(&self) -> RequestMetrics {
        RequestMetrics {
            success_count: self.state.success_count,
            error_count: self.state.error_count,
        }
    }
}

/// HTTP PATCH client with background flushing, natural batching, and rate limiting.
///
/// Queues PATCH requests and flushes them in the background to reduce server load.
/// Rapid successive updates naturally batch together as newer state overwrites
/// pending state before transmission.
pub struct Patch {
    patch_tx: tokio::sync::mpsc::UnboundedSender<PatchCommand>,
    metrics_rx: tokio::sync::watch::Receiver<RequestMetrics>,
    flush_notify: std::sync::Arc<tokio::sync::Notify>,
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
}

#[derive(Debug)]
enum PatchCommand {
    UpdateState(
        serde_json::Value,
        Option<tokio::sync::oneshot::Sender<Result<(), PatchError>>>,
    ),
}

impl Patch {
    /// Creates a new PATCH client with background flushing for the specified endpoint.
    ///
    /// Automatically starts a background task that handles periodic flushing and
    /// responds to manual flush notifications. The task is terminated when the
    /// Patch instance is dropped.
    ///
    /// # Arguments
    /// * `endpoint` - The full URL to make PATCH requests to
    /// * `config` - Request configuration including timeouts, rate limiting, and optional authentication
    ///
    /// # Example
    /// ```rust,ignore
    /// use std::time::Duration;
    ///
    /// let config = RequestConfig {
    ///     timeout: Duration::from_secs(30),
    ///     min_interval: Duration::from_secs(5),
    ///     max_backoff: Duration::from_secs(300),
    ///     api_token: None,
    /// }.with_api_token("your-api-token");
    ///
    /// let client = Patch::new("https://api.example.com/device/state", config);
    /// ```
    pub fn new(endpoint: impl Into<String>, config: RequestConfig) -> Self {
        let (patch_tx, patch_rx) = tokio::sync::mpsc::unbounded_channel();
        let (metrics_tx, metrics_rx) = tokio::sync::watch::channel(RequestMetrics {
            success_count: 0,
            error_count: 0,
        });
        let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);
        let flush_notify = std::sync::Arc::new(tokio::sync::Notify::new());

        let service = Self {
            patch_tx,
            metrics_rx,
            flush_notify: flush_notify.clone(),
            shutdown_tx: shutdown_tx.clone(),
        };
        let endpoint = endpoint.into();

        // Spawn background task
        tokio::spawn(async move {
            Self::background_task(
                RequestState::new(endpoint, config),
                patch_rx,
                metrics_tx,
                flush_notify,
                shutdown_rx,
            )
            .await;
        });

        service
    }

    /// Queues a new state for transmission to the server.
    ///
    /// Returns a PatchResponse that can be awaited to know when the patch completes
    /// or is interrupted. The patch processing continues in the background whether
    /// the returned future is awaited or not.
    ///
    /// If multiple patch calls are made rapidly, newer state overwrites pending
    /// state (natural batching), reducing server load and network traffic.
    ///
    /// # Arguments
    /// * `new_state` - JSON value representing the new state to send
    ///
    /// # Returns
    /// A PatchResponse future that resolves to:
    /// - `Ok(())` if the request was successful (2xx status code)
    /// - `Err(PatchError::Status(status))` if the request failed with an error status
    /// - `Err(PatchError::Incomplete)` if this request was overwritten by a newer one or the client was dropped
    ///
    /// # Example
    /// ```rust,ignore
    /// use serde_json::json;
    ///
    /// // Fire and forget (processing continues in background)
    /// let _response = client.patch(json!({"status": "running"}));
    ///
    /// // Wait for completion
    /// let result = client.patch(json!({"status": "idle"})).await;
    /// match result {
    ///     Ok(()) => {
    ///         println!("Patch succeeded");
    ///     }
    ///     Err(PatchError::Status(status)) => {
    ///         println!("Patch failed with status: {}", status);
    ///     }
    ///     Err(PatchError::Incomplete) => {
    ///         println!("Request was replaced by a newer one or client was dropped");
    ///     }
    /// }
    /// ```
    pub fn patch(&mut self, new_state: serde_json::Value) -> PatchRequest {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        if self
            .patch_tx
            .send(PatchCommand::UpdateState(new_state, Some(result_tx)))
            .is_ok()
        {
            self.flush_notify.notify_one();
        }

        PatchRequest { result_rx }
    }

    async fn background_task(
        mut state: RequestState,
        mut patch_rx: tokio::sync::mpsc::UnboundedReceiver<PatchCommand>,
        metrics_tx: tokio::sync::watch::Sender<RequestMetrics>,
        flush_notify: std::sync::Arc<tokio::sync::Notify>,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) {
        let mut pending_state: Option<serde_json::Value> = None;
        let mut current_result_tx: Option<tokio::sync::oneshot::Sender<Result<(), PatchError>>> =
            None;

        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    break;
                }
                cmd = patch_rx.recv() => {
                    match cmd {
                        Some(PatchCommand::UpdateState(new_state, result_tx)) => {
                            // If there's a previous pending request, notify it was replaced
                            if let Some(old_tx) = current_result_tx.take() {
                                let _ = old_tx.send(Err(PatchError::Incomplete));
                            }

                            // Update the pending state and result channel
                            pending_state = Some(new_state);
                            current_result_tx = result_tx;
                        }
                        None => break, // Channel closed
                    }
                }
                _ = flush_notify.notified() => {
                    // Small delay to allow batching of rapid requests
                    tokio::time::sleep(Duration::from_millis(10)).await;

                    // Drain any additional updates that came in during the delay
                    while let Ok(cmd) = patch_rx.try_recv() {
                        match cmd {
                            PatchCommand::UpdateState(new_state, result_tx) => {
                                // If there's a previous pending request, notify it was replaced
                                if let Some(old_tx) = current_result_tx.take() {
                                    let _ = old_tx.send(Err(PatchError::Incomplete));
                                }

                                pending_state = Some(new_state);
                                current_result_tx = result_tx;
                            }
                        }
                    }

                    Self::handle_pending_state(&mut state, &mut pending_state, &mut current_result_tx, &metrics_tx).await;
                }
                _ = tokio::time::sleep(state.current_backoff), if pending_state.is_some() => {
                    Self::handle_pending_state(&mut state, &mut pending_state, &mut current_result_tx, &metrics_tx).await;
                }
            }
        }
    }

    async fn handle_pending_state(
        state: &mut RequestState,
        pending_state: &mut Option<serde_json::Value>,
        current_result_tx: &mut Option<tokio::sync::oneshot::Sender<Result<(), PatchError>>>,
        metrics_tx: &tokio::sync::watch::Sender<RequestMetrics>,
    ) {
        if let Some(state_to_send) = pending_state.take() {
            let result_tx = current_result_tx.take();

            match Self::try_patch(state, state_to_send.clone()).await {
                Ok(_status) => {
                    // Notify success
                    if let Some(tx) = result_tx {
                        let _ = tx.send(Ok(()));
                    }

                    let _ = metrics_tx.send(RequestMetrics {
                        success_count: state.success_count,
                        error_count: state.error_count,
                    });
                }
                Err(TryPatchError::WillRetry(_, _)) => {
                    // Retryable error - put state back and retry later with backoff
                    *pending_state = Some(state_to_send);
                    *current_result_tx = result_tx; // Keep the result channel for retry

                    let _ = metrics_tx.send(RequestMetrics {
                        success_count: state.success_count,
                        error_count: state.error_count,
                    });
                }
                Err(TryPatchError::Status(status)) => {
                    if status.is_server_error() {
                        // 5xx server errors - retry with backoff
                        *pending_state = Some(state_to_send);
                        *current_result_tx = result_tx; // Keep the result channel for retry
                    } else {
                        // 4xx client errors - notify and don't retry
                        if let Some(tx) = result_tx {
                            let _ = tx.send(Err(PatchError::Status(status.as_u16())));
                        }
                    }

                    let _ = metrics_tx.send(RequestMetrics {
                        success_count: state.success_count,
                        error_count: state.error_count,
                    });
                }
            }
        }
    }

    #[instrument(name = "patch", skip_all, fields(status=field::Empty), err)]
    async fn try_patch(
        state: &mut RequestState,
        state_to_send: serde_json::Value,
    ) -> Result<StatusCode, TryPatchError> {
        state.wait_for_rate_limit().await;

        let mut request = state
            .client
            .patch(&state.endpoint)
            .header("Content-Type", "application/json")
            .header("Accept-Encoding", "br, gzip, deflate")
            .timeout(state.config.timeout)
            .json(&state_to_send);

        if let Some(api_token) = &state.config.api_token {
            request = request.header("Authorization", format!("Bearer {api_token}"));
        }

        let response = match request.send().await {
            Ok(value) => value,
            Err(e) => {
                state.record_failure(None);
                return Err(TryPatchError::WillRetry(
                    e.to_string(),
                    state.current_backoff,
                ));
            }
        };
        let status = response.status();

        Span::current().record("status", status.as_u16());

        match status {
            status if status.is_success() => {
                state.record_success();
                Ok(status)
            }
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                let retry_after = RequestState::parse_retry_after(&response);
                state.record_failure(retry_after);

                Err(TryPatchError::WillRetry(
                    format!("server responded with {status}"),
                    retry_after.unwrap_or(state.current_backoff),
                ))
            }
            status if status.is_server_error() => {
                // 5xx errors are server issues - retry with backoff
                state.record_failure(None);
                Err(TryPatchError::WillRetry(
                    format!("server responded with {status}"),
                    state.current_backoff,
                ))
            }
            _ => {
                // Other 4xx client errors are permanent - don't retry
                state.error_count += 1;
                // Reset the back-off since this terminates the loop
                state.current_backoff = state.config.min_interval;
                state.next_retry = Some(Instant::now() + state.config.min_interval);
                Err(TryPatchError::Status(status))
            }
        }
    }

    /// Returns current request metrics including success and error counts.
    ///
    /// Tracks metrics for background PATCH requests, including successful
    /// transmissions and various error conditions (authentication, rate limiting, etc.).
    ///
    /// # Example
    /// ```rust,ignore
    /// let metrics = client.metrics();
    /// println!("PATCH success rate: {:.1}%", metrics.success_rate());
    /// println!("Total background requests: {}", metrics.total_requests());
    /// ```
    pub fn metrics(&self) -> RequestMetrics {
        *self.metrics_rx.borrow()
    }
}

impl Drop for Patch {
    fn drop(&mut self) {
        // Send shutdown signal to the background task
        let _ = self.shutdown_tx.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::{Matcher, Server};
    use serde_json::json;
    use std::time::Duration;

    fn test_config() -> RequestConfig {
        RequestConfig {
            timeout: Duration::from_secs(10),
            min_interval: Duration::from_millis(10),
            max_backoff: Duration::from_secs(200),
            api_token: None,
        }
    }

    #[tokio::test]
    async fn test_get_request_basic() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        let mock = server
            .mock("GET", "/")
            .match_header("accept-encoding", "br, gzip, deflate")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status": "running"}"#)
            .create_async()
            .await;

        let mut client = Get::new(endpoint, test_config().with_api_token("test-token"));
        let response = client.get().await.unwrap();

        assert_eq!(response.value, Some(json!({"status": "running"})));
        assert!(response.modified);

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_request_etag_caching() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        // First request returns data with ETag
        let mock1 = server
            .mock("GET", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_header("etag", "\"version1\"")
            .with_body(r#"{"status": "running"}"#)
            .create_async()
            .await;

        let mut client = Get::new(endpoint.clone(), test_config().with_api_token("test-token"));
        let response1 = client.get().await.unwrap();

        assert_eq!(response1.value, Some(json!({"status": "running"})));
        assert!(response1.modified);

        mock1.assert_async().await;

        // Second request with If-None-Match header returns 304
        let mock2 = server
            .mock("GET", "/")
            .match_header("if-none-match", "\"version1\"")
            .with_status(304)
            .create_async()
            .await;

        let response2 = client.get().await.unwrap();

        assert_eq!(response2.value, Some(json!({"status": "running"})));
        assert!(!response2.modified);

        mock2.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_request_rate_limiting() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        let mock = server
            .mock("GET", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status": "running"}"#)
            .expect_at_least(2)
            .create_async()
            .await;

        let config = RequestConfig {
            timeout: Duration::from_secs(10),
            min_interval: Duration::from_millis(100),
            max_backoff: Duration::from_secs(60),
            api_token: None,
        }
        .with_api_token("test-token");

        let mut client = Get::new(endpoint, config);

        let start = std::time::Instant::now();
        client.get().await.unwrap();
        client.get().await.unwrap();
        let end = std::time::Instant::now();

        assert!(end.duration_since(start) >= Duration::from_millis(100));

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_request_basic() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        let mock = server
            .mock("PATCH", "/")
            .match_header("authorization", "Bearer test-token")
            .match_header("content-type", "application/json")
            .match_header("accept-encoding", "br, gzip, deflate")
            .match_body(Matcher::Json(json!({"status": "updated"})))
            .with_status(200)
            .create_async()
            .await;

        let mut client = Patch::new(endpoint, test_config().with_api_token("test-token"));

        client.patch(json!({"status": "updated"}));

        tokio::time::sleep(Duration::from_millis(15)).await;

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_request_batching() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        let mock = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "final"})))
            .with_status(200)
            .create_async()
            .await;

        let mut client = Patch::new(endpoint, test_config().with_api_token("test-token"));

        client.patch(json!({"status": "first"}));
        client.patch(json!({"status": "second"}));
        client.patch(json!({"status": "final"}));

        tokio::time::sleep(Duration::from_millis(15)).await;

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_request_metrics() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        let mock = server
            .mock("PATCH", "/")
            .with_status(200)
            .create_async()
            .await;

        let mut client = Patch::new(endpoint, test_config().with_api_token("test-token"));

        let metrics_before = client.metrics();
        assert_eq!(metrics_before.success_count, 0);
        assert_eq!(metrics_before.error_count, 0);

        client.patch(json!({"status": "test"}));

        tokio::time::sleep(Duration::from_millis(25)).await;

        let metrics_after = client.metrics();
        assert_eq!(metrics_after.success_count, 1);
        assert_eq!(metrics_after.error_count, 0);

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_authentication_error() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        let mock = server
            .mock("GET", "/")
            .with_status(401)
            .create_async()
            .await;

        let mut client = Get::new(endpoint, test_config().with_api_token("invalid-token"));
        let response = client.get().await;

        assert!(matches!(response, Err(GetError::Unauthorized)));

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_request_error() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        // Request returns invalid JSON - should now return error immediately
        let mock = server
            .mock("GET", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("invalid json")
            .create_async()
            .await;

        let config = RequestConfig {
            timeout: Duration::from_secs(10),
            min_interval: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
            api_token: None,
        }
        .with_api_token("test-token");

        let mut client = Get::new(endpoint, config);
        let response = client.get().await;

        // Should return error for invalid JSON instead of retrying
        assert!(matches!(response, Err(GetError::Serialization(_))));

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_do_not_fallback_on_error() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        // First request succeeds and caches data
        let mock1 = server
            .mock("GET", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status": "running"}"#)
            .create_async()
            .await;

        let config = RequestConfig {
            timeout: Duration::from_secs(10),
            min_interval: Duration::from_millis(10), // Very short for test
            max_backoff: Duration::from_millis(50),  // Very short for test
            api_token: None,
        }
        .with_api_token("test-token");

        let mut client = Get::new(endpoint.clone(), config);
        let response1 = client.get().await.unwrap();

        assert_eq!(response1.value, Some(json!({"status": "running"})));
        assert!(response1.modified);

        mock1.assert_async().await;

        // First few requests fail, then succeeds
        let mock2 = server
            .mock("GET", "/")
            .with_status(500)
            .expect(2) // Fail twice
            .create_async()
            .await;

        let mock3 = server
            .mock("GET", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status": "recovered"}"#)
            .create_async()
            .await;

        let response2 = client.get().await.unwrap();

        assert_eq!(response2.value, Some(json!({"status": "recovered"})));
        assert!(response2.modified);

        mock2.assert_async().await;
        mock3.assert_async().await;
    }

    #[tokio::test]
    async fn test_retry_after_header() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        // First request gets rate limited with retry-after
        let mock1 = server
            .mock("GET", "/")
            .with_status(429)
            .with_header("retry-after", "1") // 1 second for faster test
            .create_async()
            .await;

        // Second request succeeds
        let mock2 = server
            .mock("GET", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status": "success"}"#)
            .create_async()
            .await;

        let config = RequestConfig {
            timeout: Duration::from_secs(10),
            min_interval: Duration::from_millis(10),
            max_backoff: Duration::from_secs(100),
            api_token: None,
        }
        .with_api_token("test-token");

        let mut client = Get::new(endpoint, config);

        // Should eventually succeed after rate limit expires
        let start_time = std::time::Instant::now();
        let result = client.get().await.unwrap();
        let elapsed = start_time.elapsed();

        // Verify that at least 1 second passed (respecting the retry-after header)
        assert!(
            elapsed >= Duration::from_millis(900),
            "Request should have waited for retry-after, but only took {elapsed:#?}",
        );
        assert_eq!(result.value, Some(json!({"status": "success"})));
        assert!(result.modified);

        mock1.assert_async().await;
        mock2.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_drop_terminates_task() {
        let server = Server::new_async().await;
        let endpoint = server.url();

        // Create a patch request and immediately drop it
        {
            let _client = Patch::new(endpoint, test_config().with_api_token("test-token"));
            // Request goes out of scope here and should be dropped
        }

        // Give some time for the drop to take effect
        tokio::time::sleep(Duration::from_millis(50)).await;

        // If the task is properly terminated, this test should complete without hanging
        // The fact that we reach this point means the drop worked correctly
    }

    #[tokio::test]
    async fn test_patch_permanent_error_discards_state() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        // First request returns 500 error
        let mock0 = server
            .mock("PATCH", "/")
            .with_status(500)
            .create_async()
            .await;

        // Second request returns 400 Bad Request
        let mock1 = server
            .mock("PATCH", "/")
            .with_status(400)
            .create_async()
            .await;

        // Thid request should succeed (new patch request)
        let mock2 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "new_request"})))
            .with_status(200)
            .create_async()
            .await;

        let mut client = Patch::new(endpoint, test_config().with_api_token("test-token"));

        // Send first patch that will fail with 500
        client.patch(json!({"status": "will_fail"}));

        // Wait for the failed request to be processed
        tokio::time::sleep(Duration::from_millis(25)).await;

        // Send second patch that will fail with 400
        client.patch(json!({"status": "will_also_fail"}));

        // Wait for the failed request to be processed
        tokio::time::sleep(Duration::from_millis(15)).await;

        // Send third patch - this should succeed and not be blocked by the second
        // and time should have been reset at this point
        client.patch(json!({"status": "new_request"}));

        // Wait for the second request to be processed
        tokio::time::sleep(Duration::from_millis(15)).await;

        mock0.assert_async().await;
        mock1.assert_async().await;
        mock2.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_authentication_error_discards_state() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        // First request returns 401 Unauthorized
        let mock1 = server
            .mock("PATCH", "/")
            .with_status(401)
            .create_async()
            .await;

        // Second request should succeed (new patch request)
        let mock2 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "new_after_auth_error"})))
            .with_status(200)
            .create_async()
            .await;

        let mut client = Patch::new(endpoint, test_config().with_api_token("invalid-token"));

        // Send first patch that will fail with 401
        client.patch(json!({"status": "will_fail_auth"}));

        // Wait for the failed request to be processed
        tokio::time::sleep(Duration::from_millis(15)).await;

        // Send second patch - this should succeed and not be blocked by the first
        client.patch(json!({"status": "new_after_auth_error"}));

        // Wait for the second request to be processed
        tokio::time::sleep(Duration::from_millis(15)).await;

        mock1.assert_async().await;
        mock2.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_not_found_error_discards_state() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        // First request returns 404 Not Found
        let mock1 = server
            .mock("PATCH", "/")
            .with_status(404)
            .create_async()
            .await;

        // Second request should succeed (new patch request)
        let mock2 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "new_after_404"})))
            .with_status(200)
            .create_async()
            .await;

        let mut client = Patch::new(endpoint, test_config().with_api_token("test-token"));

        // Send first patch that will fail with 404
        client.patch(json!({"status": "will_fail_404"}));

        // Wait for the failed request to be processed
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Send second patch - this should succeed and not be blocked by the first
        client.patch(json!({"status": "new_after_404"}));

        // Wait for the second request to be processed
        tokio::time::sleep(Duration::from_millis(20)).await;

        mock1.assert_async().await;
        mock2.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_rate_limited_retries_with_backoff() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        // First request returns 429 Rate Limited
        let mock1 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "will_be_rate_limited"})))
            .with_status(429)
            .with_header("retry-after", "1")
            .create_async()
            .await;

        // Second request (retry) should succeed with same data
        let mock2 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "will_be_rate_limited"})))
            .with_status(200)
            .create_async()
            .await;

        let config = RequestConfig {
            timeout: Duration::from_secs(10),
            min_interval: Duration::from_millis(10),
            max_backoff: Duration::from_secs(100),
            api_token: None,
        }
        .with_api_token("test-token");

        let mut client = Patch::new(endpoint, config);

        // Send patch that will be rate limited and then retried
        client.patch(json!({"status": "will_be_rate_limited"}));

        // Wait for both the rate limited request and the retry
        tokio::time::sleep(Duration::from_millis(1200)).await;

        mock1.assert_async().await;
        mock2.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_network_error_retries_with_backoff() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        // First request will fail due to server shutdown (network error)
        let mock1 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "network_test"})))
            .with_status(500) // Server error that should be retried
            .create_async()
            .await;

        // Second request (retry) should succeed with same data
        let mock2 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "network_test"})))
            .with_status(200)
            .create_async()
            .await;

        let config = RequestConfig {
            timeout: Duration::from_secs(10),
            min_interval: Duration::from_millis(10),
            max_backoff: Duration::from_millis(100), // Short for test
            api_token: None,
        }
        .with_api_token("test-token");

        let mut client = Patch::new(endpoint, config);

        // Send patch that will fail with server error and then be retried
        client.patch(json!({"status": "network_test"}));

        // Wait for both the failed request and the retry
        tokio::time::sleep(Duration::from_millis(50)).await;

        mock1.assert_async().await;
        mock2.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_new_request_overwrites_pending_retry() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        // First request returns 500 (will be retried)
        let mock1 = server
            .mock("PATCH", "/")
            .with_status(500)
            .create_async()
            .await;

        // Only the newer request should be sent on retry
        let mock2 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "newer_request"})))
            .with_status(200)
            .create_async()
            .await;

        let config = RequestConfig {
            timeout: Duration::from_secs(10),
            min_interval: Duration::from_millis(10),
            max_backoff: Duration::from_millis(100),
            api_token: None,
        }
        .with_api_token("test-token");

        let mut client = Patch::new(endpoint, config);

        // Send first patch that will fail
        client.patch(json!({"status": "will_fail"}));

        // Wait for the failure
        tokio::time::sleep(Duration::from_millis(25)).await;

        // Send new patch before retry happens - this should overwrite the pending retry
        client.patch(json!({"status": "newer_request"}));

        // Wait for the retry/new request
        tokio::time::sleep(Duration::from_millis(50)).await;

        mock1.assert_async().await;
        mock2.assert_async().await;

        // Verify the first request body was never retried
        // (this is implicitly tested by mock2 matching only the newer request)
    }

    #[tokio::test]
    async fn test_patch_4xx_vs_5xx_error_handling() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        // Test 409 Conflict (4xx) - should be permanent, no retry
        let mock_409 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "conflict_test"})))
            .with_status(409)
            .expect(1) // Should only be called once, no retry
            .create_async()
            .await;

        // Test 502 Bad Gateway (5xx) - should be retried
        let mock_502_fail = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "server_error_test"})))
            .with_status(502)
            .create_async()
            .await;

        let mock_502_success = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "server_error_test"})))
            .with_status(200)
            .create_async()
            .await;

        let config = RequestConfig {
            timeout: Duration::from_secs(10),
            min_interval: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50), // Short for test
            api_token: None,
        }
        .with_api_token("test-token");

        let mut client = Patch::new(endpoint, config);

        // Send 409 request - should fail permanently
        client.patch(json!({"status": "conflict_test"}));
        tokio::time::sleep(Duration::from_millis(30)).await;

        // Send 502 request - should be retried and eventually succeed
        client.patch(json!({"status": "server_error_test"}));
        tokio::time::sleep(Duration::from_millis(100)).await;

        mock_409.assert_async().await;
        mock_502_fail.assert_async().await;
        mock_502_success.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_response_success() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        let mock = server
            .mock("PATCH", "/")
            .with_status(201)
            .create_async()
            .await;

        let mut client = Patch::new(endpoint, test_config().with_api_token("test-token"));

        let request = client.patch(json!({"status": "test"}));
        let result = request.await;

        assert!(result.is_ok());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_response_client_error() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        let mock = server
            .mock("PATCH", "/")
            .with_status(400)
            .create_async()
            .await;

        let mut client = Patch::new(endpoint, test_config().with_api_token("test-token"));

        let request = client.patch(json!({"status": "test"}));
        let result = request.await;

        match result {
            Err(PatchError::Status(400)) => {}
            _ => panic!("Expected status 400, got {result:?}"),
        }
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_response_replaced() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        let mock = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "final"})))
            .with_status(200)
            .create_async()
            .await;

        let mut client = Patch::new(endpoint, test_config().with_api_token("test-token"));

        let request1 = client.patch(json!({"status": "first"}));
        let request2 = client.patch(json!({"status": "second"}));
        let request3 = client.patch(json!({"status": "final"}));

        let result1 = request1.await;
        let result2 = request2.await;
        let result3 = request3.await;

        assert!(matches!(result1, Err(PatchError::Incomplete)));
        assert!(matches!(result2, Err(PatchError::Incomplete)));
        assert!(result3.is_ok());

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_response_dropped() {
        let server = Server::new_async().await;
        let endpoint = server.url();

        let request = {
            let mut client = Patch::new(endpoint, test_config().with_api_token("test-token"));
            client.patch(json!({"status": "will_be_dropped"}))
            // request is dropped here
        };

        let response = request.await;
        assert!(matches!(response, Err(PatchError::Incomplete)));
    }

    #[test]
    fn test_request_metrics_methods() {
        // Test with no requests
        let metrics = RequestMetrics {
            success_count: 0,
            error_count: 0,
        };
        assert_eq!(metrics.total_requests(), 0);
        assert_eq!(metrics.success_rate(), 0.0);
        assert_eq!(metrics.error_rate(), 0.0);

        // Test with only successes
        let metrics = RequestMetrics {
            success_count: 10,
            error_count: 0,
        };
        assert_eq!(metrics.total_requests(), 10);
        assert_eq!(metrics.success_rate(), 100.0);
        assert_eq!(metrics.error_rate(), 0.0);

        // Test with only errors
        let metrics = RequestMetrics {
            success_count: 0,
            error_count: 5,
        };
        assert_eq!(metrics.total_requests(), 5);
        assert_eq!(metrics.success_rate(), 0.0);
        assert_eq!(metrics.error_rate(), 100.0);

        // Test with mixed results
        let metrics = RequestMetrics {
            success_count: 7,
            error_count: 3,
        };
        assert_eq!(metrics.total_requests(), 10);
        assert_eq!(metrics.success_rate(), 70.0);
        assert_eq!(metrics.error_rate(), 30.0);
    }
}
