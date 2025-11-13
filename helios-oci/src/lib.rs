use std::fmt;

use bollard::Docker;

pub use bollard::auth::DockerCredentials as Credentials;
pub use bollard::errors::Error as ConnectionError;

mod image;
pub use image::{Image, LocalImage};

mod registry;
pub use registry::{RegistryAuth, RegistryAuthClient, RegistryAuthError};

mod container;
pub use container::Container;

use helios_util as util;

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

    fn inner(&self) -> &Docker {
        &self.0
    }

    /// Exposes methods to work with images.
    #[inline]
    pub fn image(&self) -> Image<'_> {
        Image::new(self)
    }

    /// Exposes methods to work with container
    #[inline]
    pub fn container(&self) -> Container<'_> {
        Container::new(self)
    }
}

#[derive(Debug, thiserror::Error)]
pub struct Error {
    context: Option<String>,
    source: ConnectionError,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(c) = &self.context {
            c.fmt(f)?;
            ": ".fmt(f)?;
        }
        self.source.fmt(f)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    #[inline]
    pub fn new(source: ConnectionError, context: Option<String>) -> Self {
        Self { source, context }
    }

    /// Returns a `ClientError` partial constructor with the given message as context.
    #[inline]
    pub fn with_context(msg: &'static str) -> impl (FnOnce(ConnectionError) -> Self) {
        move |source| Error {
            source,
            context: Some(msg.to_owned()),
        }
    }

    /// Assigns context to this error.
    #[inline]
    pub fn context(mut self, msg: String) -> Self {
        self.context = Some(msg);
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
    fn with_context<F>(self, f: F) -> Result<T>
    where
        F: FnOnce() -> String;

    /// Associates extra context with the [Error], if `self` is [Err].
    /// To provide a [String] as context, potentially with formatting, use
    /// [WithContext::with_context].
    #[inline]
    fn context(self, msg: &'static str) -> Result<T> {
        self.with_context(|| msg.to_owned())
    }
}

impl<T> WithContext<T> for Result<T> {
    #[inline]
    fn with_context<F>(self, f: F) -> Result<T>
    where
        F: FnOnce() -> String,
    {
        self.map_err(|err| err.context(f()))
    }
}
