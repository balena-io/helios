use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::Path;
use std::{io, path::PathBuf};
use thiserror::Error;
use tokio::fs;
use tracing::trace;

use crate::fs::safe_write_all;

#[derive(Debug, Error)]
pub enum ReadWriteError {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}

async fn ensure_parent(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        let res = fs::create_dir_all(dir).await;
        // create_dir will error if the directory already exists.
        // check if that is the reason it failed.
        if res.is_err() && !std::fs::exists(dir).unwrap_or(false) {
            return res;
        }
    }
    Ok(())
}

fn state_dir() -> PathBuf {
    let dir = if let Some(state_dir) = dirs::state_dir() {
        state_dir
    } else {
        // Fallback to home directory if state dir is not available
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local")
            .join("state")
    };
    dir.join(env!("HELIOS_PKG_NAME"))
}

fn with_state_dir<P: AsRef<Path>>(path: P) -> PathBuf {
    let path = path
        .as_ref()
        .strip_prefix("/")
        .expect("path should start with '/'");
    state_dir().join(path)
}

/// Helper function to store local state
///
/// Example:
/// ```rust,ignore
/// util::state::store("/apps/app-uuid/name", "my-app-name").await?;
/// ```
pub async fn store<P: AsRef<Path>, V: Serialize>(path: P, value: &V) -> Result<(), ReadWriteError> {
    let local_path = with_state_dir(path);
    trace!("storing local state: {}", local_path.display());
    let buf = serde_json::to_vec(&value)?;

    // make sure that the file directory exists
    ensure_parent(&local_path).await?;

    tokio::task::spawn_blocking(move || safe_write_all(local_path, &buf))
        .await
        .expect("safe_write_all should not panic")?;
    Ok(())
}

/// Helper function to store local state
///
/// Example:
/// ```rust,ignore
/// util::state::store_with_name("/apps/app-uuid", "name", "my-app-name").await?;
/// ```
pub async fn store_with_name<P: AsRef<Path>, V: Serialize>(
    path: P,
    name: &str,
    value: &V,
) -> Result<(), ReadWriteError> {
    let path = path.as_ref().join(name);
    store(path, value).await?;
    Ok(())
}

pub async fn read<P: AsRef<Path>, V: DeserializeOwned>(
    path: P,
) -> Result<Option<V>, ReadWriteError> {
    let local_path = with_state_dir(path);
    trace!("read local state {}", local_path.display());

    match fs::read_to_string(&local_path).await {
        Ok(contents) => {
            // We have a previously saved config
            let config = serde_json::from_str::<V>(&contents)?;
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

pub async fn read_all<P: AsRef<Path>, K: DeserializeOwned>(
    path: P,
) -> Result<Vec<K>, ReadWriteError> {
    let local_path = with_state_dir(path);
    let mut dir_entries = match fs::read_dir(local_path).await {
        Ok(entries) => entries,
        Err(err) => match err.kind() {
            io::ErrorKind::NotFound => return Ok(Vec::new()),
            _ => return Err(err.into()),
        },
    };
    let mut keys = Vec::new();
    while let Some(entry) = dir_entries.next_entry().await? {
        if let Ok(name) = entry.file_name().into_string() {
            let value = serde_json::Value::String(name);
            let k: K = serde_json::from_value(value)?;
            // ignore non-unicode names
            keys.push(k)
        }
    }

    Ok(keys)
}

pub async fn remove<P: AsRef<Path>>(path: P) -> Result<(), io::Error> {
    let local_path = with_state_dir(path);
    trace!("removing local state {}", local_path.display());
    fs::remove_file(local_path).await
}

pub async fn remove_dir<P: AsRef<Path>>(path: P) -> Result<(), io::Error> {
    let local_path = with_state_dir(path);
    trace!("removing local state {}", local_path.display());
    fs::remove_dir(local_path).await
}
