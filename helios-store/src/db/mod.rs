//! A generic, filesystem-based database for storing files.
//!
//! This module provides the low-level foundation for path-based (`DocumentStore`) storage.
//! It manages the directory structure, file locking for safe concurrency, and the lifecycle of
//! files from staging to commitment.
//!
//! # Core Concepts
//!
//! - **`FileDb`**: The central struct that orchestrates all file operations.
//!
//! - **Hooks (`T: Hooks`)**: The behavior of `FileDb` is customized through the [`Hooks`]
//!   trait. Implementations of this trait define how logical file IDs map to physical
//!   paths, the locking strategy, and how file listings are filtered.
//!
//! - **Directory Structure**: The store maintains a strict directory layout:
//!   - `contents/`: Stores the committed, permanent files.
//!   - `staging/`: A temporary area for files being written. Files are atomically
//!     moved from here to `contents/` upon commitment.
//!
//! - **Concurrency**: `FileDb` is designed for safe multi-thread and multi-process
//!   access. It uses filesystem locks (through `flock`) to coordinate reads and writes. A file can have
//!   multiple concurrent readers or a single exclusive writer.
//!
//! - **File Lifecycle**:
//!   1. A new file is created as a temporary file in the `staging` directory (`new_file`).
//!   2. Data is written to this temporary file.
//!   3. The file is "committed" (`commit_file`), which involves taking an exclusive lock,
//!      atomically renaming the file into the `contents` directory, and syncing the
//!      parent directory to ensure durability.
//!
//! # Limitations
//!
//! - **No Index**: The store does not maintain a central index of files. Operations like
//!   `get`, `put`, and `delete` are O(1) in terms of filesystem operations, but listing
//!   files (`list_files`) requires a full directory traversal, making it linear in complexity
//!   with respect to the number of files.
//! - **No Atomic RMW**: There is no built-in operation for an atomic read-modify-write
//!   cycle. This must be handled at a higher level.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

mod id;
pub(crate) use id::*;

mod glob;
mod util;
pub(crate) use util::*;

mod read;
pub(crate) use read::*;

mod handle;
use handle::*;

mod write;
pub(crate) use write::Blob;
pub use write::CommitError;

mod stage;
pub(crate) use stage::*;

use crate::util::fs as fsutil;
use crate::{Error, trace_ev, trace_ev_detail, trace_ev_path};

/// The commit context for the FileDb
#[derive(Debug)]
pub(crate) struct Context<T> {
    contents_path: PathBuf,
    hooks: T,
}

impl<T: Hooks> Context<T> {
    /// Atomically commits a staged file to its final destination in the `contents` directory.
    ///
    /// This is the final step of a file write operation. The process is:
    /// 1. Acquire an exclusive lock for the target file path.
    /// 2. Ensure the destination directory exists.
    /// 3. Atomically `rename` the staged file to the final path.
    /// 4. Set file permissions to `0o600` (read/write for owner only).
    /// 5. Sync the parent directory to disk to ensure the rename is durable.
    pub fn commit_file(&self, handle: &FileHandle, path: &Path) -> io::Result<()> {
        trace_ev_path(&self.contents_path, "commit", path);

        let id = FileId::try_from(path)?;
        let path = self.contents_path.join(self.hooks.path_for_file(&id)?);

        // prepare destination
        path.parent().map(fsutil::ensure_exists).transpose()?;

        // take excl lock
        let excl_handle = FileHandle::exclusive(&path)?;

        // move staged file to destination
        fs::rename(&handle.path, &excl_handle.path)?;

        // set permissions
        fsutil::set_permissions(&path, 0o600)?;

        // sync destination directory and return
        path.parent().map(fsutil::sync_dir).transpose()?;
        Ok(())
    }
}

/// The core struct for the filesystem-based database.
///
/// See the module-level documentation for a detailed explanation of its
/// design and behavior.
#[derive(Debug)]
pub(crate) struct FileDb<T: ?Sized> {
    contents_path: PathBuf,
    staging_path: PathBuf,

