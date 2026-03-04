//! A store for documents, keyed by path.
//!
//! [`DocumentStore`] is suitable for configuration files and database-like resources.
//! It allows for loading and storing any kind of file, keyed by a relative path.
//!
//! The store ensures safe concurrent access by allowing either multiple readers or a
//! single writer across the entire store at any given time. It offers a blocking API
//! for all operations.
//!
//! # Example
//!
//! ```
//! use serde_json::json;
//! use tempfile::tempdir;
//! use helios_store::DocumentStore;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempdir()?;
//! let store = DocumentStore::with_root(dir.path()).await?;
//! store.put("config.json", &json!({ "version": "1.0" })).await?;
//! # Ok(())
//! # }
//! ```

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::SystemTime;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json as json;
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

use super::db::{Blob, FileDb, FileId, Hooks, StagedFile, StoredFile, WalkDirOptions};
use super::util::fs::run_async;
use super::view::View;

use crate::{Error, Result};

pub use super::db::CommitError;

/// The document store format version
///
/// This is a safeguard for future migrations
const VERSION: &str = "0";

#[derive(Debug, Clone, Copy)]
struct DocumentHooks;

impl Hooks for DocumentHooks {
    fn file_at_path(&self, base: &Path, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(base.join(VERSION))
            .ok()
            .map(|p| p.to_path_buf())
    }

    fn path_for_file(&self, id: &FileId) -> io::Result<PathBuf> {
        Ok(format!("{VERSION}/{id}").into())
    }

    fn file_list_filters(
        &self,
        base: Option<&Path>,
        pattern: Option<&str>,
    ) -> (Vec<String>, Vec<String>) {
        let mut parts = Vec::with_capacity(2);

        parts.push(VERSION);

        if let Some(base) = base {
            parts.push(base.as_os_str().to_str().unwrap());
        }

        // 0/{base}/{pattern | **/*}
        let include = parts
            .iter()
            .chain(&[pattern.unwrap_or("**/*")])
            .copied()
            .collect::<Vec<_>>()
            .join("/");

        (vec![include], vec![])
    }

    fn file_list_options(&self, base: Option<&Path>, opts: &mut WalkDirOptions) {
        opts.max_depth = 0; // /<version>/<abcdef...>
        opts.min_depth = 2; //          1     2 ...
        opts.min_depth += base.map(|p| p.components().count() as u8).unwrap_or(0);
    }

    fn dir_list_filters(&self, base: Option<&Path>) -> (Vec<String>, Vec<String>) {
        let mut parts = Vec::with_capacity(2);

        parts.push(VERSION);

        if let Some(base) = base {
            parts.push(base.as_os_str().to_str().unwrap());
        }

        // 0/{base}/*
        let include = parts
            .iter()
            .chain(&["*"])
            .copied()
            .collect::<Vec<_>>()
            .join("/");

        (vec![include], vec![])
    }

    fn dir_list_options(&self, base: Option<&Path>, opts: &mut WalkDirOptions) {
        // Depth is relative to `contents/`. The layout is:
        //   contents/0/{base...}/X  where X is the immediate subdir we want.
        // 1 for the version component, 1 for X itself, plus the depth of base.
        let depth = 2u8 + base.map(|p| p.components().count() as u8).unwrap_or(0);
        opts.min_depth = depth;
        opts.max_depth = depth;
    }
}

/// A store for documents, keyed by path.
///
/// See module documentation for more info.

#[derive(Debug, Clone)]
pub struct DocumentStore {
    db: Arc<FileDb<DocumentHooks>>,
}

impl DocumentStore {
    /// Opens an existing document store at the specified path.
    ///
    /// It will create the store if it does not exist
    ///
    /// Returns an [`io::Error`] if the root does not exist
    pub async fn with_root(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        let db = run_async(move || FileDb::open(DocumentHooks, &root)).await?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Returns a [`View`] into the root of the store.
    ///
    /// A view allows operating on a sub-path within the store.
    pub fn as_view(&self) -> View<'_> {
        self.into()
    }

    /// Opens the document at the specified path for reading.
    ///
    /// Returns a [`StoredDocument`] which implements [`AsyncRead`] and [`io::Seek`].
    pub async fn open(&self, path: impl AsRef<Path>) -> Result<StoredDocument> {
        let ctx = Error::with_path(path.as_ref());
        let path = path.as_ref().to_path_buf();
        let db = Arc::clone(&self.db);
        run_async(move || db.open_file(&path))
            .await
            .map(Into::into)
            .map_err(&ctx)
    }

    /// Creates a new temporary file for writing data into the store.
    ///
    /// This returns a [`StagedDocument`] which can be written to. Once written,
    /// the resulting [`Document`] can be committed to a specific path in the store.
    pub async fn stage(&self) -> Result<StagedDocument> {
        let db = Arc::clone(&self.db);
        let doc = run_async(move || db.new_file()).await.map(Into::into)?;
        Ok(doc)
    }

