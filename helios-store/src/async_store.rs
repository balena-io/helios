//! Async wrappers for document storage.
//!
//! This module provides [`AsyncDocumentStore`] and [`AsyncView`], which wrap
//! the synchronous [`DocumentStore`] and [`View`] types to provide async-compatible
//! methods that don't block the async runtime.
//!
//! All filesystem operations are executed on a blocking thread pool via
//! [`tokio::task::spawn_blocking`].

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::db::Digest;
use crate::document::DocumentStore;
use crate::util::fs::run_async;
use crate::{Error, Result};

/// An async wrapper around [`DocumentStore`].
///
/// This type provides async versions of all `DocumentStore` methods by executing
/// the underlying synchronous operations on a blocking thread pool.
///
/// # Example
///
/// ```ignore
/// use helios_store::AsyncDocumentStore;
/// use serde_json::json;
///
/// let store = AsyncDocumentStore::create_with_root("/path/to/store").await?;
/// store.put("config.json".into(), json!({ "version": "1.0" })).await?;
/// ```
#[derive(Debug, Clone)]
pub struct AsyncDocumentStore {
    inner: Arc<DocumentStore>,
}

impl AsyncDocumentStore {
    /// Wraps an existing [`DocumentStore`] in an async wrapper.
    pub fn new(store: DocumentStore) -> Self {
        Self {
            inner: Arc::new(store),
        }
    }

    /// Opens an existing document store at the specified path.
    ///
    /// See [`DocumentStore::with_root`] for details.
    pub fn with_root(root: PathBuf) -> io::Result<Self> {
        DocumentStore::with_root(&root).map(Self::new)
    }

    /// Opens a document store at the specified path, creating it if it doesn't exist.
    ///
    /// See [`DocumentStore::create_with_root`] for details.
    pub async fn create_with_root(root: PathBuf) -> Result<Self> {
        run_async(move || {
            DocumentStore::create_with_root(&root)
                .map(Self::new)
                .map_err(Error::from)
        })
        .await
    }

    /// Returns a reference to the underlying [`DocumentStore`].
    pub fn inner(&self) -> &DocumentStore {
        &self.inner
    }

    /// Returns an [`AsyncView`] into the root of the store.
    pub fn as_view(&self) -> AsyncView {
        AsyncView::new(Arc::clone(&self.inner), None)
    }

    /// Checks for the existence of a document without locking it.
    ///
    /// See [`DocumentStore::exists`] for caveats.
    pub async fn exists(&self, path: PathBuf) -> Result<bool> {
        let store = Arc::clone(&self.inner);
        run_async(move || store.exists(&path)).await
    }

    /// Deletes the document at the specified path.
    ///
    /// See [`DocumentStore::delete`] for details.
    pub async fn delete(&self, path: PathBuf) -> Result<bool> {
        let store = Arc::clone(&self.inner);
        run_async(move || store.delete(&path)).await
    }

    /// Lists all documents matching the specified filter.
    ///
    /// Returns a `Vec` of paths (unlike the sync version which returns an iterator).
    /// See [`DocumentStore::list`] for filter syntax.
    pub async fn list(&self, pattern: Option<String>) -> Result<Vec<PathBuf>> {
        let store = Arc::clone(&self.inner);
        run_async(move || {
            store
                .list(pattern.as_deref())?
                .collect::<io::Result<Vec<_>>>()
                .map_err(Error::from)
        })
        .await
    }

    /// JSON-encodes and stores the given object at the specified path.
    ///
    /// See [`DocumentStore::put`] for details.
    pub async fn put<U>(&self, path: PathBuf, obj: U) -> Result<(Digest, u64)>
    where
        U: Serialize + Send + 'static,
    {
        let store = Arc::clone(&self.inner);
        run_async(move || store.put(&path, &obj)).await
    }

    /// Loads and JSON-decodes the document at the specified path.
    ///
    /// See [`DocumentStore::get`] for details.
    pub async fn get<U>(&self, path: PathBuf) -> Result<Option<U>>
    where
        U: DeserializeOwned + Send + 'static,
    {
        let store = Arc::clone(&self.inner);
        run_async(move || store.get(&path)).await
    }
}

impl From<DocumentStore> for AsyncDocumentStore {
    fn from(store: DocumentStore) -> Self {
        Self::new(store)
    }
}

/// An async view into a sub-path of a [`AsyncDocumentStore`].
///
/// See [`crate::View`] for the synchronous equivalent.
#[derive(Debug, Clone)]
pub struct AsyncView {
    store: Arc<DocumentStore>,
    path: Option<PathBuf>,
}

impl AsyncView {
    fn new(store: Arc<DocumentStore>, path: Option<PathBuf>) -> Self {
        Self { store, path }
    }

    fn full_path(&self, path: &PathBuf) -> PathBuf {
        self.path
            .as_ref()
            .map(|p| p.join(path))
            .unwrap_or_else(|| path.clone())
    }

    /// Creates a new `AsyncView` at the given path relative to this view's root.
    pub fn join(&self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let full = self.full_path(&path);
        Self::new(Arc::clone(&self.store), Some(full))
    }

