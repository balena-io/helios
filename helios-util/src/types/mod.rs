use std::ops::Deref;
use std::path::PathBuf;

mod api_key;
mod environment;
mod image_uri;
mod operating_system;
mod uuid;

pub use api_key::ApiKey;
pub use environment::{Environment, Value};
pub use image_uri::{ImageUri, InvalidImageUriError};
pub use operating_system::OperatingSystem;
pub use uuid::Uuid;

// Just an alias for more descriptive code
pub type DeviceType = String;

/// Directory on the host filesystem used by helios at runtime when running
/// inside a container
#[derive(Debug, Clone)]
pub struct HostRuntimeDir(pub PathBuf);

impl Deref for HostRuntimeDir {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
