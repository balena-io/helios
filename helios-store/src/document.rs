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
//! let dir = tempdir().unwrap();
//! let store = DocumentStore::create_with_root(dir.path()).unwrap();
//! let data = json!({ "version": "1.0" });
//! store.put("config.json", &data).unwrap();
//! ```

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json as json;

use crate::util::fs as fsutil;

use crate::db::{Blob, CommitError, Digest, FileDb, FileId, Hooks, StagedFile, StoredFile};
use crate::view::View;
use crate::{Error, Result};

/// A store for documents, keyed by path.
///
/// See module documentation for more info.
#[derive(Debug)]
pub struct DocumentStore {
    db: FileDb<DocumentHooks>,
}

#[derive(Debug)]
struct DocumentHooks {}

impl Hooks for DocumentHooks {
    fn file_at_path(&self, base: &Path, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(base.join("0"))
            .ok()
            .map(|p| p.to_path_buf())
    }

    fn path_for_file(&self, id: &FileId) -> Result<PathBuf> {
        Ok(format!("0/{id}").into())
    }

    fn lock_for_file(&self, _id: Option<&FileId>) -> Option<FileId> {
        None
    }

    fn file_list_filters(
        &self,
        base: Option<&Path>,
        pattern: Option<&str>,
    ) -> (Vec<String>, Vec<String>) {
        let mut parts = Vec::with_capacity(2);

        parts.push("0");

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

    fn file_list_options(&self, base: Option<&Path>, opts: &mut fsutil::WalkDirOptions) {
        opts.max_depth = 0; // /0/<abcdef...>
        opts.min_depth = 2; //  1     2 ...
        opts.min_depth += base.map(|p| p.components().count() as u8).unwrap_or(0);
    }
}

impl DocumentStore {
    /// Opens an existing document store at the specified path.
    ///
    /// Returns an [`io::Error`] if the path does not exist or is not a valid store.
    pub fn with_root(root: impl AsRef<Path>) -> io::Result<Self> {
        let hooks = DocumentHooks {};
        let db = FileDb::open(hooks, root.as_ref(), false)?;
        Ok(Self { db })
    }

    /// Opens a document store at the specified path, creating it if it doesn't exist.
    ///
    /// Returns an [`io::Error`] if the path cannot be created.
    pub fn create_with_root(root: impl AsRef<Path>) -> io::Result<Self> {
        let hooks = DocumentHooks {};
        let db = FileDb::open(hooks, root.as_ref(), true)?;
        Ok(Self { db })
    }

    /// Returns a [`View`] into the root of the store.
    ///
    /// A view allows operating on a sub-path within the store.
    pub fn as_view(&self) -> View<'_> {
        self.into()
    }

    /// Creates a new temporary file for writing data into the store.
    ///
    /// This returns a [`StagedDocument`] which can be written to. Once written,
    /// the resulting [`Document`] can be committed to a specific path in the store.
    pub fn stage(&self) -> Result<StagedDocument<'_>> {
        self.db.new_file().map(Into::into)
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
    /// let dir = tempdir().unwrap();
    /// let store = DocumentStore::create_with_root(dir.path()).unwrap();
    /// assert!(!store.exists("my-doc").unwrap());
    /// ```
    pub fn exists(&self, path: impl AsRef<Path>) -> Result<bool> {
        self.db.has_file(path.as_ref())
    }

