use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::fs;
use tracing::trace;

use super::fs::safe_write_all;

/// A filesystem backed store
///
/// This is a very simple mechanism for persisting data on disk.
/// It supports atomic writes but no concurrency. Use it carefully and sparingly
#[derive(Clone)]
pub struct Store {
    root: PathBuf,
    // NOTE: we might want to restrict concurrent writes at some point with
    // something like locks: Arc<DashMap<PathBuf, Arc<RwLock<()>>>>
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Serialization(#[from] serde_json::Error),

    #[error("path must not have any `..` segments")]
    InvalidPath,
}

impl Store {
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    // Constructs a full path for a given collection path and document name.
    fn with_root(&self, path: &Path) -> Result<PathBuf, StoreError> {
        let doc_path = path.strip_prefix("/").unwrap_or(path);

        // Reject any path with ".." or other non-normal components
        for component in doc_path.components() {
            if !matches!(component, std::path::Component::Normal(_)) {
                return Err(StoreError::InvalidPath);
            }
        }
        Ok(self.root.join(doc_path))
    }

    /// Create or update a document at the specified location and with the given
    /// key
    ///
    /// Note that while writes are atomic, concurrent usage of a store may result in
    /// data-loss. Same with two different stores on the same base path.
    pub async fn write<P: AsRef<Path>, V: Serialize>(
        &self,
        path: P,
        key: &str,
        value: &V,
    ) -> Result<(), StoreError> {
        let parent = self.with_root(path.as_ref())?;

        // ensure the parent exists, this will fail if the parent
        // exists but is not a directory
        fs::create_dir_all(parent.as_path()).await?;

        let full_path = parent.join(key).with_extension("json");
        let buf = serde_json::to_vec(&value)?;
        trace!("writing {}", full_path.display());
        safe_write_all(full_path, &buf).await?;
        Ok(())
    }

    /// Read a document from the specified location
    pub async fn read<P: AsRef<Path>, V: DeserializeOwned>(
        &self,
        path: P,
        key: &str,
    ) -> Result<Option<V>, StoreError> {
        let full_path = self
            .with_root(path.as_ref())?
            .join(key)
            .with_extension("json");
        trace!("reading {}", full_path.display());

        match fs::read_to_string(&full_path).await {
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

    /// List all documents from the given location, this will fail if the
    /// location is a document.
    ///
    /// The generic type `K` must be deserializable from a JSON string value,
    /// as document names are returned as filenames. This works for `String`,
    /// newtypes like `struct UserId(String)`, or types with custom string
    /// deserialization.
    pub async fn list<P: AsRef<Path>, K: DeserializeOwned>(
        &self,
        path: P,
    ) -> Result<Vec<K>, StoreError> {
        let full_path = self.with_root(path.as_ref())?;
        let mut dir_entries = match fs::read_dir(full_path).await {
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

    /// Delete the document at the given location.
    ///
    /// This operation is idempotent - deleting a non-existent document succeeds.
    pub async fn delete<P: AsRef<Path>>(&self, path: P, name: &str) -> Result<(), StoreError> {
        let full_path = self
            .with_root(path.as_ref())?
            .join(name)
            .with_extension("json");
        trace!("removing {}", full_path.display());
        match fs::remove_file(full_path).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// Delete all documents at the given location.
    ///
    /// This operation is idempotent - deleting a non-existent path succeeds.
    pub async fn delete_all<P: AsRef<Path>>(&self, collection_path: P) -> Result<(), StoreError> {
        let full_path = self.with_root(collection_path.as_ref())?;
        match fs::remove_dir_all(full_path).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}