    /// Checks for the existence of a document without locking it.
    ///
    /// **Warning:** This method is prone to time-of-check-to-time-of-use (TOCTOU)
    /// race conditions. Another process could delete or modify the document between
    /// this check and a subsequent operation. Prefer [`DocumentStore::open`], which
    /// acquires a shared lock and prevents such issues.
    ///
    /// This function is appropriate only when you can tolerate the document's state
    /// changing after the check, or when you need to check for a file that might be
    /// exclusively locked (as `open` would block in that case).
    ///
    /// # Example
    ///
    /// ```
    /// use tempfile::tempdir;
    /// use helios_store::DocumentStore;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let dir = tempdir().unwrap();
    /// let store = DocumentStore::with_root(dir.path()).await?;
    /// assert!(!store.exists("my-doc").await?);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn exists(&self, path: impl AsRef<Path>) -> Result<bool> {
        let ctx = Error::with_path(path.as_ref());
        let path = path.as_ref().to_path_buf();
        let db = Arc::clone(&self.db);
        run_async(move || db.has_file(&path)).await.map_err(&ctx)
    }

    /// Deletes the document at the specified path.
    ///
    /// This operation is idempotent; deleting a non-existent document will succeed.
    /// Returns whether the file was deleted.
    pub async fn delete(&self, path: impl AsRef<Path>) -> Result<bool> {
        let ctx = Error::with_path(path.as_ref());
        let path = path.as_ref().to_path_buf();
        let db = Arc::clone(&self.db);

        run_async(move || db.delete_file(&path)).await.map_err(&ctx)
    }

    /// Loads and JSON-decodes the document at the specified path.
    ///
    /// This is a convenience method that reads a document from the store and
    /// deserializes it from JSON into an instance of `U`.
    pub async fn get<U>(&self, path: impl AsRef<Path>) -> Result<Option<U>>
    where
        U: DeserializeOwned + Send + 'static,
    {
        let ctx = Error::with_path(path.as_ref());
        let path = path.as_ref().to_path_buf();
        let db = Arc::clone(&self.db);

        run_async(move || {
            let file = match db.open_file(&path) {
                Ok(fd) => fd,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(err),
            };

            let obj: U = json::from_reader(file)?;

            Ok(Some(obj))
        })
        .await
        .map_err(&ctx)
    }

    /// Lists all documents matching the specified filter.
    ///
    /// The filter is a glob pattern. See [`helios_store::util::glob`] for syntax.
    /// If `None`, all documents are listed.
    ///
    /// # Example
    ///
    /// ```
    /// use tempfile::tempdir;
    /// use std::path::PathBuf;
    /// use helios_store::DocumentStore;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let dir = tempdir()?;
    /// let store = DocumentStore::with_root(dir.path()).await?;
    /// store.put("a/1", &"").await?;
    /// store.put("b/2", &"").await;
    ///
    /// let paths = store.list(Some("a/*")).await?;
    /// assert_eq!(paths, vec![PathBuf::from("a/1")]);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn list(&self, pattern: Option<&str>) -> Result<Vec<PathBuf>> {
        self.list_at(None, pattern).await
    }

    pub(crate) async fn list_at(
        &self,
        base: Option<&Path>,
        pattern: Option<&str>,
    ) -> Result<Vec<PathBuf>> {
        let db = Arc::clone(&self.db);
        let base = base.map(|b| b.to_path_buf());
        let pattern = pattern.map(|s| s.to_owned());
        let paths = run_async(move || {
            db.list_files(base.as_deref(), pattern.as_deref())
                // keep files we have access to
                .map(|iter| iter.filter_map(|r| r.ok()).collect::<Vec<_>>())
        })
        .await?;
        Ok(paths)
    }

    /// Deletes all documents matching specified filter
    ///
    /// This is a convenience method combining operations in [`DocumentStore::list`] and
    /// [`DocumentStore::delete`].
    ///
    /// The filter is a glob pattern. See [`helios_store::util::glob`] for syntax.
    ///
    /// The filter is required to avoid accidental deletion of all files.
    ///
    /// Returns a list of all successfully deleted files.
    ///
    /// # Example
    ///
    /// ```
    /// use tempfile::tempdir;
    /// use std::path::PathBuf;
    /// use helios_store::DocumentStore;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let dir = tempdir()?;
    /// let store = DocumentStore::with_root(dir.path()).await?;
    /// store.put("a/1", &"").await?;
    /// store.put("b/2", &"").await;
    ///
    /// let paths = store.delete_all("a/*").await?;
    /// assert_eq!(paths, vec![PathBuf::from("a/1")]);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn delete_all(&self, pattern: &str) -> Result<Vec<PathBuf>> {
        self.delete_all_at(None, pattern).await
    }

    pub(crate) async fn delete_all_at(
        &self,
        base: Option<&Path>,
        pattern: &str,
    ) -> Result<Vec<PathBuf>> {
        let db = Arc::clone(&self.db);
        let base = base.map(|b| b.to_path_buf());
        let pattern = pattern.to_owned();

        let paths = run_async(move || {
            let mut deleted = Vec::new();
            for file in db
                .list_files(base.as_deref(), Some(pattern.as_ref()))?
                .flatten()
            {
                let full = base
                    .as_deref()
                    .map(|b| b.join(&file))
                    .unwrap_or(file.clone());
                if db.delete_file(&full)? {
                    deleted.push(file);
                }
            }

            Ok(deleted)
        })
        .await?;
        Ok(paths)
    }

    /// Lists all immediate subdirectories under the given path.
    pub async fn keys(&self, path: impl AsRef<Path>) -> Result<Vec<String>> {
        self.keys_at(Some(path.as_ref())).await
    }

    pub(crate) async fn keys_at(&self, base: Option<&Path>) -> Result<Vec<String>> {
        let db = Arc::clone(&self.db);
        let base = base.map(|b| b.to_path_buf());
        let dirs = run_async(move || {
            db.list_dirs(base.as_deref()).map(|iter| {
                // keep only directories we have access to and that are valid unicode names
                iter.filter_map(|r| r.ok()?.into_os_string().into_string().ok())
                    .collect::<Vec<_>>()
            })
        })
        .await?;
        Ok(dirs)
    }

    /// Removes unused staging files and empty directories from the store.
    ///
    /// Staging files that are currently in use (held by an exclusive lock) are preserved.
    pub async fn gc(&self) -> Result<()> {
        let db = Arc::clone(&self.db);
        run_async(move || {
            db.gc();
            Ok(())
        })
        .await
        .map_err(Into::into)
    }

    /// JSON-encodes and stores the given object at the specified path.
    ///
    /// This is a convenience method that serializes a `Serialize`-able object to JSON
    /// and writes it to the given path. It returns the digest and size of the stored data.
    pub async fn put<U>(&self, path: impl AsRef<Path>, obj: &U) -> Result<u64>
    where
        U: Serialize + ?Sized,
    {
        let ctx = Error::with_path(path.as_ref());
        let path = path.as_ref().to_path_buf();
        let db = Arc::clone(&self.db);
        let bytes = json::to_vec_pretty(obj)?;

        run_async(move || {
            let file = db.new_file()?;
            let blob = file.write_all(bytes.as_slice())?;

            match blob.commit(path.as_ref()) {
                Ok(size) => Ok(size),
                Err(err) => Err(err.source),
            }
        })
        .await
        .map_err(&ctx)
    }
}

