use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::Result;
use crate::db::FileId;
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
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let dir = tempdir().unwrap();
/// let store = DocumentStore::with_root(dir.path()).await?;
/// let view = store.as_view().at("my-app")?;
/// view.put("config.json", &json!({ "setting": "enabled" })).await?;
/// # Ok(())
/// # }
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
    pub fn at(&self, path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        FileId::validate_path(path)?;
        Ok(Self::new(self.store, Some(self.path(path))))
    }

    /// Creates a new `View` from a static path string.
    ///
    /// This is a convenience method that panics on debug builds if the path is
    /// invalid, but is a no-op on release builds.
    #[inline]
    pub fn at_static(&self, path: &'static str) -> Self {
        #[cfg(debug_assertions)]
        FileId::validate_path(Path::new(path)).unwrap();
        Self::new(self.store, Some(self.path(Path::new(path))))
    }
}

impl View<'_> {
    /// Creates a new temporary file for writing data within this view.
    ///
    /// See [`DocumentStore::stage`] for more details.
    pub async fn stage(&self) -> Result<StagedDocument> {
        self.store.stage().await.map(|mut d| {
            d.set_base(self.path.as_deref());
            d
        })
    }

    /// Returns whether a document at the specified path (relative to the view) exists.
    ///
    /// See [`DocumentStore::exists`] for caveats related to this function.
    pub async fn exists(&self, path: impl AsRef<Path>) -> Result<bool> {
        self.store.exists(self.path(path.as_ref())).await
    }

    /// Opens the document at the specified path (relative to the view) for reading.
    pub async fn open(&self, path: impl AsRef<Path>) -> Result<StoredDocument> {
        self.store.open(self.path(path.as_ref())).await
    }

    /// Deletes the document at the specified path (relative to the view).
    pub async fn delete(&self, path: impl AsRef<Path>) -> Result<bool> {
        self.store.delete(self.path(path.as_ref())).await
    }

    /// Deletes all the documents within the view that match the specified filter
    ///
    /// The filter is a glob pattern. See [`crate::db::glob`] for syntax.
    pub async fn delete_all(&self, filter: &str) -> Result<Vec<PathBuf>> {
        self.store.delete_all_at(self.path.as_deref(), filter).await
    }

    /// Lists all documents within this view that match the specified filter.
    ///
    /// The filter is a glob pattern. See [`crate::db::glob`] for syntax.
    /// If `None`, all documents are listed.
    ///
    /// Paths in the result are relative to the view's root.
    pub async fn list(&self, filter: Option<&str>) -> Result<Vec<PathBuf>> {
        self.store.list_at(self.path.as_deref(), filter).await
    }

    /// Lists all immediate subdirectories under this view's root.
    ///
    /// Paths in the result are relative to the view's root.
    pub async fn keys(&self) -> Result<Vec<String>> {
        self.store.keys_at(self.path.as_deref()).await
    }
}

