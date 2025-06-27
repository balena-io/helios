use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::warn;

/// Trait for abstracting file system operations to enable dependency injection
#[async_trait]
pub trait FileSystem {
    /// Read the contents of a file as bytes
    async fn read(&self, path: &Path) -> Result<Vec<u8>>;

    /// Check if a path exists
    async fn exists(&self, path: &Path) -> bool;

    /// Read directory entries and return paths of subdirectories
    #[allow(dead_code)]
    async fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>>;

    /// Remove a file
    async fn remove_file(&self, path: &Path) -> Result<()>;
}

/// Real file system implementation for production use
struct LocalFileSystem;

#[async_trait]
impl FileSystem for LocalFileSystem {
    async fn read(&self, path: &Path) -> Result<Vec<u8>> {
        fs::read(path)
            .await
            .with_context(|| format!("Failed to read file {}", path.display()))
    }

    async fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    async fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let mut entries = fs::read_dir(path)
            .await
            .with_context(|| format!("Failed to read directory {}", path.display()))?;

        let mut dirs = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            }
        }
        Ok(dirs)
    }

    async fn remove_file(&self, path: &Path) -> Result<()> {
        fs::remove_file(path)
            .await
            .with_context(|| format!("Failed to remove file {}", path.display()))
    }
}

/// Overrides handler that applies app-specific configuration overrides
pub struct Overrides {
    /// Device uuid
    uuid: String,
    fs: Box<dyn FileSystem + Send + Sync>,
}

impl Overrides {
    /// Create a new Overrides instance with the specified device UUID and file system
    pub fn new(device_uuid: impl Into<String>) -> Self {
        Self {
            uuid: device_uuid.into(),
            fs: Box::new(LocalFileSystem),
        }
    }

    #[allow(dead_code)]
    fn with_fs<FS>(mut self, fs: FS) -> Self
    where
        FS: FileSystem + Send + Sync + 'static,
    {
        self.fs = Box::new(fs);
        self
    }

    /// Apply overrides to target state by reading override.json files for each app.
    ///
    /// For each app-uuid in the device's target state, this function looks for a file at
    /// `/mnt/temp/apps/{app-uuid}/override.json`. If the file exists and contains
    /// valid JSON, it replaces the corresponding app configuration with the
    /// override content.
    ///
    /// # Arguments
    /// * `target_state` - The original target state as a JSON value
    ///
    /// # Returns
    /// A new JSON value with overrides applied where available
    pub async fn apply(&self, target_state: Value) -> Value {
        let mut result = target_state;

        let apps_path = format!("/{}/apps", self.uuid);
        if let Some(apps_obj) = result
            .pointer_mut(&apps_path)
            .and_then(|apps| apps.as_object_mut())
        {
            for (app_uuid, app_config) in apps_obj.iter_mut() {
                let override_path =
                    PathBuf::from(format!("/mnt/temp/apps/{app_uuid}/override.json"));

                // If there is an override for the app in the temp
                // directory then use that instead of the target state.
                // Ignore any errors
                if let Ok(Some(override_content)) = self.read_override_file(&override_path).await {
                    *app_config = override_content;
                }
            }
        }

        result
    }

    /// Clear all override.json files under /mnt/temp/apps/
    ///
    /// This function recursively searches for override.json files in all app
    /// directories under /mnt/temp/apps/ and removes them.
    ///
    /// # Returns
    /// Result indicating success or failure of the cleanup operation
    #[allow(dead_code)]
    pub async fn clear(&self) {
        let apps_dir = Path::new("/mnt/temp/apps");

        if !self.fs.exists(apps_dir).await {
            return;
        }

        let app_dirs = self.fs.read_dir(apps_dir).await.unwrap_or(Vec::new());
        for app_dir in app_dirs {
            let override_file = app_dir.join("override.json");
            if self.fs.exists(&override_file).await {
                if let Err(e) = self.fs.remove_file(&override_file).await {
                    warn!(
                        "Failed to remove override file {}: {}",
                        override_file.display(),
                        e
                    );
                }
            }
        }
    }