    /// Opens the document at the specified path for reading.
    ///
    /// Returns a [`StoredDocument`] which implements [`io::Read`] and [`io::Seek`].
    pub fn open(&self, path: impl AsRef<Path>) -> Result<StoredDocument<'_>> {
        self.db.open_file(path.as_ref()).map(Into::into)
    }

    /// Deletes the document at the specified path.
    ///
    /// This operation is idempotent; deleting a non-existent document will succeed.
    /// Returns whether the file was deleted.
    pub fn delete(&self, path: impl AsRef<Path>) -> Result<bool> {
        self.db.delete_file(path.as_ref())
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
    /// let dir = tempdir().unwrap();
    /// let store = DocumentStore::create_with_root(dir.path()).unwrap();
    /// store.put("a/1", &"").unwrap();
    /// store.put("b/2", &"").unwrap();
    ///
    /// let iter = store.list(Some("a/*")).unwrap();;
    /// let paths: Vec<PathBuf> = iter.map(|r| r.unwrap()).collect();
    /// assert_eq!(paths, vec![PathBuf::from("a/1")]);
    /// ```
    pub fn list(&self, pattern: Option<&str>) -> Result<impl Iterator<Item = io::Result<PathBuf>>> {
        self.db.list_files(None::<&Path>, pattern)
    }

    pub(crate) fn list_at(
        &self,
        base: Option<&Path>,
        pattern: Option<&str>,
    ) -> Result<impl Iterator<Item = io::Result<PathBuf>>> {
        self.db.list_files(base, pattern)
    }

    /// JSON-encodes and stores the given object at the specified path.
    ///
    /// This is a convenience method that serializes a `Serialize`-able object to JSON
    /// and writes it to the given path. It returns the digest and size of the stored data.
    pub fn put<U>(&self, path: impl AsRef<Path>, obj: &U) -> Result<(Digest, u64)>
    where
        U: Serialize,
    {
        let ctx = Error::with_path(path.as_ref());

        let file = self.db.new_file()?;
        let bytes = json::to_vec_pretty(obj)?;
        let blob = file.write_all(bytes.as_slice()).map_err(&ctx)?;

        match blob.commit(path.as_ref()) {
            Ok((digest, size)) => Ok((digest, size)),
            Err(err) => Err(err.source),
        }
    }

    /// Loads and JSON-decodes the document at the specified path.
    ///
    /// This is a convenience method that reads a document from the store and
    /// deserializes it from JSON into an instance of `U`.
    pub fn get<U>(&self, path: impl AsRef<Path>) -> Result<Option<U>>
    where
        U: DeserializeOwned,
    {
        let file = match self.db.open_file(path.as_ref()) {
            Ok(fd) => fd,
            Err(Error::NotFound { .. }) => return Ok(None),
            Err(err) => return Err(err),
        };
        let obj: U = json::from_reader(file)?;
        Ok(Some(obj))
    }
}

/// A document in the store that has been opened for reading.
#[derive(Debug)]
pub struct StoredDocument<'a> {
    inner: StoredFile<'a, DocumentHooks>,
}

impl<'a> From<StoredFile<'a, DocumentHooks>> for StoredDocument<'a> {
    fn from(value: StoredFile<'a, DocumentHooks>) -> Self {
        Self { inner: value }
    }
}

impl io::Read for StoredDocument<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }

    fn read_vectored(&mut self, bufs: &mut [io::IoSliceMut<'_>]) -> io::Result<usize> {
        self.inner.read_vectored(bufs)
    }
}

impl io::Seek for StoredDocument<'_> {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }

    fn stream_position(&mut self) -> io::Result<u64> {
        self.inner.stream_position()
    }
}

/// A temporary file in the document store that is ready to be written to.
#[derive(Debug)]
pub struct StagedDocument<'a> {
    inner: StagedFile<'a, DocumentHooks>,
    base: Option<PathBuf>,
}

impl<'a> From<StagedFile<'a, DocumentHooks>> for StagedDocument<'a> {
    fn from(value: StagedFile<'a, DocumentHooks>) -> Self {
        Self {
            inner: value,
            base: None,
        }
    }
}

impl<'a> StagedDocument<'a> {
    /// Writes all data from a reader into this staged document.
    ///
    /// This consumes the `StagedDocument` and returns a [`Document`] which can then be committed.
    pub fn write_all<R: io::Read>(self, stream: R) -> io::Result<Document<'a>> {
        let base = self.base;
        self.inner
            .write_all(stream)
            .map(Document::from)
            .map(|mut d| {
                d.set_base(base);
                d
            })
    }

    pub fn write_str(self, data: &str) -> io::Result<Document<'a>> {
        let stream = io::Cursor::new(data);
        self.write_all(stream)
    }

    pub(crate) fn set_base(&mut self, path: Option<impl Into<PathBuf>>) {
        self.base = path.map(|p| p.into());
    }
}

/// A document that has been written to the store but not yet committed to a path.
#[derive(Debug)]
pub struct Document<'a> {
    inner: Blob<'a, DocumentHooks>,
    base: Option<PathBuf>,
}

impl<'a> From<Blob<'a, DocumentHooks>> for Document<'a> {
    fn from(value: Blob<'a, DocumentHooks>) -> Self {
        Self {
            inner: value,
            base: None,
        }
    }
}

type CommitResult<'a> = std::result::Result<(Digest, u64), CommitError<Document<'a>>>;

impl<'db> Document<'db> {
    /// Commits the document to a specific path in the store.
    ///
    /// This atomically moves the document to its final destination.
    /// On success, returns the digest and size of the document.
    pub fn commit(self, path: impl AsRef<Path>) -> CommitResult<'db> {
        let path = path.as_ref();
        let base = self.base;

        let path = base
            .as_ref()
            .map(|p| p.join(path))
            .unwrap_or_else(|| path.to_path_buf());

