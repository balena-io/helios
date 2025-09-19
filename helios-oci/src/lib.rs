use std::fmt;
use std::time::Duration;

use bollard::Docker;

pub use bollard::auth::DockerCredentials as Credentials;
pub use bollard::errors::Error as ConnectionError;

mod image;
pub use image::{Image, LocalImage};

mod models;
pub use models::{ImageUri, InvalidImageUriError};

mod registry;
pub use registry::{RegistryAuth, RegistryAuthClient, RegistryAuthError};

#[derive(Debug, Clone)]
pub struct Client(Docker);

impl Client {
    /// Connect to the daemon based on the `DOCKER_HOST` environment variable.
    pub async fn connect() -> Result<Self> {
        let inner = Docker::connect_with_defaults()?;

        // Bollard doesn't actually connect with the `connect_*` call.
        // Do a /ping to ensure we can connect before proceeding.
        inner
            .ping()
            .await
            .map_err(Error::with_context("failed to connect to daemon"))?;

        // TODO: determine which engine it is
        // let version_info = inner
        //     .version()
        //     .await
        //     .map_err(Error::with_context("failed to fetch version info"))?;

        Ok(Self(inner))
    }

    #[inline]
    fn inner(&self) -> &Docker {
        &self.0
    }

    #[inline]
    fn inner_mut(&mut self) -> &mut Docker {
        &mut self.0
    }

    /// Sets the request timeout, affecting all subsequent requests to the daemon.
    /// Default is 2 minutes.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.inner_mut().set_timeout(timeout);
    }

    /// Sets the request timeout, affecting all subsequent requests to the daemon.
    /// Default is 2 minutes.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.set_timeout(timeout);
        self
    }

    /// Exposes methods to work with images.
    #[inline]
    pub fn image(&self) -> Image<'_> {
        Image::new(self)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(c) = &self.1 {
            c.fmt(f)?;
            ": ".fmt(f)?;
        }
        self.0.fmt(f)
    }
}

#[derive(Debug, thiserror::Error)]
pub struct Error(ConnectionError, Option<String>);

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    #[inline]
    pub fn new(source: ConnectionError, context: Option<String>) -> Self {
        Self(source, context)
    }

    /// Returns a `ClientError` partial constructor with the given message as context.
    #[inline]
    pub fn with_context(msg: &'static str) -> impl (FnOnce(ConnectionError) -> Self) {
        move |source| Error(source, Some(msg.to_owned()))
    }

    /// Adds extra context to this error, prepending the message to the previous
    /// context if one exists.
    #[inline]
    pub fn context(mut self, msg: String) -> Self {
        self.1 = if let Some(ctx) = self.1 {
            Some(format!("{msg}: {ctx}"))
        } else {
            Some(msg)
        };
        self
    }
}

impl From<ConnectionError> for Error {
    #[inline]
    fn from(value: ConnectionError) -> Self {
        Self::new(value, None)
    }
}

/// Adds methods to [`Result`][std::result::Result] to associate extra context with an [Error].
pub trait WithContext<T>: Sized {
    /// Associates extra context with the [Error], if `self` is [Err].
    /// To provide a [String] as context, potentially with formatting, use
    /// [WithContext::with_context_lazy].
    fn with_context(self, msg: &'static str) -> Result<T>;

    /// Associates extra context with the [Error], if `self` is [Err].
    fn with_context_lazy<F>(self, f: F) -> Result<T>
    where
        F: FnOnce() -> String;
}

impl<T> WithContext<T> for Result<T> {
    fn with_context(self, msg: &'static str) -> Result<T> {
        self.map_err(|err| err.context(msg.to_owned()))
    }

    fn with_context_lazy<F>(self, f: F) -> Result<T>
    where
        F: FnOnce() -> String,
    {
        self.map_err(|err| err.context(f()))
    }
}
