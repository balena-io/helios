use serde::Serialize;
use serde::de::DeserializeOwned;

pub trait StoredConfig
where
    Self: Serialize,
    Self: DeserializeOwned,
{
    fn kind() -> &'static str;

    /// This config's preferred file name exluding the extension.
    fn default_name() -> &'static str {
        Self::kind()
    }
}