    /// Checks for the existence of a document.
    pub async fn exists(&self, path: PathBuf) -> Result<bool> {
        let store = Arc::clone(&self.store);
        let full = self.full_path(&path);
        run_async(move || store.exists(&full)).await
    }

    /// Deletes the document at the specified path.
    pub async fn delete(&self, path: PathBuf) -> Result<bool> {
        let store = Arc::clone(&self.store);
        let full = self.full_path(&path);
        run_async(move || store.delete(&full)).await
    }

    /// Lists all documents matching the specified filter.
    pub async fn list(&self, pattern: Option<String>) -> Result<Vec<PathBuf>> {
        let store = Arc::clone(&self.store);
        let base = self.path.clone();
        run_async(move || {
            store
                .list_at(base.as_deref(), pattern.as_deref())?
                .collect::<io::Result<Vec<_>>>()
                .map_err(Error::from)
        })
        .await
    }

    /// JSON-encodes and stores the given object at the specified path.
    pub async fn put<U>(&self, path: PathBuf, obj: U) -> Result<(Digest, u64)>
    where
        U: Serialize + Send + 'static,
    {
        let store = Arc::clone(&self.store);
        let full = self.full_path(&path);
        run_async(move || store.put(&full, &obj)).await
    }

    /// Loads and JSON-decodes the document at the specified path.
    pub async fn get<U>(&self, path: PathBuf) -> Result<Option<U>>
    where
        U: DeserializeOwned + Send + 'static,
    {
        let store = Arc::clone(&self.store);
        let full = self.full_path(&path);
        run_async(move || store.get(&full)).await
    }

    /// Reads the raw bytes of a document.
    pub async fn read(&self, path: PathBuf) -> Result<Option<Vec<u8>>> {
        let store = Arc::clone(&self.store);
        let full = self.full_path(&path);
        run_async(move || {
            let mut file = match store.open(&full) {
                Ok(f) => f,
                Err(Error::NotFound { .. }) => return Ok(None),
                Err(e) => return Err(e),
            };
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut buf)?;
            Ok(Some(buf))
        })
        .await
    }

    /// Writes raw bytes to a document.
    pub async fn write(&self, path: PathBuf, data: Vec<u8>) -> Result<(Digest, u64)> {
        let store = Arc::clone(&self.store);
        let full = self.full_path(&path);
        run_async(move || {
            let staged = store.stage()?;
            let doc = staged.write_all(std::io::Cursor::new(data))?;
            doc.commit(&full).map_err(|e| e.source)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde::{Deserialize, Serialize};
    use tempfile::tempdir;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct TestData {
        name: String,
        value: i32,
    }

    #[tokio::test]
    async fn test_async_put_get() {
        let dir = tempdir().unwrap();
        let store = AsyncDocumentStore::create_with_root(dir.path().to_path_buf())
            .await
            .unwrap();

        let data = TestData {
            name: "test".to_string(),
            value: 42,
        };

        store.put("data.json".into(), data.clone()).await.unwrap();

        let retrieved: Option<TestData> = store.get("data.json".into()).await.unwrap();
        assert_eq!(retrieved, Some(data));
    }

    #[tokio::test]
    async fn test_async_exists_delete() {
        let dir = tempdir().unwrap();
        let store = AsyncDocumentStore::create_with_root(dir.path().to_path_buf())
            .await
            .unwrap();

        store.put("doc.json".into(), "test").await.unwrap();
        assert!(store.exists("doc.json".into()).await.unwrap());

        store.delete("doc.json".into()).await.unwrap();
        assert!(!store.exists("doc.json".into()).await.unwrap());
    }

    #[tokio::test]
    async fn test_async_list() {
        let dir = tempdir().unwrap();
        let store = AsyncDocumentStore::create_with_root(dir.path().to_path_buf())
            .await
            .unwrap();

        store.put("a/1.json".into(), "1").await.unwrap();
        store.put("a/2.json".into(), "2").await.unwrap();
        store.put("b/1.json".into(), "3").await.unwrap();

        let mut all = store.list(None).await.unwrap();
        all.sort();
        assert_eq!(
            all,
            vec![
                PathBuf::from("a/1.json"),
                PathBuf::from("a/2.json"),
                PathBuf::from("b/1.json"),
            ]
        );

        let mut filtered = store.list(Some("a/*".to_string())).await.unwrap();
        filtered.sort();
        assert_eq!(
            filtered,
            vec![PathBuf::from("a/1.json"), PathBuf::from("a/2.json"),]
        );
    }

    #[tokio::test]
    async fn test_async_view() {
        let dir = tempdir().unwrap();
        let store = AsyncDocumentStore::create_with_root(dir.path().to_path_buf())
            .await
            .unwrap();

        let view = store.as_view().join("myapp");

        view.put("config.json".into(), "config").await.unwrap();

        assert!(view.exists("config.json".into()).await.unwrap());
        assert!(store.exists("myapp/config.json".into()).await.unwrap());

        let items = view.list(None).await.unwrap();
        assert_eq!(items, vec![PathBuf::from("config.json")]);
    }
}
