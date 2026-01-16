use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::Result;
use crate::db::{Digest, FileId};
use crate::document::{DocumentStore, StagedDocument, StoredDocument};

/// A view into a sub-path of a [`DocumentStore`].
///
/// A `View` allows you to scope all store operations to a specific directory
/// within the `DocumentStore`. All paths are treated as relative to the view's
/// root path.
///
/// # Example
///
/// ```
/// use serde_json::json;
/// use tempfile::tempdir;
/// use helios_store::DocumentStore;
///
/// let dir = tempdir().unwrap();
/// let store = DocumentStore::create_with_root(dir.path()).unwrap();
/// let view = store.as_view().join("my-app").unwrap();
/// view.put("config.json", &json!({ "setting": "enabled" })).unwrap();
/// ```
#[derive(Debug, Clone)]
pub struct View<'a> {
    store: &'a DocumentStore,
    path: Option<PathBuf>,
}

impl<'a> From<&'a DocumentStore> for View<'a> {
    fn from(value: &'a DocumentStore) -> Self {
        View::new(value, None)
    }
}

impl<'a> View<'a> {
    fn new(store: &'a DocumentStore, path: Option<PathBuf>) -> Self {
        debug_assert!(path.as_deref().map(|p| p.is_relative()).unwrap_or(true));
        Self { store, path }
    }

    fn path(&self, path: &Path) -> PathBuf {
        self.path
            .as_deref()
            .map(|p| p.join(path))
            .unwrap_or_else(|| path.to_path_buf())
    }
}

impl View<'_> {
    /// Creates a new `View` at the given path relative to this store's root.
    ///
    /// Returns [`crate::Error::InvalidPath`] if the given path is absolute or contains `..`.
    #[inline]
    pub fn join(&self, path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        FileId::validate_path(path)?;
        Ok(Self::new(self.store, Some(self.path(path))))
    }

    /// Creates a new `View` from a static path string.
    ///
    /// This is a convenience method that panics on debug builds if the path is
    /// invalid, but is a no-op on release builds.
    #[inline]
    pub fn join_static(&self, path: &'static str) -> Self {
        #[cfg(debug_assertions)]
        FileId::validate_path(Path::new(path)).unwrap();
        Self::new(self.store, Some(self.path(Path::new(path))))
    }
}

impl View<'_> {
    /// Creates a new temporary file for writing data within this view.
    ///
    /// See [`DocumentStore::stage`] for more details.
    pub fn stage(&self) -> Result<StagedDocument<'_>> {
        self.store.stage().map(|mut d| {
            d.set_base(self.path.as_deref());
            d
        })
    }

    /// Returns whether a document at the specified path (relative to the view) exists.
    ///
    /// See [`DocumentStore::exists`] for caveats related to this function.
    pub fn exists(&self, path: impl AsRef<Path>) -> Result<bool> {
        self.store.exists(self.path(path.as_ref()))
    }

    /// Opens the document at the specified path (relative to the view) for reading.
    pub fn open(&self, path: impl AsRef<Path>) -> Result<StoredDocument<'_>> {
        self.store.open(self.path(path.as_ref()))
    }

    /// Deletes the document at the specified path (relative to the view).
    pub fn delete(&self, path: impl AsRef<Path>) -> Result<bool> {
        self.store.delete(self.path(path.as_ref()))
    }

    /// Lists all documents within this view that match the specified filter.
    ///
    /// The filter is a glob pattern. See [`aetna_util::glob`] for syntax.
    /// If `None`, all documents are listed.
    ///
    /// Paths in the result are relative to the view's root.
    pub fn list(&self, filter: Option<&str>) -> Result<impl Iterator<Item = io::Result<PathBuf>>> {
        self.store.list_at(self.path.as_deref(), filter)
    }
}

