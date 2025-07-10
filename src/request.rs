use axum::http::uri::PathAndQuery;
use hyper::Uri;
use reqwest::StatusCode;
use std::{
    str::FromStr,
    time::{Duration, Instant},
};
use thiserror::Error;
use tracing::{field, instrument, Span};

#[derive(Debug, Error)]
pub enum UriError {
    #[error(transparent)]
    InvalidUri(#[from] axum::http::uri::InvalidUri),

    #[error(transparent)]
    InvalidUriParts(#[from] axum::http::uri::InvalidUriParts),
}

pub fn make_uri(base_uri: Uri, path: &str, query: Option<&str>) -> Result<Uri, UriError> {
    // Build the URI from the address parts
    let mut parts = base_uri.into_parts();
    parts.path_and_query = if let Some(qs) = query {
        Some(PathAndQuery::from_maybe_shared(format!("{path}?{qs}",))?)
    } else {
        Some(PathAndQuery::from_str(path)?)
    };
    Uri::from_parts(parts).map_err(|err| err.into())
}

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

impl From<TryPatchError> for PatchError {
    fn from(err: TryPatchError) -> Self {
        match err {
            TryPatchError::Status(status) => PatchError(status.as_u16()),
            TryPatchError::WillRetry(_, _) => unreachable!(),
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

#[derive(Debug, Error)]
#[error("Request failed with status code {0}")]
pub struct PatchError(pub u16);

/// HTTP PATCH response
pub type PatchResponse = ();

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
}

#[derive(Clone)]
struct RequestState {
    client: reqwest::Client,
    endpoint: String,
    config: RequestConfig,
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
            next_retry: None,
            current_backoff,
            success_count: 0,
            error_count: 0,
        }
    }

    fn reset_interval(&mut self) {
        self.current_backoff = self.config.min_interval;
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
    cached: Option<serde_json::Value>,
    etag: Option<String>,
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
            cached: None,
            etag: None,
        }
    }

    #[instrument(level = "trace", skip_all, fields(response=field::Empty) err)]
    async fn try_get(&mut self) -> Result<GetResponse, TryGetError> {
        self.state.wait_for_rate_limit().await;

        // Reset the interval in case the future gets dropped before a response
        // is received
        self.state.reset_interval();

        let mut request = self
            .state
            .client
            .get(&self.state.endpoint)
            .timeout(self.state.config.timeout)
            .header("Accept-Encoding", "br, gzip, deflate");

        if let Some(api_token) = &self.state.config.api_token {
            request = request.header("Authorization", format!("Bearer {api_token}"));
        }

        if let Some(etag) = &self.etag {
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

        Span::current().record("response", field::display(status));
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

                self.cached = Some(json.clone());
                self.etag = new_etag;
                self.state.record_success();

                Ok(GetResponse {
                    value: Some(json),
                    modified: true,
                })
            }
            StatusCode::NOT_MODIFIED => {
                self.state.record_success();
                Ok(GetResponse {
                    value: self.cached.clone(),
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
    #[instrument(level="debug", skip_all, fields(retries=field::Empty), err)]
    pub async fn get(&mut self) -> Result<GetResponse, GetError> {
        let mut tries = 1;
        loop {
            match self.try_get().await {
                Ok(response) => {
                    Span::current().record("retries", tries - 1);
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

/// HTTP PATCH client with rate limiting and automatic retries.
///
/// Supports blocking patch operations with exponential backoff for error handling.
pub struct Patch {
    state: RequestState,
}

impl Patch {
    /// Creates a new PATCH client for the specified endpoint.
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
        Self {
            state: RequestState::new(endpoint.into(), config),
        }
    }

    /// Performs an HTTP PATCH request with automatic retries and error handling.
    ///
    /// Blocks until the request completes successfully or fails with a permanent error.
    /// Uses exponential backoff for retryable errors (rate limiting, server errors).
    ///
    /// # Arguments
    /// * `new_state` - JSON value representing the new state to send
    ///
    /// # Returns
    /// * `Ok(())` - Request was successful (2xx status code)
    /// * `Err(PatchError)` - Authentication failed or permanent client error
    ///
    /// # Example
    /// ```rust,ignore
    /// use serde_json::json;
    ///
    /// let result = client.patch(json!({"status": "running"})).await?;
    /// println!("Patch succeeded");
    /// ```
    #[instrument(level="debug", skip_all, fields(retries=field::Empty), err)]
    pub async fn patch(
        &mut self,
        new_state: serde_json::Value,
    ) -> Result<PatchResponse, PatchError> {
        let mut tries = 1;
        loop {
            match Self::try_patch(&mut self.state, new_state.clone()).await {
                Ok(_status) => {
                    Span::current().record("retries", tries - 1);
                    return Ok(());
                }
                Err(TryPatchError::WillRetry(_, _)) => {
                    tries += 1;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    #[instrument(level = "trace", skip_all, fields(response=field::Empty), err)]
    async fn try_patch(
        state: &mut RequestState,
        state_to_send: serde_json::Value,
    ) -> Result<StatusCode, TryPatchError> {
        state.wait_for_rate_limit().await;

        // Reset the interval in case the future gets dropped before a response
        // is received
        state.reset_interval();

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
                // Re-try network errors
                state.record_failure(None);
                return Err(TryPatchError::WillRetry(
                    e.to_string(),
                    state.current_backoff,
                ));
            }
        };
        let status = response.status();

        Span::current().record("response", field::display(status));

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
    /// # Example
    /// ```rust,ignore
    /// let metrics = client.metrics();
    /// println!("PATCH success rate: {:.1}%", metrics.success_rate());
    /// println!("Total requests: {}", metrics.total_requests());
    /// ```
    pub fn metrics(&self) -> RequestMetrics {
        RequestMetrics {
            success_count: self.state.success_count,
            error_count: self.state.error_count,
        }
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

        let mut client_config = test_config();
        client_config.api_token = Some("test-token".to_string());
        let mut client = Get::new(endpoint, client_config);
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

        let mut client_config = test_config();
        client_config.api_token = Some("test-token".to_string());
        let mut client = Get::new(endpoint.clone(), client_config);
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
            api_token: Some("test-token".to_string()),
        };

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

        let mut client_config = test_config();
        client_config.api_token = Some("test-token".to_string());
        let mut client = Patch::new(endpoint, client_config);

        client.patch(json!({"status": "updated"})).await.unwrap();

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_request_sequential() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        let mock1 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "first"})))
            .with_status(200)
            .create_async()
            .await;

        let mock2 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "second"})))
            .with_status(200)
            .create_async()
            .await;

        let mock3 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "final"})))
            .with_status(200)
            .create_async()
            .await;