impl View<'_> {
    /// JSON-encodes and stores an object at a path relative to the view.
    ///
    /// See [`DocumentStore::put`] for more details.
    pub async fn put<U>(&self, path: impl AsRef<Path>, obj: &U) -> Result<u64>
    where
        U: Serialize,
    {
        self.store.put(self.path(path.as_ref()), obj).await
    }

    /// Loads and JSON-decodes a document from a path relative to the view.
    ///
    /// See [`DocumentStore::get`] for more details.
    pub async fn get<U>(&self, path: impl AsRef<Path>) -> Result<Option<U>>
    where
        U: DeserializeOwned + Send + 'static,
    {
        self.store.get(self.path(path.as_ref())).await
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::{Value, from_str, json};
    use tempfile::tempdir;
    use tokio::io::AsyncReadExt;

    use super::*;

    #[tokio::test]
    async fn test_creation_and_join() {
        let dir = tempdir().unwrap();
        let store = DocumentStore::with_root(dir.path()).await.unwrap();
        let root_view: View = store.as_view();
        assert_eq!(root_view.path, None);

        // Test join with valid path
        let view = root_view.at("app1").unwrap();
        assert_eq!(view.path, Some(PathBuf::from("app1")));

        let view = view.at("data").unwrap();
        assert_eq!(view.path, Some(PathBuf::from("app1/data")));

        // Test join with invalid paths
        assert!(root_view.at("/absolute/path").is_err());
        assert!(root_view.at("..").is_err());
        assert!(root_view.at("a/../b").is_err());

        // Test at_static
        let view = root_view.at_static("static_app");
        assert_eq!(view.path, Some(PathBuf::from("static_app")));
    }

    #[tokio::test]
    #[should_panic]
    #[cfg(debug_assertions)]
    async fn test_at_static_panic_on_invalid() {
        let dir = tempdir().unwrap();
        let store = DocumentStore::with_root(dir.path()).await.unwrap();
        store.as_view().at_static("../invalid");
    }

    #[tokio::test]
    async fn test_operations() {
        let dir = tempdir().unwrap();
        let store = DocumentStore::with_root(dir.path()).await.unwrap();

        // Put some data in the root to make sure view operations are scoped
        store.put("root_doc", &json!("root")).await.unwrap();

        let view = store.as_view().at("my_app").unwrap();

        // Test put and get through the view
        let data = json!({ "version": "1.0" });
        view.put("config.json", &data).await.unwrap();

        let retrieved_data: Value = view.get("config.json").await.unwrap().unwrap();
        assert_eq!(data, retrieved_data);

        // Test exists
        assert!(view.exists("config.json").await.unwrap());
        assert!(!view.exists("nonexistent.json").await.unwrap());
        assert!(store.exists("my_app/config.json").await.unwrap());

        // Test open
        {
            let mut file = view.open("config.json").await.unwrap();
            let mut contents = String::new();
            file.read_to_string(&mut contents).await.unwrap();
            let opened_data: Value = from_str(&contents).unwrap();
            assert_eq!(data, opened_data);
        }

        // Test stage
        let doc_content = "this is a staged document";
        {
            let staged_doc = view.stage().await.unwrap();
            let doc = staged_doc.write_all(doc_content.as_bytes()).await.unwrap();
            doc.commit("staged.txt").await.unwrap();
            assert!(view.exists("staged.txt").await.unwrap());
        }

        // Test commit
        {
            let mut file = view.open("staged.txt").await.unwrap();
            let mut staged_content = String::new();
            file.read_to_string(&mut staged_content).await.unwrap();
            assert_eq!(staged_content, doc_content);
        }
    }

    #[tokio::test]
    async fn test_keys() {
        let dir = tempdir().unwrap();
        let store = DocumentStore::with_root(dir.path()).await.unwrap();

        store.put("app/a/1.json", &1u32).await.unwrap();
        store.put("app/a/2.json", &2u32).await.unwrap();
        store.put("app/b/3.json", &3u32).await.unwrap();
        store.put("app/c.json", &4u32).await.unwrap();
        store.put("other/d.json", &5u32).await.unwrap();

        let view = store.as_view().at("app").unwrap();

        // keys() returns immediate subdirs relative to view root
        let mut keys = view.keys().await.unwrap();
        keys.sort();
        assert_eq!(keys, ["a", "b"]);

        // keys on a nested view
        let view_a = view.at("a").unwrap();
        let keys = view_a.keys().await.unwrap();
        assert!(keys.is_empty()); // no subdirs under app/a/

        // keys on root view returns top-level dirs
        let root_view = store.as_view();
        let mut keys = root_view.keys().await.unwrap();
        keys.sort();
        assert_eq!(keys, ["app", "other"]);
    }

    #[tokio::test]
    async fn test_list_and_delete() {
        let dir = tempdir().unwrap();
        let store = DocumentStore::with_root(dir.path()).await.unwrap();

        // Create a couple of views and populate them
        let view1 = store.as_view().at("view1").unwrap();
        view1.put("doc1.json", &json!(1)).await.unwrap();
        view1.put("doc2.json", &json!(2)).await.unwrap();

        let view2 = store.as_view().at("view2").unwrap();
        view2.put("docA.json", &json!("A")).await.unwrap();

        // Test list on view1
        let mut paths = view1.list(None).await.unwrap();
        paths.sort();
        assert_eq!(
            paths,
            vec![PathBuf::from("doc1.json"), PathBuf::from("doc2.json")]
        );

        // Test list with filter
        let paths = view1.list(Some("doc1*")).await.unwrap();
        assert_eq!(paths, vec![PathBuf::from("doc1.json")]);

        // Test delete
        view1.delete("doc1.json").await.unwrap();
        assert!(!view1.exists("doc1.json").await.unwrap());
        assert!(view1.exists("doc2.json").await.unwrap());
        assert!(view2.exists("docA.json").await.unwrap());
    }

    #[tokio::test]
    async fn test_delete_all() {
        let dir = tempdir().unwrap();
        let store = DocumentStore::with_root(dir.path()).await.unwrap();

        let view = store.as_view().at("app").unwrap();
        view.put("a/1.json", &1u32).await.unwrap();
        view.put("a/2.json", &2u32).await.unwrap();
        view.put("b/3.json", &3u32).await.unwrap();

        // file outside the view must not be touched
        store.put("other/4.json", &4u32).await.unwrap();

        // delete_all returns paths relative to the view root
        let mut deleted: Vec<PathBuf> = view.delete_all("a/*").await.unwrap();
        deleted.sort();
        assert_eq!(
            deleted,
            vec![PathBuf::from("a/1.json"), PathBuf::from("a/2.json")]
        );

        // matched files are gone
        assert!(!view.exists("a/1.json").await.unwrap());
        assert!(!view.exists("a/2.json").await.unwrap());

        // unmatched files inside the view are untouched
        assert!(view.exists("b/3.json").await.unwrap());

        // file outside the view is untouched
        assert!(store.exists("other/4.json").await.unwrap());
    }
}