impl View<'_> {
    /// JSON-encodes and stores an object at a path relative to the view.
    ///
    /// See [`DocumentStore::put`] for more details.
    pub fn put<U>(&self, path: impl AsRef<Path>, obj: &U) -> Result<(Digest, u64)>
    where
        U: Serialize,
    {
        self.store.put(self.path(path.as_ref()), obj)
    }

    /// Loads and JSON-decodes a document from a path relative to the view.
    ///
    /// See [`DocumentStore::get`] for more details.
    pub fn get<U>(&self, path: impl AsRef<Path>) -> Result<Option<U>>
    where
        U: DeserializeOwned,
    {
        self.store.get(self.path(path.as_ref()))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use pretty_assertions::assert_eq;
    use serde_json::{Value, from_str, json};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_creation_and_join() {
        let dir = tempdir().unwrap();
        let store = DocumentStore::create_with_root(dir.path()).unwrap();
        let root_view: View = store.as_view();
        assert_eq!(root_view.path, None);

        // Test join with valid path
        let view = root_view.join("app1").unwrap();
        assert_eq!(view.path, Some(PathBuf::from("app1")));

        let view = view.join("data").unwrap();
        assert_eq!(view.path, Some(PathBuf::from("app1/data")));

        // Test join with invalid paths
        assert!(root_view.join("/absolute/path").is_err());
        assert!(root_view.join("..").is_err());
        assert!(root_view.join("a/../b").is_err());

        // Test join_static
        let view = root_view.join_static("static_app");
        assert_eq!(view.path, Some(PathBuf::from("static_app")));
    }

    #[test]
    #[should_panic]
    #[cfg(debug_assertions)]
    fn test_join_static_panic_on_invalid() {
        let dir = tempdir().unwrap();
        let store = DocumentStore::create_with_root(dir.path()).unwrap();
        store.as_view().join_static("../invalid");
    }

    #[test]
    fn test_operations() {
        let dir = tempdir().unwrap();
        let store = DocumentStore::create_with_root(dir.path()).unwrap();

        // Put some data in the root to make sure view operations are scoped
        store.put("root_doc", &json!("root")).unwrap();

        let view = store.as_view().join("my_app").unwrap();

        // Test put and get through the view
        let data = json!({ "version": "1.0" });
        view.put("config.json", &data).unwrap();

        let retrieved_data: Value = view.get("config.json").unwrap().unwrap();
        assert_eq!(data, retrieved_data);

        // Test exists
        assert!(view.exists("config.json").unwrap());
        assert!(!view.exists("nonexistent.json").unwrap());
        assert!(store.exists("my_app/config.json").unwrap());

        // Test open
        {
            let mut file = view.open("config.json").unwrap();
            let mut contents = String::new();
            file.read_to_string(&mut contents).unwrap();
            let opened_data: Value = from_str(&contents).unwrap();
            assert_eq!(data, opened_data);
        }

        // Test stage
        let doc_content = "this is a staged document";
        {
            let staged_doc = view.stage().unwrap();
            let doc = staged_doc.write_all(doc_content.as_bytes()).unwrap();
            doc.commit("staged.txt").unwrap();
            assert!(view.exists("staged.txt").unwrap());
        }

        // Test commit
        {
            let mut staged_content = String::new();
            view.open("staged.txt")
                .unwrap()
                .read_to_string(&mut staged_content)
                .unwrap();
            assert_eq!(staged_content, doc_content);
        }
    }

    #[test]
    fn test_list_and_delete() {
        let dir = tempdir().unwrap();
        let store = DocumentStore::create_with_root(dir.path()).unwrap();

        // Create a couple of views and populate them
        let view1 = store.as_view().join("view1").unwrap();
        view1.put("doc1.json", &json!(1)).unwrap();
        view1.put("doc2.json", &json!(2)).unwrap();

        let view2 = store.as_view().join("view2").unwrap();
        view2.put("docA.json", &json!("A")).unwrap();

        // Test list on view1
        let mut paths: Vec<PathBuf> = view1.list(None).unwrap().map(|r| r.unwrap()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![PathBuf::from("doc1.json"), PathBuf::from("doc2.json")]
        );

        // Test list with filter
        let paths: Vec<PathBuf> = view1
            .list(Some("doc1*"))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(paths, vec![PathBuf::from("doc1.json")]);

        // Test delete
        view1.delete("doc1.json").unwrap();
        assert!(!view1.exists("doc1.json").unwrap());
        assert!(view1.exists("doc2.json").unwrap()); // Other file should still exist
        assert!(view2.exists("docA.json").unwrap()); // File in other view should still exist
    }
}
