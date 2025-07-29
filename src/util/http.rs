#[allow(unused)]
pub use client::{Auth, Client, ClientError, HeaderMap, Headers, Method, Response, StatusCode};
pub use uri::{InvalidUriError, Uri};

mod uri {
    use std::fmt::Display;
    use std::str::FromStr;

    use axum::http;
    use serde::{Deserialize, Serialize};
    use thiserror::Error;

    #[derive(Debug, Error)]
    pub struct InvalidUriError(String);

    impl InvalidUriError {
        pub fn reason(&self) -> &str {
            self.0.as_str()
        }
    }

    impl Display for InvalidUriError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.fmt(f)
        }
    }

    impl From<http::uri::InvalidUri> for InvalidUriError {
        fn from(value: http::uri::InvalidUri) -> Self {
            InvalidUriError(value.to_string())
        }
    }

    impl From<http::uri::InvalidUriParts> for InvalidUriError {
        fn from(value: http::uri::InvalidUriParts) -> Self {
            InvalidUriError(value.to_string())
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub struct Uri(http::Uri);

    impl Uri {
        pub fn new(uri: http::Uri) -> Self {
            Self(uri)
        }

        pub fn from_static(src: &'static str) -> Self {
            Self(http::Uri::from_static(src))
        }

        pub fn from_string(src: String) -> Result<Self, InvalidUriError> {
            Ok(Self(http::uri::Uri::from_maybe_shared(src)?))
        }

        pub fn from_parts(
            base_uri: Uri,
            path: &str,
            query: Option<&str>,
        ) -> Result<Self, InvalidUriError> {
            let path_and_query = if let Some(qs) = query {
                http::uri::PathAndQuery::from_maybe_shared(format!("{path}?{qs}",))?
            } else {
                http::uri::PathAndQuery::from_str(path)?
            };
            let mut parts = base_uri.0.into_parts();
            parts.path_and_query = Some(path_and_query);

            Ok(http::Uri::from_parts(parts).map(Self::new)?)
        }
    }

    impl Display for Uri {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.fmt(f)
        }
    }

    impl FromStr for Uri {
        type Err = InvalidUriError;

        fn from_str(s: &str) -> Result<Self, Self::Err> {
            Ok(http::Uri::from_str(s).map(Self::new)?)
        }
    }

    impl TryFrom<String> for Uri {
        type Error = InvalidUriError;

        fn try_from(value: String) -> Result<Self, Self::Error> {
            Ok(Self(http::Uri::from_maybe_shared(value)?))
        }
    }

    impl From<http::Uri> for Uri {
        fn from(value: http::Uri) -> Self {
            Self(value)
        }
    }

    impl From<Uri> for http::Uri {
        fn from(value: Uri) -> Self {
            value.0
        }
    }

    impl Serialize for Uri {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            serializer.serialize_str(&self.to_string())
        }
    }

    impl<'de> Deserialize<'de> for Uri {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            let s = String::deserialize(deserializer)?;
            s.parse().map_err(serde::de::Error::custom)
        }
    }
}

mod client {
    use std::collections::HashMap;
    use std::convert::TryInto;
    use std::error::Error;
    use std::time::Duration;

    use axum::body::Bytes;
    use axum::http;
    use futures_core::TryStream;
    use futures_lite::StreamExt;
    use reqwest::RequestBuilder;
    use serde::de::DeserializeOwned;
    use serde::Serialize;

    use super::uri::Uri;

    pub type Method = reqwest::Method;
    pub type StatusCode = reqwest::StatusCode;
    pub type HeaderMap = reqwest::header::HeaderMap;
    pub type Headers = HashMap<String, String>;

    #[derive(Debug, thiserror::Error)]
    pub enum ClientError {
        #[error("failed to build request: {0}")]
        Request(String),

        #[error("server replied with status: {0}")]
        Response(StatusCode),

        #[error(transparent)]
        Client(reqwest::Error),

        #[error(transparent)]
        Server(reqwest::Error),
    }

    #[derive(Debug)]
    pub struct Response(reqwest::Response);

    #[allow(unused)]
    impl Response {
        pub fn status(&self) -> StatusCode {
            self.0.status()
        }

        pub fn headers(&self) -> &HeaderMap {
            self.0.headers()
        }

        pub fn stream(self) -> impl futures_core::Stream<Item = Result<Bytes, ClientError>> {
            self.0
                .bytes_stream()
                .map(|res| res.map_err(ClientError::Server))
        }

        pub async fn json<T: DeserializeOwned>(self) -> Result<T, ClientError> {
            self.0.json().await.map_err(ClientError::Server)
        }

        pub async fn text(self) -> Result<String, ClientError> {
            self.0.text().await.map_err(ClientError::Server)
        }
    }

    #[allow(unused)]
    #[derive(Debug, Clone)]
    pub enum Auth {
        Basic {
            username: String,
            password: Option<String>,
        },
        Bearer {
            token: String,
        },
    }

    // Based on: https://github.com/ramsayleung/rspotify/blob/master/rspotify-http/src/reqwest.rs
    #[derive(Debug, Clone)]
    pub struct Client {
        client: reqwest::Client,
        timeout: Option<Duration>,
        auth: Option<Auth>,
    }

    impl Default for Client {
        /// Default client with a timeout of 59 seconds.
        fn default() -> Self {
            Self::new(Some(Duration::from_secs(59)))
        }
    }