    inner: T,
}

/// A trait that defines the specific behavior of a `FileDb` instance.
///
/// This trait acts as a strategy pattern, allowing `FileDb` to be adapted for
/// different storage models (e.g., path-based vs. content-addressable) by
/// plugging in a different set of hooks.
pub(crate) trait Hooks: Copy + Sized {
    /// Translates a full, physical file path from the `contents` directory back
    /// into a logical, user-facing path.
    ///
    /// This is used during file listing to convert the paths returned by the
    /// directory walker into the paths the user expects.
    ///
    /// - `base`: The root of the `contents` directory.
    /// - `path`: The full path to a file found within the `contents` directory.
    ///
    /// Returns `Some(PathBuf)` with the logical path if it's a valid store file,
    /// or `None` if the file should be ignored.
    fn file_at_path(&self, base: &Path, path: &Path) -> Option<PathBuf>;

    /// Translates a logical `FileId` into a relative physical path within the
    /// `contents` directory.
    ///
    /// This determines the storage location for a given file ID. For example, a
    /// content-addressable store might return `sha256/ab/cd/abcdef...`, while a
    /// document store might return `default/path/to/doc.json`.
    fn path_for_file(&self, id: &FileId) -> io::Result<PathBuf>;

    /// Provides glob patterns for filtering files during a `list_files` operation.
    /// Returns a tuple of `(include, exclude)` patterns.
    fn file_list_filters(
        &self,
        base: Option<&Path>,
        pattern: Option<&str>,
    ) -> (Vec<String>, Vec<String>);

    /// Allows customization of `WalkDirOptions` for file listing.
    /// This can be used to set traversal depth, whether to follow symlinks, etc.
    fn file_list_options(&self, base: Option<&Path>, opts: &mut util::WalkDirOptions);

    /// Provides glob patterns for filtering directories during a `list_dirs` operation.
    /// Returns a tuple of `(include, exclude)` patterns.
    fn dir_list_filters(&self, base: Option<&Path>) -> (Vec<String>, Vec<String>);

    /// Allows customization of `WalkDirOptions` for directory listing.
    fn dir_list_options(&self, base: Option<&Path>, opts: &mut util::WalkDirOptions);
}

impl<T: Hooks> FileDb<T> {
    /// Open the database at the given root.
    ///
    /// It will set-up the internal directories if they have not
    /// been created yet
    pub fn open(hooks: T, root: &Path) -> io::Result<Self> {
        assert!(root.is_absolute());

        let db = Self {
            contents_path: root.join("contents").to_path_buf(),
            staging_path: root.join("staging").to_path_buf(),
            inner: hooks,
        };

        fsutil::ensure_exists(&db.contents_path)?;
        fsutil::ensure_exists(&db.staging_path)?;

        debug_assert!(db.contents_path.is_dir());
        debug_assert!(db.staging_path.is_dir());

        trace_ev(&db.contents_path, "open");

        Ok(db)
    }

    /// Creates a new temporary file in the `staging` directory for writing data.
    ///
    /// This is the first step in adding a new file to the store. The returned
    /// [`StagedFile`] provides a handle for writing. Once writing is complete,
    /// the resulting `Blob` can be committed.
    pub fn new_file(&self) -> io::Result<StagedFile<T>> {
        let handle = FileHandle::tempfile(&self.staging_path)?;
        trace_ev(&self.contents_path, "new");
        Ok(StagedFile::new(self.as_context(), handle))
    }

    /// Checks for the existence of a file with the given logical path.
    ///
    /// This translates the logical path to a physical path using the `Hooks`
    /// and checks if the file exists on the filesystem.
    pub fn has_file(&self, path: &Path) -> io::Result<bool> {
        trace_ev_path(&self.contents_path, "exists", path);
        let id = FileId::try_from(path)?;
        let path = self.contents_path.join(self.inner.path_for_file(&id)?);
        fs::exists(&path)
    }