        self.inner.commit(&path).map_err(|err| CommitError {
            inner: Self {
                inner: *err.inner,
                base,
            }
            .into(),
            source: err.source,
        })
    }

    /// Returns the digest of the document's content.
    pub fn digest(&self) -> &Digest {
        self.inner.digest()
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
    use std::io;

    use pretty_assertions::assert_eq;
    use serde::Deserialize;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_basic() {
        let root = tempdir().unwrap();
        let store = DocumentStore::create_with_root(root.path()).unwrap();

        // Check that it creates store directories.
        assert!(store.db.contents_path().is_dir());
        assert!(store.db.staging_path().is_dir());
        assert!(store.db.private_path().is_dir());

        // Use a crate's file for input.
        const SRC: &str = "README.md";

        let file_id: String;

        // Test new file.
        {
            // Open source stream.
            let src = fs::File::open(SRC).unwrap();

            // Get handle to a new file.
            let file = store.stage().unwrap();

            // Check that it is created in staging directory.
            assert_eq!(store.db.staging_path(), file.inner.path().parent().unwrap());

            // Write data to handle.
            let blob = file.write_all(&src).unwrap();

            // Commit blob to contents directory.
            const DST: &str = "some/path/to/README.md";
            let (_digest, _size) = blob.commit(DST).unwrap();
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
            let file = store.open(&file_id).unwrap();
            assert_eq!(
                fs::read_to_string(SRC).unwrap(),
                io::read_to_string(file).unwrap()
            );
        }

        // Test list files.
        {
            let file = store.list(None).unwrap();
            let paths: Vec<_> = file.map(|res| res.unwrap()).collect();
            assert_eq!(paths.len(), 1);
            assert_eq!(paths.first().unwrap(), &PathBuf::from(file_id));
        }
    }

    #[test]
    fn test_put_get() {
        let root = tempdir().unwrap();
        let store = DocumentStore::create_with_root(root.path()).unwrap();

        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct MyConfig {
            version: String,
            enabled: bool,
        }

        let config = MyConfig {
            version: "1.0".to_string(),
            enabled: true,
        };

        // Test put
        let (digest, size) = store.put("config.json", &config).unwrap();
        assert!(!digest.value.is_empty());
        assert!(size > 0);

        // Test exists
        assert!(store.exists("config.json").unwrap());

        // Test get
        let retrieved_config: MyConfig = store.get("config.json").unwrap().unwrap();
        assert_eq!(config, retrieved_config);

        // Test get non-existent file
        let res: Option<MyConfig> = store.get("non-existent.json").unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn test_delete() {
        let root = tempdir().unwrap();
        let store = DocumentStore::create_with_root(root.path()).unwrap();

        store.put("a/1.txt", &"doc1").unwrap();
        store.put("a/2.txt", &"doc2").unwrap();

        assert!(store.exists("a/1.txt").unwrap());
        assert!(store.exists("a/2.txt").unwrap());

        // Delete one file
        assert!(store.delete("a/1.txt").unwrap());

        assert!(!store.exists("a/1.txt").unwrap());
        assert!(store.exists("a/2.txt").unwrap());

        // Deleting a non-existent file should succeed but return `false`
        assert!(!store.delete("a/1.txt").unwrap());
        assert!(!store.delete("non/existent.txt").unwrap());
    }

    #[test]
    fn test_list_with_patterns() {
        let root = tempdir().unwrap();
        let store = DocumentStore::create_with_root(root.path()).unwrap();

        store.put("a/1.json", &"").unwrap();
        store.put("a/2.txt", &"").unwrap();
        store.put("b/1.json", &"").unwrap();
        store.put("c.json", &"").unwrap();

        // List all
        let mut all: Vec<_> = store.list(None).unwrap().map(|r| r.unwrap()).collect();
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
        let mut a_files: Vec<_> = store
            .list(Some("a/*"))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        a_files.sort();
        assert_eq!(
            a_files,
            vec![PathBuf::from("a/1.json"), PathBuf::from("a/2.txt")]
        );

        // List with extension
        let mut json_files: Vec<_> = store
            .list(Some("**/*.json"))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
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
        let no_matches: Vec<_> = store
            .list(Some("*.rs"))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(no_matches.is_empty());
    }

    #[test]
    fn test_open_non_existent() {
        let root = tempdir().unwrap();
        let store = DocumentStore::create_with_root(root.path()).unwrap();
        let res = store.open("non-existent.txt");
        assert!(matches!(res.unwrap_err(), Error::NotFound { .. }));
    }

    #[test]
    fn test_with_root_on_existing() {
        let root = tempdir().unwrap();
        {
            let store = DocumentStore::create_with_root(root.path()).unwrap();
            store.put("test.txt", &"data").unwrap();
        } // store is dropped

        let store = DocumentStore::with_root(root.path()).unwrap();
        assert!(store.exists("test.txt").unwrap());
    }
}