    /// Read and parse an override file if it exists
    async fn read_override_file(&self, path: &Path) -> Result<Option<Value>> {
        if !self.fs.exists(path).await {
            return Ok(None);
        }

        let content = self.fs.read(path).await?;
        let parsed: Value = serde_json::from_slice(&content).with_context(|| {
            format!("Failed to parse JSON from override file {}", path.display())
        })?;

        Ok(Some(parsed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    /// Mock file system implementation for testing
    pub struct MockFileSystem {
        files: HashMap<PathBuf, Vec<u8>>,
        deleted_files: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
    }

    impl MockFileSystem {
        pub fn new() -> Self {
            Self {
                files: HashMap::new(),
                deleted_files: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        pub fn add_file<P: Into<PathBuf>, S: Into<String>>(&mut self, path: P, content: S) {
            self.files.insert(path.into(), content.into().into_bytes());
        }

        pub fn get_deleted_files_handle(&self) -> std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>> {
            self.deleted_files.clone()
        }
    }

    #[async_trait]
    impl FileSystem for MockFileSystem {
        async fn read(&self, path: &Path) -> Result<Vec<u8>> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("File not found: {}", path.display()))
        }

        async fn exists(&self, path: &Path) -> bool {
            // Check if it's an exact file match
            if self.files.contains_key(path) {
                return true;
            }

            // Check if it's a directory (has files under it)
            for file_path in self.files.keys() {
                if file_path.starts_with(path) && file_path != path {
                    return true;
                }
            }

            false
        }

        async fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
            // For mock, we'll simulate the /mnt/temp/apps directory structure
            if path == Path::new("/mnt/temp/apps") {
                let mut dirs = Vec::new();
                for file_path in self.files.keys() {
                    if let Some(parent) = file_path.parent() {
                        if parent.starts_with(path) && parent != path {
                            // Find the immediate subdirectory under /mnt/temp/apps
                            if let Ok(relative) = parent.strip_prefix(path) {
                                if let Some(first_component) = relative.components().next() {
                                    let subdir = path.join(first_component);
                                    if !dirs.contains(&subdir) {
                                        dirs.push(subdir);
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(dirs)
            } else {
                Ok(Vec::new())
            }
        }

        async fn remove_file(&self, path: &Path) -> Result<()> {
            self.deleted_files.lock().unwrap().push(path.to_path_buf());
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_apply_no_overrides() {
        let target_state = json!({
            "device-uuid": {
                "name": "test-device",
                "apps": {
                    "app-uuid-1": {
                        "name": "test-app",
                        "id": 123
                    }
                }
            }
        });

        let overrides = Overrides::new("device-uuid").with_fs(MockFileSystem::new());
        let result = overrides.apply(target_state.clone()).await;
        assert_eq!(result, target_state);
    }

    #[tokio::test]
    async fn test_apply_with_valid_override() {
        let mut mock_fs = MockFileSystem::new();
        mock_fs.add_file(
            "/mnt/temp/apps/app-uuid-1/override.json",
            json!({
                "name": "overridden-app",
                "id": 456,
                "new_field": "added"
            })
            .to_string(),
        );

        let target_state = json!({
            "device-uuid": {
                "name": "test-device",
                "apps": {
                    "app-uuid-1": {
                        "name": "test-app",
                        "id": 123
                    }
                }
            }
        });

        let overrides = Overrides::new("device-uuid").with_fs(mock_fs);
        let result = overrides.apply(target_state).await;

        assert_eq!(
            result["device-uuid"]["apps"]["app-uuid-1"]["name"],
            "overridden-app"
        );
        assert_eq!(result["device-uuid"]["apps"]["app-uuid-1"]["id"], 456);
        assert_eq!(
            result["device-uuid"]["apps"]["app-uuid-1"]["new_field"],
            "added"
        );
    }

    #[tokio::test]
    async fn test_apply_with_invalid_json() {
        let mut mock_fs = MockFileSystem::new();
        mock_fs.add_file(
            "/mnt/temp/apps/app-uuid-1/override.json",
            "invalid json content",
        );

        let target_state = json!({
            "device-uuid": {
                "name": "test-device",
                "apps": {
                    "app-uuid-1": {
                        "name": "test-app",
                        "id": 123
                    }
                }
            }
        });

        let overrides = Overrides::new("device-uuid").with_fs(mock_fs);
        let result = overrides.apply(target_state.clone()).await;

        // Should remain unchanged due to invalid JSON
        assert_eq!(result, target_state);
    }

    #[tokio::test]
    async fn test_apply_device_not_found() {
        let target_state = json!({
            "other-device-uuid": {
                "name": "other-device",
                "apps": {
                    "app-uuid-1": {
                        "name": "test-app",
                        "id": 123
                    }
                }
            }
        });

        let overrides = Overrides::new("nonexistent-device-uuid").with_fs(MockFileSystem::new());
        let result = overrides.apply(target_state.clone()).await;
        assert_eq!(result, target_state);
    }

    #[tokio::test]
    async fn test_apply_multiple_apps() {
        let mut mock_fs = MockFileSystem::new();
        mock_fs.add_file(
            "/mnt/temp/apps/app-uuid-1/override.json",
            json!({"name": "overridden-app-1"}).to_string(),
        );
        mock_fs.add_file(
            "/mnt/temp/apps/app-uuid-2/override.json",
            json!({"name": "overridden-app-2"}).to_string(),
        );

        let target_state = json!({
            "device-uuid": {
                "name": "test-device",
                "apps": {
                    "app-uuid-1": {"name": "original-app-1"},
                    "app-uuid-2": {"name": "original-app-2"},
                    "app-uuid-3": {"name": "original-app-3"}
                }
            }
        });

        let overrides = Overrides::new("device-uuid").with_fs(mock_fs);
        let result = overrides.apply(target_state).await;

        assert_eq!(
            result["device-uuid"]["apps"]["app-uuid-1"]["name"],
            "overridden-app-1"
        );
        assert_eq!(
            result["device-uuid"]["apps"]["app-uuid-2"]["name"],
            "overridden-app-2"
        );
        assert_eq!(
            result["device-uuid"]["apps"]["app-uuid-3"]["name"],
            "original-app-3"
        );
    }

    #[tokio::test]
    async fn test_clear_no_directory() {
        let mock_fs = MockFileSystem::new();
        let deleted_files = mock_fs.get_deleted_files_handle();
        let overrides = Overrides::new("device-uuid").with_fs(MockFileSystem::new());

        overrides.clear().await;

        let deleted = deleted_files.lock().unwrap();
        assert_eq!(deleted.len(), 0);
    }

    #[tokio::test]
    async fn test_clear_with_files() {
        let mut mock_fs = MockFileSystem::new();
        mock_fs.add_file("/mnt/temp/apps/app1/override.json", "{}");
        mock_fs.add_file("/mnt/temp/apps/app2/override.json", "{}");

        // Get a handle to the deleted files list before moving mock_fs
        let deleted_files = mock_fs.get_deleted_files_handle();
        let overrides = Overrides::new("device-uuid").with_fs(mock_fs);

        overrides.clear().await;

        // Verify files were deleted
        let deleted = deleted_files.lock().unwrap();
        assert!(deleted.contains(&PathBuf::from("/mnt/temp/apps/app1/override.json")));
        assert!(deleted.contains(&PathBuf::from("/mnt/temp/apps/app2/override.json")));
    }
}