    /// Opens an existing file for reading.
    ///
    /// This operation acquires a shared lock on the file, allowing multiple
    /// concurrent readers. The lock is held for the lifetime of the returned
    /// [`StoredFile`], preventing any writes to the file while it's being read.
    /// Returns `Error::NotFound` if the file does not exist.
    pub fn open_file(&self, path: &Path) -> io::Result<StoredFile> {
        trace_ev_path(&self.contents_path, "open", path);

        let id = FileId::try_from(path)?;
        let path = self.contents_path.join(self.inner.path_for_file(&id)?);

        let handle = FileHandle::shared(&path)?;
        let metadata = handle.file.metadata()?;
        let stored_file = StoredFile::new(handle, metadata);

        Ok(stored_file)
    }

    /// Deletes a file from the store.
    ///
    /// This operation acquires an exclusive lock, then removes the file.
    /// It is idempotent: if the file does not exist, the operation succeeds
    /// without error. Returns true if the file was removed, false if it didn't
    /// exist.
    pub fn delete_file(&self, path: &Path) -> io::Result<bool> {
        trace_ev_path(&self.contents_path, "delete", path);

        let id = FileId::try_from(path)?;
        let path = self.contents_path.join(self.inner.path_for_file(&id)?);

        if !fs::exists(&path)? {
            return Ok(false);
        }

        // take excl lock
        let handle = FileHandle::exclusive(&path)?;

        match fs::remove_file(&handle.path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// Removes unused staging files and empty directories under the contents directory.
    ///
    /// Staging files currently held by an exclusive lock are skipped. Empty directories
    /// under `contents_path` are removed bottom-up.
    pub fn gc(&self) {
        self.gc_staging();
        self.gc_empty_dirs();
    }

    fn gc_staging(&self) {
        trace_ev(&self.contents_path, "gc_staging");
        let read_dir = match fs::read_dir(&self.staging_path) {
            Ok(rd) => rd,
            Err(_) => return,
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Ok(_handle) = FileHandle::exclusive(&path) {
                trace_ev_path(&self.staging_path, "gc_staging.rm", &path);
                let _ = fs::remove_file(&path);
            }
        }
    }

    fn gc_empty_dirs(&self) {
        trace_ev(&self.contents_path, "gc_empty_dirs");
        let entries: Vec<_> = walkdir::WalkDir::new(&self.contents_path)
            .min_depth(1)
            .contents_first(true)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_dir())
            .map(|e| e.path().to_path_buf())
            .collect();

        for path in entries {
            trace_ev_path(&self.contents_path, "gc_empty_dirs.rmdir", &path);
            let _ = fs::remove_dir(&path);
        }
    }

    /// Extract the commit context from this database
    ///
    /// This ensures staged files can be commited to the right content path
    fn as_context(&self) -> Context<T> {
        Context {
            contents_path: self.contents_path.clone(),
            hooks: self.inner,
        }
    }

    /// Lists files in the store, optionally filtered by a glob pattern.
    ///
    /// This method walks the `contents` directory and yields logical file paths.
    /// The `Hooks` are used to configure the directory traversal (`file_list_options`),
    /// filter the entries (`file_list_filters`), and map the physical paths back
    /// to logical paths (`file_at_path`).
    ///
    /// The returned iterator is not atomic; changes to the store while iterating may be reflected.
    pub fn list_files(
        &self,
        base: Option<&Path>,
        pattern: Option<&str>,
    ) -> io::Result<impl Iterator<Item = io::Result<PathBuf>>> {
        trace_ev_detail(&self.contents_path, "list", pattern.unwrap_or("**"));

        let (include, exclude) = self.inner.file_list_filters(base, pattern);

        let mut opts = util::WalkDirOptions {
            max_depth: 0, // overridden by hooks
            min_depth: 0, // overridden by hooks
            follow_symlinks: false,
            cross_filesystems: false,
            exclude_hidden: true,
            exclude_symlinks: true,
            exclude_files: false,
            exclude_directories: true,
            include_devices: false,
            include_special: false,
            include,
            exclude,
        };

        // WalkDirOptions is a pretty large struct due to the globs, so pass
        // a mut reference instead of moving it in and out of the hook.
        self.inner.file_list_options(base, &mut opts);

        let iter = util::walk_dir(&self.contents_path, opts)
            .map_err(|err| {
                Error::InvalidFilter {
                    filter: pattern
                        // if `pattern` is None, the reason we failed
                        // is the hook, which is programmer error.
                        .expect("hooks should return valid filters")
                        .to_string(),
                    reason: err.to_string(),
                }
            })
            .map_err(io::Error::other)?;

        let iter = iter.filter_map(move |res| {
            match res {
                Ok(entry) => {
                    // strip backend prefix
                    let Some(path) = self.inner.file_at_path(&self.contents_path, entry.path())
                    else {
                        // not a store file
                        trace_ev_path(&self.contents_path, "list.ignore", entry.path());
                        return None;
                    };

                    // strip subpath (as set by a View)
                    let path = base
                        .map(|p| {
                            path.strip_prefix(p)
                                .expect("hook filters should match prefix")
                                .to_path_buf()
                        })
                        .unwrap_or(path);

                    trace_ev_path(&self.contents_path, "list.yield", &path);

                    #[cfg(debug_assertions)]
                    FileId::validate_path(&path).unwrap();

                    Some(Ok(path))
                }
                Err(err) => Some(Err(err)),
            }
        });

        Ok(iter)
    }

    /// Lists immediate subdirectories under `base` in the store.
    ///
    /// The returned paths are relative to `base` (single path component each).
    /// The returned iterator is not atomic; changes to the store while iterating may be reflected.
    pub fn list_dirs(
        &self,
        base: Option<&Path>,
    ) -> io::Result<impl Iterator<Item = io::Result<PathBuf>>> {
        trace_ev_detail(
            &self.contents_path,
            "list_dirs",
            base.and_then(|p| p.to_str()).unwrap_or("*"),
        );

        let (include, exclude) = self.inner.dir_list_filters(base);

        let mut opts = util::WalkDirOptions {
            max_depth: 0, // overridden by hooks
            min_depth: 0, // overridden by hooks
            follow_symlinks: false,
            cross_filesystems: false,
            exclude_hidden: true,
            exclude_symlinks: true,
            exclude_files: true,
            exclude_directories: false,
            include_devices: false,
            include_special: false,
            include,
            exclude,
        };

        self.inner.dir_list_options(base, &mut opts);

        let iter = util::walk_dir(&self.contents_path, opts)
            .map_err(io::Error::other)?
            .filter_map(move |res| match res {
                Ok(entry) => {
                    let Some(path) = self.inner.file_at_path(&self.contents_path, entry.path())
                    else {
                        // not a store file
                        trace_ev_path(&self.contents_path, "list_dirs.ignore", entry.path());
                        return None;
                    };
                    let path = base
                        .map(|p| {
                            path.strip_prefix(p)
                                .expect("hook filters should match prefix")
                                .to_path_buf()
                        })
                        .unwrap_or(path);

                    #[cfg(debug_assertions)]
                    FileId::validate_path(&path).unwrap();
                    trace_ev_path(&self.contents_path, "list_dirs.yield", &path);

                    Some(Ok(path))
                }
                Err(err) => Some(Err(err)),
            });

        Ok(iter)
    }
}

#[cfg(test)]
impl<T> FileDb<T> {
    pub fn contents_path(&self) -> &Path {
        &self.contents_path
    }

    pub fn staging_path(&self) -> &Path {
        &self.staging_path
    }
}