/// A document in the store that has been opened for reading.
///
/// Holds a shared lock on the underlying file for its lifetime, preventing
/// concurrent writes while the document is open.
///
/// Implements [`AsyncRead`] and [`AsyncSeek`] for asynchronous I/O.
///
/// # Example
///
/// ```no_run
/// use tokio::io::AsyncReadExt;
/// use helios_store::DocumentStore;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// # let dir = tempfile::tempdir()?;
/// let store = DocumentStore::with_root(dir.path()).await?;
/// let mut doc = store.open("config.json").await?;
///
/// let mut contents = String::new();
/// doc.read_to_string(&mut contents).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct StoredDocument {
    inner: StoredFile,
}

impl From<StoredFile> for StoredDocument {
    fn from(value: StoredFile) -> Self {
        Self { inner: value }
    }
}

impl StoredDocument {
    /// Returns the time the document was last modified.
    ///
    /// May return `None` on platforms that do not track modification time
    pub fn modified(&self) -> Option<SystemTime> {
        self.inner.metadata.modified().ok()
    }

    /// Returns the time the document was created.
    ///
    /// May return `None` on platforms that do not track creation time
    pub fn created(&self) -> Option<SystemTime> {
        self.inner.metadata.created().ok()
    }

    /// Reads and JSON-deserializes the document, consuming it and releasing the lock.
    pub async fn into_value<U>(self) -> Result<U>
    where
        U: DeserializeOwned + Send + 'static,
    {
        run_async(move || Ok(json::from_reader(self.inner)?))
            .await
            .map_err(Into::into)
    }
}

impl AsyncRead for StoredDocument {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        use std::io::Read as _;
        let n = self.inner.read(buf.initialize_unfilled())?;
        buf.advance(n);
        Poll::Ready(Ok(()))
    }
}

impl AsyncSeek for StoredDocument {
    fn start_seek(mut self: Pin<&mut Self>, position: io::SeekFrom) -> io::Result<()> {
        use std::io::Seek as _;
        self.inner.seek(position).map(|_| ())
    }

    fn poll_complete(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<u64>> {
        use std::io::Seek as _;
        Poll::Ready(self.inner.stream_position())
    }
}

/// A temporary file in the document store that is ready to be written to.
#[derive(Debug)]
pub struct StagedDocument {
    inner: StagedFile<DocumentHooks>,
    base: Option<PathBuf>,
}

impl From<StagedFile<DocumentHooks>> for StagedDocument {
    fn from(value: StagedFile<DocumentHooks>) -> Self {
        Self {
            inner: value,
            base: None,
        }
    }
}

impl StagedDocument {
    /// Writes all data from a reader into this staged document.
    ///
    /// This consumes the `StagedDocument` and returns a [`Document`] which can then be committed.
    pub async fn write_all<R: io::Read + Send + 'static>(self, stream: R) -> io::Result<Document> {
        run_async(move || {
            let base = self.base;
            self.inner
                .write_all(stream)
                .map(Document::from)
                .map(|mut d| {
                    d.set_base(base);
                    d
                })
        })
        .await
    }

