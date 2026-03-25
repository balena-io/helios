use std::fmt::{self};

use bollard::Docker;

pub use bollard::auth::DockerCredentials as Credentials;
pub use bollard::errors::Error as ConnectionError;

mod image;
pub use image::{Image, ImageConfig, LocalImage};

mod registry;
pub use registry::RegistryAuth;

mod container;
pub use container::{Container, ContainerConfig, ContainerState, ContainerStatus, LocalContainer};

mod datetime;
pub use datetime::DateTime;

mod network;
pub use network::{
    LocalNetwork, Network, NetworkConfig, NetworkDriver, NetworkIpamConfig, NetworkIpamDriver,
    NetworkIpamPoolConfig,
};

mod volume;
pub use volume::{LocalVolume, Volume, VolumeConfig, VolumeDriver};

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

        Ok(Self(inner))
    }

    fn inner(&self) -> &Docker {
        &self.0
    }

    /// Returns version information about the connected daemon.
    ///
    /// The returned components can be used to identify the engine type (e.g. "Podman Engine")
    /// and its version.
    pub async fn version(&self) -> Result<Version> {
        let version = self
            .0
            .version()
            .await
            .map_err(Error::with_context("failed to get server version"))?;

        let components = version
            .components
            .unwrap_or_default()
            .into_iter()
            .map(|c| Component {
                name: c.name,
                version: c.version,
            })
            .collect();

        Ok(Version { components })
    }

    /// Exposes methods to work with images.
    #[inline]
    pub fn image(&self) -> Image<'_> {
        Image::new(self)
    }

    /// Exposes methods to work with non-namespaced containers
    #[inline]
    pub fn non_namepaced_container(&self) -> Container<'_, NoNamespace> {
        Container::new(self)
    }

    /// Exposes methods to work with container
    #[inline]
    pub fn container(&self) -> Container<'_, LocalNamespace> {
        Container::new(self)
    }

    /// Exposes methods to work with networks
    #[inline]
    pub fn network(&self) -> Network<'_> {
        Network::new(self)
    }

    /// Exposes methods to work with volumes
    #[inline]
    pub fn volume(&self) -> Volume<'_> {
        Volume::new(self)
    }
}

/// Version information about the connected daemon.
#[derive(Debug, Clone)]
pub struct Version {
    pub components: Vec<Component>,
}

/// A component reported by the daemon's version endpoint.
#[derive(Debug, Clone)]
pub struct Component {
    pub name: String,
    pub version: String,
}

#[doc(hidden)]
type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum ErrorKind {
    #[error(transparent)]
    Connection(#[from] ConnectionError),

    #[error(transparent)]
    Other(#[from] BoxError),
}

#[derive(Debug, thiserror::Error)]
pub struct Error {
    context: Option<String>,
    source: ErrorKind,
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
    fn new(source: ErrorKind, context: Option<String>) -> Self {
        Self { source, context }
    }

    /// Create an ClientError::Other from an input error
    pub fn other<E: Into<BoxError>>(error: E) -> Self {
        Self {
            source: ErrorKind::Other(error.into()),
            context: None,
        }
    }

    /// Returns a `ClientError` partial constructor with the given message as context.
    #[inline]
    pub fn with_context(msg: &'static str) -> impl FnOnce(ConnectionError) -> Self {
        move |source| Error {
            source: source.into(),
            context: Some(msg.to_owned()),
        }
    }

    /// Assigns context to this error.
    #[inline]
    pub fn context(mut self, msg: String) -> Self {
        self.context = Some(msg);
        self
    }

    /// Returns the error kind
    pub fn kind(&self) -> &ErrorKind {
        &self.source
    }
}

impl From<ConnectionError> for Error {
    #[inline]
    fn from(value: ConnectionError) -> Self {
        Self::new(value.into(), None)
    }
}

impl From<BoxError> for Error {
    #[inline]
    fn from(value: BoxError) -> Self {
        Self::new(value.into(), None)
    }
}

impl From<&str> for Error {
    #[inline]
    fn from(value: &str) -> Self {
        Error::other(value)
    }
}

impl From<String> for Error {
    #[inline]
    fn from(value: String) -> Self {
        Error::other(value)
    }
}

impl From<chrono::ParseError> for Error {
    #[inline]
    fn from(value: chrono::ParseError) -> Self {
        Error::other(value)
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

pub trait Namespace: Sized {
    /// Convert the given entity name to the namespaced identifier
    fn to_identifier(&self, entity: &str) -> String;

    /// Extract the namespace from the identifier and entity name
    fn from_identifier(id: &str, entity: &str) -> Option<Self>;
}

pub struct NoNamespace;

impl Namespace for NoNamespace {
    fn to_identifier(&self, name: &str) -> String {
        name.to_owned()
    }

    fn from_identifier(_: &str, _: &str) -> Option<Self> {
        Some(NoNamespace)
    }
}

impl From<()> for NoNamespace {
    fn from(_: ()) -> Self {
        NoNamespace
    }
}

// The default namespace
#[derive(Clone, Debug)]
pub struct LocalNamespace(String);

impl LocalNamespace {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl Namespace for LocalNamespace {
    fn to_identifier(&self, entity: &str) -> String {
        format!("{entity}_{}", self.0)
    }

    fn from_identifier(id: &str, entity: &str) -> Option<Self> {
        let namespace = id
            .strip_prefix(&format!("{entity}_"))
            // if the remainder has underscores, assume the last
            // component to be the namespace
            .and_then(|suffix| suffix.rsplit('_').next())
            // ignore the value if empty
            .filter(|part| !part.is_empty());

        namespace.map(|n| Self(n.to_owned()))
    }
}

impl From<String> for LocalNamespace {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for LocalNamespace {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}
