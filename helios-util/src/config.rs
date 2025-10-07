use std::path::PathBuf;
use std::sync::LazyLock;

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tracing::debug;

use crate::store::{Store, StoreError};

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

#[derive(Debug, Error)]
#[error(transparent)]
pub struct GetConfigError(#[from] StoreError);

static CONFIG_STORE: LazyLock<Store> = LazyLock::new(|| Store::new(config_dir()));

pub async fn get<C: StoredConfig>() -> Result<Option<C>, GetConfigError> {
    let name = C::default_name();
    get_with_name(name).await
}

/// Load and decode a config stored with `name`.
///
/// `name` is the desired file name in config dir, without an extension.
/// All stored configs automatically get a `.json` extension.
pub async fn get_with_name<C: StoredConfig>(name: &str) -> Result<Option<C>, GetConfigError> {
    debug!("reading {} config", C::kind());
    let value = CONFIG_STORE.read("/", name).await?;
    Ok(value)
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct StoreConfigError(#[from] StoreError);

pub async fn store<C: StoredConfig>(config: &C) -> Result<(), StoreConfigError> {
    debug!("storing {} config", C::kind());
    let name = C::default_name();
    store_with_name(config, name).await
}

/// Encode and store a config with `name`.
///
/// `name` is the desired file name in config dir, without an extension.
/// All stored configs automatically get a `.json` extension.
pub async fn store_with_name<C: StoredConfig>(
    config: &C,
    name: &str,
) -> Result<(), StoreConfigError> {
    CONFIG_STORE.store("/", name, config).await?;

    Ok(())
}

/// Delete a config stored with `name`.
///
/// `name` is the desired file name in config dir, without an extension.
/// All stored configs automatically get a `.json` extension.
pub async fn remove_with_name<C: StoredConfig>(name: &str) -> Result<(), StoreConfigError> {
    debug!("removing {} config", C::kind());
    CONFIG_STORE.delete("/", name).await?;
    Ok(())
}

fn config_dir() -> PathBuf {
    let dir = if let Some(config_dir) = dirs::config_dir() {
        config_dir
    } else {
        // Fallback to home directory if config dir is not available
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config")
    };
    dir.join(env!("HELIOS_PKG_NAME"))
}