    pub async fn write_str(self, data: &str) -> io::Result<Document> {
        let stream = io::Cursor::new(data.to_owned());
        self.write_all(stream).await
    }

    pub(crate) fn set_base(&mut self, path: Option<impl Into<PathBuf>>) {
        self.base = path.map(|p| p.into());
    }
}

/// A document that has been written to the store but not yet committed to a path.
#[derive(Debug)]
pub struct Document {
    inner: Blob<DocumentHooks>,
    base: Option<PathBuf>,
}

impl From<Blob<DocumentHooks>> for Document {
    fn from(value: Blob<DocumentHooks>) -> Self {
        Self {
            inner: value,
            base: None,
        }
    }
}

impl Document {
    /// Commits the document to a specific path in the store.
    ///
    /// This atomically moves the document to its final destination.
    /// On success, returns the size of the document.
    ///
    /// If an error happens, the document may be recoverable using [`std::io::Error::downcast`] to
    /// a [`CommitError`]
    pub async fn commit(self, path: impl AsRef<Path>) -> io::Result<u64> {
        let path = path.as_ref().to_path_buf();

        run_async(|| {
            let base = self.base;
            let path = base.as_ref().map(|p| p.join(&path)).unwrap_or_else(|| path);

            self.inner
                .commit(&path)
                .map_err(|err| CommitError {
                    inner: Self {
                        inner: *err.inner,
                        base,
                    }
                    .into(),
                    source: err.source,
                })
                .map_err(io::Error::other)
        })
        .await
    }

    /// Returns the size of the document's content in bytes.
    pub fn size(&self) -> u64 {
        self.inner.size()
    }

    pub(crate) fn set_base(&mut self, path: Option<impl Into<PathBuf>>) {
        self.base = path.map(|p| p.into());
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::time::Duration;

    use pretty_assertions::assert_eq;
    use serde::Deserialize;
    use tempfile::tempdir;
    use tokio::io::AsyncReadExt;

    use super::*;

    #[tokio::test]
    async fn test_basic() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        // Check that it creates store directories.
        assert!(store.db.contents_path().is_dir());
        assert!(store.db.staging_path().is_dir());

        // Use a crate's file for input.
        const SRC: &str = "README.md";

        let file_id: String;

        // Test new file.
        {
            // Open source stream.
            let src = fs::File::open(SRC).unwrap();

            // Get handle to a new file.
            let file = store.stage().await.unwrap();

            // Check that it is created in staging directory.
            assert_eq!(store.db.staging_path(), file.inner.path().parent().unwrap());

            // Write data to handle.
            let blob = file.write_all(src).await.unwrap();

            // Commit blob to contents directory.
            const DST: &str = "some/path/to/README.md";
            blob.commit(DST).await.unwrap();
            file_id = DST.to_string();

            // Check that it is written to the contents directory with proper contents.
            let file_path = store.db.contents_path().join("0").join(DST);
            assert!(file_path.is_file());
            assert_eq!(
                fs::read_to_string(SRC).unwrap(),
                fs::read_to_string(file_path).unwrap()
            );
        }

        // Test open file.
        {
            let mut file = store.open(&file_id).await.unwrap();
            let mut buf = String::new();
            file.read_to_string(&mut buf).await.unwrap();
            assert_eq!(fs::read_to_string(SRC).unwrap(), buf,);
        }

        // Test list files.
        {
            let paths = store.list(None).await.unwrap();
            assert_eq!(paths.len(), 1);
            assert_eq!(paths.first().unwrap(), &PathBuf::from(file_id));
        }
    }

    #[tokio::test]
    async fn test_stage_write_str() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        const DATA: &str = "hello world";
        let doc = store.stage().await.unwrap().write_str(DATA).await.unwrap();

        assert_eq!(doc.size(), DATA.len() as u64);

        let committed_size = doc.commit("greeting.txt").await.unwrap();
        assert_eq!(committed_size, DATA.len() as u64);

