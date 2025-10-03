mod api_key;
mod image_uri;
mod operating_system;
mod uuid;

pub use api_key::ApiKey;
pub use image_uri::{ImageUri, InvalidImageUriError};
pub use operating_system::OperatingSystem;
pub use uuid::Uuid;

// Just an alias for more descriptive code
pub type DeviceType = String;
