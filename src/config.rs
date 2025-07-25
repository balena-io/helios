use std::fs;
use std::io;
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::Serialize;
use thiserror::Error;
use tracing::debug;

use crate::util::fs::safe_write_all;

pub trait StaticConfig
where
    Self: Serialize,
    Self: DeserializeOwned,
{
    fn name() -> String;
}

#[derive(Debug, Error)]
pub enum LoadConfigError {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Derialization(#[from] serde_json::Error),
}

pub fn load<C>() -> Result<Option<C>, LoadConfigError>
where
    C: StaticConfig,
{
    let name = C::name();
    let filename = format!("{name}.json");
    let path = config_dir().join(filename);

    match fs::read_to_string(&path) {
        Ok(contents) => {
            // We have a previously saved config
            let config = serde_json::from_str::<C>(&contents)?;
            debug!("Loaded config {name} from {}", path.display());
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
pub enum SaveConfigError {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}

pub async fn save<C>(config: &C) -> Result<(), SaveConfigError>
where
    C: StaticConfig,
{
    let name = C::name();
    let filename = format!("{name}.json");
    let path = config_dir().join(filename);
    let buf = serde_json::to_vec(&config)?;

    debug!("Writing config {name} to {}", path.display());
    safe_write_all(path, &buf)?;

    Ok(())
}

pub fn config_dir() -> PathBuf {
    if let Some(config_dir) = dirs::config_dir() {
        config_dir.join(env!("CARGO_PKG_NAME"))
    } else {
        // Fallback to home directory if config dir is not available
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config")
            .join(env!("CARGO_PKG_NAME"))
    }
}

pub fn ensure_config_dir() -> io::Result<()> {
    let dir = config_dir();
    let res = fs::create_dir_all(dir.as_path());
    // create_dir will error if the directory already exists.
    // check if that is the reason it failed.
    if res.is_err() && fs::exists(dir.as_path()).unwrap_or(false) {
        Ok(())
    } else {
        res
    }
}