        let mut stored = store.open("greeting.txt").await.unwrap();
        let mut buf = String::new();
        stored.read_to_string(&mut buf).await.unwrap();
        assert_eq!(buf, DATA);
    }

    #[tokio::test]
    async fn test_stage_overwrites_existing() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        store
            .stage()
            .await
            .unwrap()
            .write_str("original")
            .await
            .unwrap()
            .commit("file.txt")
            .await
            .unwrap();

        store
            .stage()
            .await
            .unwrap()
            .write_str("updated")
            .await
            .unwrap()
            .commit("file.txt")
            .await
            .unwrap();

        let mut stored = store.open("file.txt").await.unwrap();
        let mut buf = String::new();
        stored.read_to_string(&mut buf).await.unwrap();
        assert_eq!(buf, "updated");
    }

    #[tokio::test]
    async fn test_staged_temp_file_removed_after_commit() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        let staged = store.stage().await.unwrap();
        let temp_path = staged.inner.path().to_path_buf();
        assert!(temp_path.exists());

        staged
            .write_str("data")
            .await
            .unwrap()
            .commit("output.txt")
            .await
            .unwrap();

        assert!(!temp_path.exists());
    }

    #[tokio::test]
    async fn test_staged_temp_file_removed_on_drop() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        let staged = store.stage().await.unwrap();
        let temp_path = staged.inner.path().to_path_buf();
        assert!(temp_path.exists());

        let doc = staged.write_str("data").await.unwrap();
        drop(doc); // dropped without committing

        assert!(!temp_path.exists());
    }

    #[tokio::test]
    async fn test_put_get() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
        struct MyConfig {
            version: String,
            enabled: bool,
        }

        let config = MyConfig {
            version: "1.0".to_string(),
            enabled: true,
        };

        // Test put
        let size = store.put("config.json", &config).await.unwrap();
        assert!(size > 0);

        // Test exists
        assert!(store.exists("config.json").await.unwrap());

        // Test get
        let retrieved_config: MyConfig = store.get("config.json").await.unwrap().unwrap();
        assert_eq!(config, retrieved_config);

        // Test get non-existent file
        let res: Option<MyConfig> = store.get("non-existent.json").await.unwrap();
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn test_delete() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        store.put("a/1.txt", &"doc1").await.unwrap();
        store.put("a/2.txt", &"doc2").await.unwrap();

        assert!(store.exists("a/1.txt").await.unwrap());
        assert!(store.exists("a/2.txt").await.unwrap());

        // Delete one file
        assert!(store.delete("a/1.txt").await.unwrap());

        assert!(!store.exists("a/1.txt").await.unwrap());
        assert!(store.exists("a/2.txt").await.unwrap());

        // Deleting a non-existent file should succeed but return `false`
        assert!(!store.delete("a/1.txt").await.unwrap());
        assert!(!store.delete("non/existent.txt").await.unwrap());
    }

    #[tokio::test]
    async fn test_delete_all() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        store.put("a/1.json", &1u32).await.unwrap();
        store.put("a/2.json", &2u32).await.unwrap();
        store.put("b/3.json", &3u32).await.unwrap();
        store.put("c.json", &4u32).await.unwrap();

        // delete_all returns the paths of deleted files
        let mut deleted: Vec<PathBuf> = store.delete_all("a/*").await.unwrap();
        deleted.sort();
        assert_eq!(
            deleted,
            vec![PathBuf::from("a/1.json"), PathBuf::from("a/2.json")]
        );

        // matched files are gone
        assert!(!store.exists("a/1.json").await.unwrap());
        assert!(!store.exists("a/2.json").await.unwrap());

        // unmatched files are untouched
        assert!(store.exists("b/3.json").await.unwrap());
        assert!(store.exists("c.json").await.unwrap());

        // pattern with no matches returns an empty iterator and deletes nothing
        let deleted: Vec<PathBuf> = store.delete_all("z/*").await.unwrap();
        assert!(deleted.is_empty());
        assert!(store.exists("b/3.json").await.unwrap());
    }

    #[tokio::test]
    async fn test_list_with_patterns() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        store.put("a/1.json", "").await.unwrap();
        store.put("a/2.txt", "").await.unwrap();
        store.put("b/1.json", "").await.unwrap();
        store.put("c.json", "").await.unwrap();

        // List all
        let mut all = store.list(None).await.unwrap();
        all.sort();
        assert_eq!(
            all,
            vec![
                PathBuf::from("a/1.json"),
                PathBuf::from("a/2.txt"),
                PathBuf::from("b/1.json"),
                PathBuf::from("c.json"),
            ]
        );

        // List with wildcard
        let mut a_files = store.list(Some("a/*")).await.unwrap();
        a_files.sort();
        assert_eq!(
            a_files,
            vec![PathBuf::from("a/1.json"), PathBuf::from("a/2.txt")]
        );

        // List with extension
        let mut json_files = store.list(Some("**/*.json")).await.unwrap();
        json_files.sort();
        assert_eq!(
            json_files,
            vec![
                PathBuf::from("a/1.json"),
                PathBuf::from("b/1.json"),
                PathBuf::from("c.json"),
            ]
        );

        // List with no matches
        let no_matches = store.list(Some("*.rs")).await.unwrap();
        assert!(no_matches.is_empty());
    }

    /// 16 tasks all acquire a shared lock on the same file simultaneously.
    /// Every open must succeed; shared locks must not block each other.
    #[tokio::test]
    async fn test_many_concurrent_readers() {
        let root = tempdir().unwrap();
        let store = Arc::new(DocumentStore::with_root(root.path()).await.unwrap());
        store.put("shared.json", &42u32).await.unwrap();

        const N: usize = 16;
        let barrier = Arc::new(tokio::sync::Barrier::new(N));

        let handles: Vec<_> = (0..N)
            .map(|_| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                tokio::spawn(async move {
                    // All tasks race to open at the same moment.
                    barrier.wait().await;
                    let doc = store.open("shared.json").await?;
                    // Hold the lock until every peer has also acquired theirs.
                    barrier.wait().await;
                    drop(doc);
                    Result::<()>::Ok(())
                })
            })
            .collect();

        tokio::time::timeout(Duration::from_secs(5), async {
            for h in handles {
                h.await.unwrap().unwrap();
            }
        })
        .await
        .expect("concurrent readers timed out");
    }

    /// Write must stay blocked as long as *any* shared lock is outstanding;
    /// it succeeds only once the last reader releases.
    #[tokio::test]
    async fn test_write_blocked_until_all_readers_released() {
        use std::io;

        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();
        store.put("data.json", &1u32).await.unwrap();

        let r1 = store.open("data.json").await.unwrap();
        let r2 = store.open("data.json").await.unwrap();
        let r3 = store.open("data.json").await.unwrap();

        let is_would_block = |e: &Error| matches!(e, Error::Io { source, .. } if source.kind() == io::ErrorKind::WouldBlock);

        // All three readers hold locks.
        assert!(is_would_block(
            &store.put("data.json", &2u32).await.unwrap_err()
        ));
        drop(r1);
        // Two readers remain.
        assert!(is_would_block(
            &store.put("data.json", &2u32).await.unwrap_err()
        ));
        drop(r2);
        // One reader remains.
        assert!(is_would_block(
            &store.put("data.json", &2u32).await.unwrap_err()
        ));
        drop(r3);
        // All readers released — write must now succeed.
        store.put("data.json", &2u32).await.unwrap();
        let v: u32 = store.get("data.json").await.unwrap().unwrap();
        assert_eq!(v, 2);
    }

    /// An open (shared lock) must fail with WouldBlock while an exclusive lock
    /// is held externally on the same file.
    #[tokio::test]
    async fn test_open_blocked_by_exclusive_lock() {
        use std::io;

        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();
        store.put("data.json", &1u32).await.unwrap();

        // Acquire an exclusive lock directly on the underlying file.
        let file_path = store
            .db
            .contents_path()
            .join(format!("{VERSION}/data.json"));
        let file = fs::OpenOptions::new().write(true).open(&file_path).unwrap();
        file.try_lock().unwrap();

        // open() (shared lock) must fail while the exclusive lock is held.
        let err = store.open("data.json").await.unwrap_err();
        assert!(
            matches!(&err, Error::Io { source, .. } if source.kind() == io::ErrorKind::WouldBlock),
            "expected WouldBlock, got: {err:?}"
        );

        drop(file); // exclusive lock released

        // open() must now succeed.
        let doc = store.open("data.json").await.unwrap();
        drop(doc);
    }

    /// Spawning N concurrent writes to the same path must not deadlock or panic.
    /// Each task either succeeds or gets WouldBlock; at least one must succeed.
    #[tokio::test]
    async fn test_concurrent_writes_no_deadlock() {
        let root = tempdir().unwrap();
        let store = Arc::new(DocumentStore::with_root(root.path()).await.unwrap());
        store.put("counter.json", &0u32).await.unwrap();

        const N: usize = 16;
        let barrier = Arc::new(tokio::sync::Barrier::new(N));

        let handles: Vec<_> = (0..N as u32)
            .map(|i| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                tokio::spawn(async move {
                    barrier.wait().await;
                    store.put("counter.json", &i).await
                })
            })
            .collect();

        let (mut successes, mut would_block) = (0usize, 0usize);
        tokio::time::timeout(Duration::from_secs(5), async {
            for h in handles {
                match h.await.unwrap() {
                    Ok(_) => successes += 1,
                    Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::WouldBlock => {
                        would_block += 1
                    }
                    Err(e) => panic!("unexpected error: {e:?}"),
                }
            }
        })
        .await
        .expect("concurrent writes timed out");

        assert!(successes >= 1, "at least one write must succeed");
        assert_eq!(successes + would_block, N);
    }

    /// A mix of concurrent readers and writers on the same file must all
    /// complete within the timeout — no task may hang.
    #[tokio::test]
    async fn test_no_deadlock_mixed_reads_and_writes() {
        let root = tempdir().unwrap();
        let store = Arc::new(DocumentStore::with_root(root.path()).await.unwrap());
        store.put("shared.json", &0u32).await.unwrap();

        const READERS: usize = 8;
        const WRITERS: usize = 8;
        let barrier = Arc::new(tokio::sync::Barrier::new(READERS + WRITERS));

        let readers: Vec<_> = (0..READERS)
            .map(|_| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                tokio::spawn(async move {
                    barrier.wait().await;
                    match store.open("shared.json").await {
                        Ok(doc) => drop(doc),
                        Err(Error::Io { source, .. })
                            if source.kind() == io::ErrorKind::WouldBlock => {}
                        Err(e) => panic!("unexpected read error: {e:?}"),
                    }
                })
            })
            .collect();

        let writers: Vec<_> = (0..WRITERS as u32)
            .map(|i| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                tokio::spawn(async move {
                    barrier.wait().await;
                    match store.put("shared.json", &i).await {
                        Ok(_) => {}
                        Err(Error::Io { source, .. })
                            if source.kind() == io::ErrorKind::WouldBlock => {}
                        Err(e) => panic!("unexpected write error: {e:?}"),
                    }
                })
            })
            .collect();

        tokio::time::timeout(Duration::from_secs(5), async {
            for h in readers.into_iter().chain(writers) {
                h.await.unwrap();
            }
        })
        .await
        .expect("mixed reads/writes timed out");
    }

    #[tokio::test]
    async fn test_open_non_existent() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();
        let res = store.open("non-existent.txt").await;
        assert!(matches!(res.unwrap_err(), Error::NotFound { .. }));
    }

    #[tokio::test]
    async fn test_put_get_subpath() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        store.put("a/b/c.json", &42u32).await.unwrap();
        store.put("a/b/d.json", &99u32).await.unwrap();
        store.put("a/e.json", &7u32).await.unwrap();

        assert!(store.exists("a/b/c.json").await.unwrap());
        assert!(store.exists("a/b/d.json").await.unwrap());
        assert!(store.exists("a/e.json").await.unwrap());

        let v: u32 = store.get("a/b/c.json").await.unwrap().unwrap();
        assert_eq!(v, 42);

        let v: u32 = store.get("a/b/d.json").await.unwrap().unwrap();
        assert_eq!(v, 99);

        let v: u32 = store.get("a/e.json").await.unwrap().unwrap();
        assert_eq!(v, 7);

        let none: Option<u32> = store.get("a/b/missing.json").await.unwrap();
        assert!(none.is_none());
    }

    #[tokio::test]
    async fn test_list_at() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        store.put("a/1.json", &1u32).await.unwrap();
        store.put("a/2.json", &2u32).await.unwrap();
        store.put("a/sub/3.json", &3u32).await.unwrap();
        store.put("b/4.json", &4u32).await.unwrap();

        // list_at("a") returns paths relative to "a", scoped to "a" only
        let mut paths = store.list_at(Some(Path::new("a")), None).await.unwrap();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("1.json"),
                PathBuf::from("2.json"),
                PathBuf::from("sub/3.json"),
            ]
        );

        // list_at with a pattern scoped to the base
        let paths = store
            .list_at(Some(Path::new("a")), Some("*.json"))
            .await
            .unwrap();
        assert_eq!(
            paths,
            vec![PathBuf::from("1.json"), PathBuf::from("2.json")]
        );

        // list_at on a deeper sub-path
        let paths = store.list_at(Some(Path::new("a/sub")), None).await.unwrap();
        assert_eq!(paths, vec![PathBuf::from("3.json")]);

        // list_at does not include files from sibling paths
        let paths = store.list_at(Some(Path::new("b")), None).await.unwrap();
        assert_eq!(paths, vec![PathBuf::from("4.json")]);
    }

    #[tokio::test]
    async fn test_list_nonexistent_dir() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        // list on an empty store
        let paths = store.list(None).await.unwrap();
        assert!(paths.is_empty());

        // list_at on a sub-path that was never written to
        let paths = store
            .list_at(Some(Path::new("nonexistent")), None)
            .await
            .unwrap();
        assert!(paths.is_empty());

        // same with a pattern
        let paths = store
            .list_at(Some(Path::new("nonexistent")), Some("*.json"))
            .await
            .unwrap();
        assert!(paths.is_empty());
    }

    #[tokio::test]
    async fn test_with_root_on_existing() {
        let root = tempdir().unwrap();
        {
            let store = DocumentStore::with_root(root.path()).await.unwrap();
            store.put("test.txt", "data").await.unwrap();
        } // store is dropped

        let store = DocumentStore::with_root(root.path()).await.unwrap();
        assert!(store.exists("test.txt").await.unwrap());
    }

    /// All path-accepting methods must return `Error::InvalidPath`, not
    /// `Error::Io`, when given an absolute path or one containing non-normal
    /// components such as `..` or `.`.
    #[tokio::test]
    async fn test_error_invalid_path() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        for raw in ["/absolute/path", "../traversal", "./current"] {
            let path = Path::new(raw);

            assert!(
                matches!(
                    store.open(path).await.unwrap_err(),
                    Error::InvalidPath { .. }
                ),
                "open({raw:?}) should return InvalidPath"
            );
            assert!(
                matches!(
                    store.exists(path).await.unwrap_err(),
                    Error::InvalidPath { .. }
                ),
                "exists({raw:?}) should return InvalidPath"
            );
            assert!(
                matches!(
                    store.delete(path).await.unwrap_err(),
                    Error::InvalidPath { .. }
                ),
                "delete({raw:?}) should return InvalidPath"
            );
            assert!(
                matches!(
                    store.get::<u32>(path).await.unwrap_err(),
                    Error::InvalidPath { .. }
                ),
                "get({raw:?}) should return InvalidPath"
            );
            assert!(
                matches!(
                    store.put(path, &1u32).await.unwrap_err(),
                    Error::InvalidPath { .. }
                ),
                "put({raw:?}) should return InvalidPath"
            );
        }
    }

    /// `list()` must return `Error::InvalidFilter`, not `Error::Io`, when
    /// given a glob pattern that cannot be compiled (unclosed class or
    /// alternate).
    #[tokio::test]
    async fn test_error_document_not_found() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        let err = store.open("some/file").await.unwrap_err();
        assert!(matches!(err, Error::NotFound { .. }))
    }

    /// `list()` must return `Error::InvalidFilter`, not `Error::Io`, when
    /// given a glob pattern that cannot be compiled (unclosed class or
    /// alternate).
    #[tokio::test]
    async fn test_error_invalid_filter() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        for pattern in ["[unclosed", "{unclosed"] {
            let result = store.list(Some(pattern)).await;
            assert!(
                matches!(result, Err(Error::InvalidFilter { .. })),
                "list({pattern:?}) should return InvalidFilter"
            );
        }
    }

    #[tokio::test]
    async fn test_keys_at_root() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        store.put("a/1.json", &1u32).await.unwrap();
        store.put("a/2.json", &2u32).await.unwrap();
        store.put("b/3.json", &3u32).await.unwrap();
        store.put("c.json", &4u32).await.unwrap();

        let mut keys = store.keys_at(None).await.unwrap();
        keys.sort();
        assert_eq!(keys, ["a", "b"]);
    }

    #[tokio::test]
    async fn test_keys() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        store.put("a/1.json", &1u32).await.unwrap();
        store.put("a/2.json", &2u32).await.unwrap();
        store.put("a/sub/3.json", &3u32).await.unwrap();
        store.put("a/sub/deep/4.json", &4u32).await.unwrap();
        store.put("b/5.json", &5u32).await.unwrap();
        store.put("c.json", &6u32).await.unwrap();

        // keys("a") returns only immediate subdirs, not files or deeper dirs
        let mut keys = store.keys("a").await.unwrap();
        keys.sort();
        assert_eq!(keys, ["sub"]);

        // keys("a/sub") returns dirs one level below
        let mut keys = store.keys("a/sub").await.unwrap();
        keys.sort();
        assert_eq!(keys, ["deep"]);

        // keys for a path with only files, no subdirs → empty
        let keys: Vec<String> = store.keys("b").await.unwrap();
        assert!(keys.is_empty());

        // keys for a nonexistent path → empty
        let keys: Vec<String> = store.keys("nonexistent").await.unwrap();
        assert!(keys.is_empty());
    }

    /// `get()` must return `Error::Serialization`, not `Error::Io`, when the
    /// stored content cannot be deserialized into the requested type.
    #[tokio::test]
    async fn test_error_serialization_get() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        store.put("test.json", &"a string value").await.unwrap();

        assert!(matches!(
            store.get::<u32>("test.json").await.unwrap_err(),
            Error::Serialization { .. }
        ));
    }

    #[tokio::test]
    async fn test_gc_cleans_abandoned_staging_files() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        // Create a file in staging directly, simulating an abandoned write.
        let staging = store.db.staging_path().join("abandoned");
        fs::write(&staging, b"leftover").unwrap();
        assert!(staging.exists());

        store.gc().await.unwrap();

        assert!(!staging.exists());
    }

    #[tokio::test]
    async fn test_gc_skips_locked_staging_files() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        let staging = store.db.staging_path().join("in-use");
        fs::write(&staging, b"in progress").unwrap();

        // Hold an exclusive lock to simulate an active write.
        let file = fs::OpenOptions::new().write(true).open(&staging).unwrap();
        file.try_lock().unwrap();

        store.gc().await.unwrap();
        assert!(staging.exists(), "locked staging file must not be removed");

        drop(file);

        store.gc().await.unwrap();
        assert!(!staging.exists(), "unlocked staging file must be removed");
    }

    #[tokio::test]
    async fn test_gc_removes_empty_content_dirs() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        store.put("a/b/file.json", &1u32).await.unwrap();

        // remove the file leaving empty directories
        store.delete("a/b/file.json").await.unwrap();

        let dir_b = store.db.contents_path().join(format!("{VERSION}/a/b"));
        let dir_a = store.db.contents_path().join(format!("{VERSION}/a"));
        assert!(dir_b.is_dir());
        assert!(dir_a.is_dir());

        store.gc().await.unwrap();

        assert!(!dir_b.exists());
        assert!(!dir_a.exists());
    }

    #[tokio::test]
    async fn test_gc_preserves_non_empty_content_dirs() {
        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        store.put("a/1.json", &1u32).await.unwrap();
        store.put("a/2.json", &2u32).await.unwrap();

        let dir_a = store.db.contents_path().join(format!("{VERSION}/a"));
        assert!(dir_a.is_dir());

        store.gc().await.unwrap();

        // Directory still has files, must not be removed.
        assert!(dir_a.is_dir());
        assert!(store.exists("a/1.json").await.unwrap());
        assert!(store.exists("a/2.json").await.unwrap());
    }

    /// `put()` must return `Error::Serialization`, not `Error::Io`, when the
    /// provided value cannot be serialized to JSON.
    #[tokio::test]
    async fn test_error_serialization_put() {
        struct FailsToSerialize;

        impl serde::Serialize for FailsToSerialize {
            fn serialize<S>(&self, _: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                Err(serde::ser::Error::custom("intentional failure"))
            }
        }

        let root = tempdir().unwrap();
        let store = DocumentStore::with_root(root.path()).await.unwrap();

        assert!(matches!(
            store.put("test.json", &FailsToSerialize).await.unwrap_err(),
            Error::Serialization { .. }
        ));
    }
}