    #[allow(unused)]
    impl Client {
        pub fn new(timeout: Option<Duration>) -> Self {
            Self {
                client: reqwest::Client::new(),
                timeout,
                auth: None,
            }
        }

        pub fn timeout(self, timeout: Option<Duration>) -> Self {
            Self {
                client: self.client,
                timeout,
                auth: self.auth,
            }
        }

        pub fn auth(self, auth: Option<Auth>) -> Self {
            Self {
                client: self.client,
                timeout: self.timeout,
                auth,
            }
        }

        pub async fn get<Response>(
            &self,
            uri: &Uri,
            headers: Option<&Headers>,
        ) -> Result<Response, ClientError>
        where
            Response: DeserializeOwned,
        {
            wrap_status_error(
                self.request(Method::GET, uri, |req| {
                    let headers = headers
                        .map(into_header_map)
                        .unwrap_or(Ok(HeaderMap::new()))?;
                    Ok(req.headers(headers))
                })
                .await?,
            )?
            .json()
            .await
        }

        pub async fn post<Payload, Response>(
            &self,
            uri: &Uri,
            headers: Option<&Headers>,
            payload: &Payload,
        ) -> Result<Response, ClientError>
        where
            Payload: Serialize + ?Sized,
            Response: DeserializeOwned,
        {
            wrap_status_error(
                self.request(Method::POST, uri, |req| {
                    let headers = headers
                        .map(into_header_map)
                        .unwrap_or(Ok(HeaderMap::new()))?;
                    Ok(req.headers(headers).json(payload))
                })
                .await?,
            )?
            .json()
            .await
        }

        pub async fn post_stream<Stream, Response>(
            &self,
            uri: &Uri,
            headers: Option<&Headers>,
            stream: Stream,
        ) -> Result<Response, ClientError>
        where
            Stream: TryStream + Send + 'static,
            Stream::Error: Into<Box<dyn Error + Send + Sync>>,
            Bytes: From<Stream::Ok>,
            Response: DeserializeOwned,
        {
            wrap_status_error(
                self.request(Method::POST, uri, |req| {
                    let headers = headers
                        .map(into_header_map)
                        .unwrap_or(Ok(HeaderMap::new()))?;
                    Ok(req
                        .headers(headers)
                        .body(reqwest::Body::wrap_stream(stream)))
                })
                .await?,
            )?
            .json()
            .await
        }

        pub async fn patch<Payload, Response>(
            &self,
            uri: &Uri,
            headers: Option<&Headers>,
            payload: &Payload,
        ) -> Result<Response, ClientError>
        where
            Payload: Serialize + ?Sized,
            Response: DeserializeOwned,
        {
            wrap_status_error(
                self.request(Method::PATCH, uri, |req| {
                    let headers = headers
                        .map(into_header_map)
                        .unwrap_or(Ok(HeaderMap::new()))?;
                    Ok(req.headers(headers).json(payload))
                })
                .await?,
            )?
            .json()
            .await
        }

        pub async fn put<Payload, Response>(
            &self,
            uri: &Uri,
            headers: Option<&Headers>,
            payload: &Payload,
        ) -> Result<Response, ClientError>
        where
            Payload: Serialize + ?Sized,
            Response: DeserializeOwned,
        {
            wrap_status_error(
                self.request(Method::PUT, uri, |req| {
                    let headers = headers
                        .map(into_header_map)
                        .unwrap_or(Ok(HeaderMap::new()))?;
                    Ok(req.headers(headers).json(payload))
                })
                .await?,
            )?
            .json()
            .await
        }

        pub async fn delete<Payload, Response>(
            &self,
            uri: &Uri,
            headers: Option<&Headers>,
            payload: &Payload,
        ) -> Result<Response, ClientError>
        where
            Payload: Serialize + ?Sized,
            Response: DeserializeOwned,
        {
            wrap_status_error(
                self.request(Method::DELETE, uri, |req| {
                    let headers = headers
                        .map(into_header_map)
                        .unwrap_or(Ok(HeaderMap::new()))?;
                    Ok(req.headers(headers).json(payload))
                })
                .await?,
            )?
            .json()
            .await
        }

        /// Thin wrapper around [reqwest::Request], this is your gateway to
        /// a fully customizable client if this type's methods won't do.
        pub async fn request<D>(
            &self,
            method: Method,
            uri: &Uri,
            decorator: D,
        ) -> Result<Response, ClientError>
        where
            D: FnOnce(RequestBuilder) -> Result<RequestBuilder, ClientError>,
        {
            let mut request = self.client.request(method, uri.to_string());

            if let Some(timeout) = self.timeout {
                request = request.timeout(timeout);
            }

            if let Some(auth) = &self.auth {
                request = match auth {
                    Auth::Basic { username, password } => {
                        request.basic_auth(username, password.clone())
                    }
                    Auth::Bearer { token } => request.bearer_auth(token),
                };
            };

            request = decorator(request)?;

            Ok(Response(request.send().await.map_err(ClientError::Client)?))
        }
    }

    /// Convert headers into a [HeaderMap].
    ///
    /// This will return a [ClientError] for any non-ASCII keys or values.
    fn into_header_map(headers: &Headers) -> Result<HeaderMap, ClientError> {
        headers
            .try_into()
            .map_err(|err: http::Error| ClientError::Request(err.to_string()))
    }

    fn wrap_status_error(res: Response) -> Result<Response, ClientError> {
        match res.status() {
            status if status.is_success() => Ok(res),
            status => Err(ClientError::Response(status)),
        }
    }
}