        let mut client_config = test_config();
        client_config.api_token = Some("test-token".to_string());
        let mut client = Patch::new(endpoint, client_config);

        client.patch(json!({"status": "first"})).await.unwrap();
        client.patch(json!({"status": "second"})).await.unwrap();
        client.patch(json!({"status": "final"})).await.unwrap();

        mock1.assert_async().await;
        mock2.assert_async().await;
        mock3.assert_async().await;
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

        let mut client_config = test_config();
        client_config.api_token = Some("test-token".to_string());
        let mut client = Patch::new(endpoint, client_config);

        let metrics_before = client.metrics();
        assert_eq!(metrics_before.success_count, 0);
        assert_eq!(metrics_before.error_count, 0);

        client.patch(json!({"status": "test"})).await.unwrap();

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

        let mut client_config = test_config();
        client_config.api_token = Some("invalid-token".to_string());
        let mut client = Get::new(endpoint, client_config);
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
            api_token: Some("test-token".to_string()),
        };

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
            api_token: Some("test-token".to_string()),
        };

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
            api_token: Some("test-token".to_string()),
        };

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
    async fn test_patch_error_handling() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        // First request returns 400 Bad Request
        let mock1 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "will_fail"})))
            .with_status(400)
            .create_async()
            .await;

        // Second request should succeed
        let mock2 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "success"})))
            .with_status(200)
            .create_async()
            .await;

        let mut client_config = test_config();
        client_config.api_token = Some("invalid-token".to_string());
        let mut client = Patch::new(endpoint, client_config);

        // First patch fails with 400
        let result1 = client.patch(json!({"status": "will_fail"})).await;
        assert!(matches!(result1, Err(PatchError(400))));

        // Second patch should succeed
        client.patch(json!({"status": "success"})).await.unwrap();

        mock1.assert_async().await;
        mock2.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_authentication_error() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        // First request returns 401 Unauthorized
        let mock1 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "will_fail_auth"})))
            .with_status(401)
            .create_async()
            .await;

        // Second request should succeed
        let mock2 = server
            .mock("PATCH", "/")
            .match_body(Matcher::Json(json!({"status": "success"})))
            .with_status(200)
            .create_async()
            .await;

        let mut client_config = test_config();
        client_config.api_token = Some("invalid-token".to_string());
        let mut client = Patch::new(endpoint, client_config);

        // First patch fails with 401
        let result1 = client.patch(json!({"status": "will_fail_auth"})).await;
        assert!(matches!(result1, Err(PatchError(401))));

        // Second patch should succeed
        client.patch(json!({"status": "success"})).await.unwrap();

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
            api_token: Some("test-token".to_string()),
        };

        let mut client = Patch::new(endpoint, config);

        // Send patch that will be rate limited and then retried - this should block until success
        let start_time = std::time::Instant::now();
        client
            .patch(json!({"status": "will_be_rate_limited"}))
            .await
            .unwrap();
        let elapsed = start_time.elapsed();

        // Verify that at least 1 second passed (respecting the retry-after header)
        assert!(
            elapsed >= Duration::from_millis(900),
            "Request should have waited for retry-after, but only took {elapsed:#?}",
        );

        mock1.assert_async().await;
        mock2.assert_async().await;
    }

    #[tokio::test]
    async fn test_patch_network_error_retries_with_backoff() {
        let mut server = Server::new_async().await;
        let endpoint = server.url();

        // First request will fail with server error
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
            api_token: Some("test-token".to_string()),
        };

        let mut client = Patch::new(endpoint, config);

        // Send patch that will fail with server error and then be retried - this should block until success
        client
            .patch(json!({"status": "network_test"}))
            .await
            .unwrap();

        mock1.assert_async().await;
        mock2.assert_async().await;
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
            api_token: Some("test-token".to_string()),
        };

        let mut client = Patch::new(endpoint, config);

        // Send 409 request - should fail permanently
        let result1 = client.patch(json!({"status": "conflict_test"})).await;
        assert!(matches!(result1, Err(PatchError(409))));

        // Send 502 request - should be retried and eventually succeed
        client
            .patch(json!({"status": "server_error_test"}))
            .await
            .unwrap();

        mock_409.assert_async().await;
        mock_502_fail.assert_async().await;
        mock_502_success.assert_async().await;
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

        // Test with only successes
        let metrics = RequestMetrics {
            success_count: 10,
            error_count: 0,
        };
        assert_eq!(metrics.total_requests(), 10);
        assert_eq!(metrics.success_rate(), 100.0);

        // Test with only errors
        let metrics = RequestMetrics {
            success_count: 0,
            error_count: 5,
        };
        assert_eq!(metrics.total_requests(), 5);
        assert_eq!(metrics.success_rate(), 0.0);

        // Test with mixed results
        let metrics = RequestMetrics {
            success_count: 7,
            error_count: 3,
        };
        assert_eq!(metrics.total_requests(), 10);
        assert_eq!(metrics.success_rate(), 70.0);
    }
}
