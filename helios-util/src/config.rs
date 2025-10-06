use std::io;
use std::path::PathBuf;

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::fs;
use tracing::trace;

use crate::fs::safe_write_all;

pub trait StoredConfig
where
    Self: Serialize,
    Self: DeserializeOwned,
{
    fn kind() -> String;

    /// This config's preferred file name exluding the extension.
    fn default_name() -> String {
        Self::kind()
    }
}

#[derive(Debug, Error)]
pub enum GetConfigError {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Derialization(#[from] serde_json::Error),
}

pub async fn get<C: StoredConfig>() -> Result<Option<C>, GetConfigError> {
    let name = C::default_name();
    get_with_name(&name).await
}

/// Load and decode a config stored with `name`.
///
/// `name` is the desired file name in config dir, without an extension.
/// All stored configs automatically get a `.json` extension.
pub async fn get_with_name<C: StoredConfig>(name: &str) -> Result<Option<C>, GetConfigError> {
    let filename = format!("{name}.json");
    let path = config_dir().join(filename);

    trace!("getting {} config from {}", C::kind(), path.display());
    match fs::read_to_string(&path).await {
        Ok(contents) => {
            // We have a previously saved config
            let config = serde_json::from_str::<C>(&contents)?;
            Ok(Some(config))
        }
        Err(err) => match err.kind() {
            // We don't have a saved config
            io::ErrorKind::NotFound => Ok(None),

            // We have a config but failed to load it
            _ => Err(err.into()),
        },
    }
}

#[derive(Debug, Error)]
pub enum StoreConfigError {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}

pub async fn store<C: StoredConfig>(config: &C) -> Result<(), StoreConfigError> {
    let name = C::default_name();
    store_with_name(config, &name).await
}

/// Encode and store a config with `name`.
///
/// `name` is the desired file name in config dir, without an extension.
/// All stored configs automatically get a `.json` extension.
pub async fn store_with_name<C: StoredConfig>(
    config: &C,
    name: &str,
) -> Result<(), StoreConfigError> {
    let filename = format!("{name}.json");
    let path = config_dir().join(filename);
    let buf = serde_json::to_vec(&config)?;

    trace!("storing {} config to {}", C::kind(), path.display());
    safe_write_all(path, &buf).await?;

    Ok(())
}

#[allow(unused)]
pub async fn remove<C: StoredConfig>() -> Result<(), io::Error> {
    let name = C::default_name();
    remove_with_name::<C>(&name).await
}

/// Delete a config stored with `name`.
///
/// `name` is the desired file name in config dir, without an extension.
/// All stored configs automatically get a `.json` extension.
pub async fn remove_with_name<C: StoredConfig>(name: &str) -> Result<(), io::Error> {
    let filename = format!("{name}.json");
    let path = config_dir().join(filename);
    trace!("removing {} config from {}", C::kind(), path.display());
    fs::remove_file(path).await
}

pub async fn ensure_config_dir() -> io::Result<()> {
    let dir = config_dir();
    let res = fs::create_dir_all(dir.as_path()).await;
    // create_dir will error if the directory already exists.
    // check if that is the reason it failed.
    if res.is_err() && fs::try_exists(dir.as_path()).await.unwrap_or(false) {
        Ok(())
    } else {
        res
    }
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
