//! Utilities for working with the filesystem.
//!
//! This module provides *synchronous* functions that use [`std::fs`] and a
//! utility function to spawn tasks into the background. Unless a function is
//! marked `async`, then it performs blocking I/O.
//!
//! The rationale for prefering functions that are sync is that grouping FS
//! operations into one background task is both safer --no suspension points,
//! less potential for subtle bugs from tasks interleaving-- and likely faster
//! than one [`tokio::task::spawn_blocking`] call per operation that Tokio does.
//! Use the [`run_async`] function to run operations asynchronously.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use super::glob::{self, GlobSet};
use crate::Error;
use crate::util::rand::{self, RngExt as _};

mod openflags {
    #[cfg(unix)]
    pub use std::os::unix::fs::OpenOptionsExt;
    #[cfg(windows)]
    pub use std::os::windows::fs::OpenOptionsExt;

    #[cfg(unix)]
    pub const O_DIRECTORY: libc::c_int = libc::O_DIRECTORY;
    #[cfg(windows)]
    pub const O_DIRECTORY: u32 =
        windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
}

pub trait OpenOptionsExt {
    /// Sets the option to open a file handle to a directory.
    ///
    /// Either [`fs::OpenOptions::read`] or [`fs::OpenOptions::write`] must be used.
    ///
    /// This uses `fs::OpenOptions::custom_flags` which will override any other
    /// flags already set, and subsequent flags will override this one. To get
    /// the combined effect of multiple flags, use [`openflags::O_DIRECTORY`]
    /// OR'ing directly with other flags you want to set.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use std::fs;
    /// use helios_store::util::fs::OpenOptionsExt;
    ///
    /// let dir_handle = fs::OpenOptions::new()
    ///     .read(true)
    ///     .directory()
    ///     .open("/path/to/directory");
    ///
    /// assert!(dir_handle.is_ok());
    /// ```
    ///
    /// # Platform differences
    ///
    /// On Unix systems, this will cause `open()` to return an ENOTDIR error if
    /// the path is not a directory.
    ///
    /// Ono Windows, this will instead allow opening a handle to a directory,
    /// something which std's `File::open` doesn't support otherwise.
    fn directory(&mut self) -> &mut fs::OpenOptions;
}

impl OpenOptionsExt for fs::OpenOptions {
    fn directory(&mut self) -> &mut fs::OpenOptions {
        use openflags::OpenOptionsExt;
        self.custom_flags(openflags::O_DIRECTORY);
        self
    }
}

/// Executes a blocking function on a separate thread if inside a Tokio runtime,
/// synchronously otherwise.
///
/// This function is useful for running synchronous I/O operations without
/// blocking the asynchronous runtime. It uses [`tokio::task::spawn_blocking`]
/// to move the execution of the provided closure to a blocking thread pool.
///
/// Returns an [`io::Error`] if the task panics or fails to execute.
///
/// # Example
///
/// Writing a file asynchronously:
///
/// ```ignore
/// # async fn example() -> std::io::Result<()> {
/// use helios_store::fs::{run_async, safe_write_all};
/// use std::fs;
/// use std::path::Path;
///
/// let path = Path::new("foo.txt");
/// let data = b"Hello, world!";
///
/// run_async(move || {
///    safe_write_all(path, data)
/// }).await?;
/// # Ok(())
/// # }
/// ```
#[inline]
pub async fn run_async<F, T>(f: F) -> Result<T, Error>
where
    F: FnOnce() -> Result<T, Error> + Send + 'static,
    T: Send + 'static,
{
    use tokio::runtime;

    match runtime::Handle::try_current() {
        Ok(handle) => handle.spawn_blocking(f).await.map_err(io::Error::from)?,
        Err(_) => f(),
    }
}

/// Ensures that a directory and all of its parents exist.
///
/// If the directory already exists, this function does nothing.
///
/// This is a synchronous function. For an asynchronous version, wrap this
/// function in [`run_async`].
///
/// Returns an `io::Error` if the directory could not be created and
/// does not already exist.
///
/// # Example
///
/// ```ignore
/// use helios_store::util::fs;
///
/// # fn example() -> std::io::Result<()> {
/// fs::ensure_exists("/tmp/some/new/dir")?;
/// # Ok(())
/// # }
/// ```
pub fn ensure_exists<P: AsRef<Path>>(dir: P) -> io::Result<()> {
    let dir = dir.as_ref();
    match fs::create_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(err) => {
            // create_dir will error if the directory already exists.
            // check if that is the reason why it failed.
            if fs::exists(dir).unwrap_or(false) {
                Ok(())
            } else {
                Err(err)
            }
        }
    }
}

/// Attempts to sync all OS-internal directory metadata to disk.
///
/// This is a synchronous function. For an asynchronous version, wrap this
/// function in [`run_async`].
///
/// # Example
///
/// ```ignore
/// use helios_store::util::fs;
///
/// # fn example() -> std::io::Result<()> {
/// fs::sync_dir("/path/to/directory")?;
/// # Ok(())
/// # }
/// ```
pub fn sync_dir(path: impl AsRef<Path>) -> io::Result<()> {
    fs::OpenOptions::new()
        .read(true)
        .directory()
        .open(path)?
        .sync_all()
}

/// Creates a new temporary file with secure permissions.
///
/// The file is created in the specified directory with `0o600` (read/write for
/// owner only) permissions.
///
/// Returns an [`io::Error`] if the file could not be created.
///
/// The file will *not* be automatically deleted.
///
/// This is a synchronous function. For an asynchronous version, wrap this
/// function in [`run_async`].
///
/// # Example
///
/// ```ignore
/// use std::path::PathBuf;
/// use helios_store::util::fs;
///
/// # fn example() -> std::io::Result<()> {
/// let (path, file) = fs::safe_create_tempfile("/tmp", |name| {
///     format!("prefix-{}.tmp", name).into()
/// })?;
///
/// println!("Created temp file at: {}", path.display());
/// # Ok(())
/// # }
/// ```
pub fn safe_create_tempfile(
    base: impl AsRef<Path>,
    mut f: impl FnMut(&str) -> PathBuf,
) -> io::Result<(PathBuf, File)> {
    // Stolen from tempfile/util.rs

    let mut rng = rand::PseudoRng::new();

    for i in 0..u32::MAX {
        // If we fail to create the file the first three times, re-seed from system randomness in
        // case an attacker is predicting our randomness (fastrand is predictable). If re-seeding
        // doesn't help, either:
        //
        // 1. We have lots of temporary files, possibly created by an attacker but not necessarily.
        //    Re-seeding the randomness won't help here.
        // 2. We're failing to create random files for some other reason. This shouldn't be the case
        //    given that we're checking error kinds, but it could happen.
        if i == 3 {
            let _ = rng.reseed(); // ignore error
        }

        let key = rng.string(rand::ALPHA_NUM_LC, 32);
        let path = base.as_ref().join(f(&key));

        match File::create_new(&path) {
            Ok(file) => {
                set_permissions(&path, 0o600)?;
                return Ok((path, file));
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) if err.kind() == io::ErrorKind::AddrInUse => {}
            Err(err) => return Err(err),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many files",
    ))
}

/// Sets the permissions of a file or directory.
///
/// On Unix, this sets the mode of the file to the provided permissions.
/// On Windows, this is currently a no-op.
///
/// # Example
///
/// ```ignore
/// use helios_store::util::fs;
///
/// # fn example() -> std::io::Result<()> {
/// // Set read/write permissions for owner only.
/// fs::set_permissions("/path/to/file", 0o600)?;
/// # Ok(())
/// # }
/// ```
pub fn set_permissions(path: impl AsRef<Path>, perms: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(perms))?;
    }
    // FIXME: windows
    Ok(())
}

/// Options for configuring directory traversal with [`walk_dir`].
#[derive(Debug, Clone, Default)]
pub struct WalkDirOptions {
    /// The maximum depth to recurse. A value of `0` means no limit. Depth `1`
    /// corresponds to the direct descendents of the starting path, their
    /// descendents have depth `2`, and so on.
    pub max_depth: u8,
    /// The number of nested directory levels to skip. Depth `1` corresponds
    /// to the direct descendents of the starting path, their descendents have
    /// depth `2`, and so on. For consistency with `max_depth`, values `0` and `1`
    /// are equivalent and mean no limit.
    pub min_depth: u8,

    /// If `true`, the walker will follow symbolic links. Defaults to `false`.
    pub follow_symlinks: bool,
    /// If `true`, the walker will descend into directories that are on a
    /// different file system from the starting path. Defaults to `false`.
    pub cross_filesystems: bool,

    /// If `true`, hidden files and directories (those starting with a `.`)
    /// will be excluded from the results. Defaults to `false`.
    pub exclude_hidden: bool,
    /// If `true`, symlinks will be excluded from the results. Defaults to `false`.
    pub exclude_symlinks: bool,
    /// If `true`, files will be excluded from the results. Defaults to `false`.
    pub exclude_files: bool,
    /// If `true`, directories will be excluded from the results. Defaults to `false`.
    pub exclude_directories: bool,

    /// If `true`, block and character devices will be included. This is only
    /// applicable on Unix-like systems. Defaults to `false`.
    pub include_devices: bool,
    /// If `true`, FIFOs and sockets will be included. This is only applicable
    /// on Unix-like systems. Defaults to `false`.
    pub include_special: bool,

    /// A set of glob patterns to include. If non-empty, only paths matching
    /// at least one pattern are included. If empty, all paths are considered
    /// for inclusion (subject to exclusion rules). Directories not matched
    /// by this filter will not be traversed and their contents excluded.
    /// Patterns are matched against paths relative to the starting path.
    pub include: Vec<String>,
    /// A set of glob patterns to exclude. If a path matches any of these
    /// patterns, it is excluded. Exclusions take precedence over inclusions.
    /// Directories matched by this filter will not be traversed and their
    /// contents excluded as well. Patterns are matched against paths relative
    /// to the starting path.
    pub exclude: Vec<String>,
}

pub use walkdir::DirEntry;

/// Recursively walks a directory and yields its entries.
///
/// This function provides a configurable way to iterate over the contents of
/// a directory tree. It returns an iterator over `io::Result<DirEntry>`.
/// The starting `path` itself is not included in the results.
///
/// By default, the walker does not follow symlinks and stays on the same
/// filesystem as the starting path. Its behavior can be customized using
/// [`WalkDirOptions`].
///
/// # Example
///
/// ```ignore
/// use helios_store::util::fs::{walk_dir, WalkDirOptions};
///
/// # fn example() -> std::io::Result<()> {
/// let opts = WalkDirOptions {
///     max_depth: 2,
///     exclude_hidden: true,
///     ..Default::default()
/// };
///
/// for entry in walk_dir("/my/directory", opts).unwrap() {
///     let entry = entry?;
///     println!("Found: {}", entry.path().display());
/// }
/// # Ok(())
/// # }
/// ```
pub fn walk_dir(
    path: impl AsRef<Path>,
    opts: WalkDirOptions,
) -> Result<impl Iterator<Item = io::Result<DirEntry>>, glob::GlobError> {
    // `max_open` allows descending into up to `max_depth` levels of
    // sub-directories without extra buffering.
    let (max_open, max_depth) = if opts.max_depth != 0 {
        (opts.max_depth as usize, opts.max_depth as usize)
    } else {
        (16, usize::MAX)
    };

    // Skip `path`.
    let min_depth = opts.min_depth.max(1) as usize;

    struct TraverseOpts {
        include: GlobSet,
        exclude: GlobSet,
        exclude_hidden: bool,
        exclude_symlinks: bool,
        exclude_files: bool,
        include_devices: bool,
        include_special: bool,
    }

    struct FilterOpts {
        include: GlobSet,
        exclude_directories: bool,
    }

    fn is_hidden(name: Option<&str>) -> bool {
        // FIXME: windows
        name.map(|s| s.starts_with(".")).unwrap_or(false)
    }

    #[cfg(unix)]
    fn is_device(ty: fs::FileType) -> bool {
        use std::os::unix::fs::FileTypeExt;
        ty.is_block_device() || ty.is_char_device()
    }
    #[cfg(windows)]
    fn is_device(_: fs::FileType) -> bool {
        false
    }

    #[cfg(unix)]
    fn is_special(ty: fs::FileType) -> bool {
        use std::os::unix::fs::FileTypeExt;
        ty.is_fifo() || ty.is_socket()
    }
    #[cfg(windows)]
    fn is_special(_: fs::FileType) -> bool {
        false
    }

    let traverse_opts = TraverseOpts {
        include: glob::incremental(opts.include.iter())?,
        exclude: glob::glob(opts.exclude)?,
        exclude_hidden: opts.exclude_hidden,
        exclude_symlinks: opts.exclude_symlinks,
        exclude_files: opts.exclude_files,
        include_devices: opts.include_devices,
        include_special: opts.include_special,
    };

    fn should_traverse(opts: &TraverseOpts, entry: &DirEntry, prefix: &Path) -> bool {
        let ty = entry.file_type();
        let excluded = (!opts.include_devices && is_device(ty))
            || (!opts.include_special && is_special(ty))
            || (opts.exclude_symlinks && ty.is_symlink())
            || (opts.exclude_files && ty.is_file())
            || (opts.exclude_hidden && is_hidden(entry.file_name().to_str()));
        !excluded && {
            let path = entry.path().strip_prefix(prefix).unwrap();
            (opts.include.is_empty() || opts.include.is_match(path))
                && (opts.exclude.is_empty() || !opts.exclude.is_match(path))
        }
    }

    let filter_opts = FilterOpts {
        include: glob::glob(opts.include)?,
        exclude_directories: opts.exclude_directories,
    };

    fn matches_filters(opts: &FilterOpts, entry: &DirEntry, prefix: &Path) -> bool {
        let excluded = opts.exclude_directories && entry.file_type().is_dir();
        !excluded && {
            opts.include.is_empty() || {
                let path = entry.path().strip_prefix(prefix).unwrap();
                opts.include.is_match(path)
            }
        }
    }

    let path_copy = path.as_ref().to_path_buf();

    let iter = walkdir::WalkDir::new(path.as_ref())
        .same_file_system(!opts.cross_filesystems)
        .follow_links(opts.follow_symlinks)
        .follow_root_links(opts.follow_symlinks)
        .max_open(max_open)
        .max_depth(max_depth)
        .min_depth(min_depth)
        .into_iter()
        .filter_entry(move |entry| should_traverse(&traverse_opts, entry, &path_copy))
        .filter(move |res| {
            res.as_ref()
                .map(|entry| matches_filters(&filter_opts, entry, path.as_ref()))
                .unwrap_or(true)
        })
        .map(|res| res.map_err(Into::into));

    Ok(iter)
}

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    fn setup_test_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();

        // Directories
        fs::create_dir_all(root.join("src/module/submod")).unwrap();
        fs::create_dir_all(root.join("empty_dir")).unwrap();
        fs::create_dir_all(root.join(".hidden_dir")).unwrap();

        // Files
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        fs::write(root.join("src/module/mod.rs"), "").unwrap();
        fs::write(root.join("src/module/submod/mod.rs"), "").unwrap();
        fs::write(root.join("README.md"), "").unwrap();
        fs::write(root.join("config.toml"), "").unwrap();
        fs::write(root.join(".hidden_file"), "").unwrap();
        fs::write(root.join(".hidden_dir/file.txt"), "").unwrap();

        // Symlinks
        #[cfg(unix)]
        {
            use std::os::unix::fs as unix_fs;
            unix_fs::symlink("src/main.rs", root.join("main_link.rs")).unwrap();
            unix_fs::symlink("src", root.join("src_link")).unwrap();
        }

        (dir, root)
    }

    fn get_paths(root: &Path, opts: WalkDirOptions) -> impl Iterator<Item = PathBuf> {
        walk_dir(root, opts)
            .unwrap()
            .map(move |r| r.unwrap().path().strip_prefix(root).unwrap().to_path_buf())
    }

    fn assert_paths_eq<A, B>(a: A, b: B)
    where
        A: IntoIterator,
        B: IntoIterator,
        A::Item: Into<PathBuf>,
        B::Item: Into<PathBuf>,
    {
        let mut a: Vec<_> = a.into_iter().map(|p| p.into()).collect();
        let mut b: Vec<_> = b.into_iter().map(|p| p.into()).collect();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b);
    }

    fn assert_matches<I>(root: &Path, opts: WalkDirOptions, expected: I)
    where
        I: IntoIterator,
        I::Item: Into<PathBuf>,
    {
        assert_paths_eq(get_paths(root, opts), expected);
    }

    #[test]
    fn test_walk_dir_default() {
        let (_dir, root) = setup_test_dir();

        assert_matches(
            &root,
            WalkDirOptions::default(),
            [
                ".hidden_dir",
                ".hidden_dir/file.txt",
                ".hidden_file",
                "config.toml",
                "empty_dir",
                "README.md",
                "src",
                "src/lib.rs",
                "src/main.rs",
                "src/module",
                "src/module/mod.rs",
                "src/module/submod",
                "src/module/submod/mod.rs",
                #[cfg(unix)]
                "main_link.rs",
                #[cfg(unix)]
                "src_link",
            ],
        );
    }

    #[test]
    fn test_walk_dir_include() {
        let (_dir, root) = setup_test_dir();

        assert_matches(
            &root,
            WalkDirOptions {
                include: vec!["src/module".to_string()],
                ..Default::default()
            },
            ["src/module"],
        );

        assert_matches(
            &root,
            WalkDirOptions {
                include: vec!["src/module/*".to_string()],
                ..Default::default()
            },
            ["src/module/mod.rs", "src/module/submod"],
        );

        assert_matches(
            &root,
            WalkDirOptions {
                include: vec!["src/**/*".to_string()],
                ..Default::default()
            },
            [
                "src/lib.rs",
                "src/main.rs",
                "src/module",
                "src/module/mod.rs",
                "src/module/submod",
                "src/module/submod/mod.rs",
            ],
        );

        assert_matches(
            &root,
            WalkDirOptions {
                include: vec!["**/*.rs".to_string()],
                ..Default::default()
            },
            [
                "src/lib.rs",
                "src/main.rs",
                "src/module/mod.rs",
                "src/module/submod/mod.rs",
                #[cfg(unix)]
                "main_link.rs",
            ],
        );
    }

    #[test]
    fn test_walk_dir_exclude() {
        let (_dir, root) = setup_test_dir();

        assert_matches(
            &root,
            WalkDirOptions {
                exclude: vec!["**/*.rs".to_string()],
                ..Default::default()
            },
            [
                ".hidden_dir",
                ".hidden_dir/file.txt",
                ".hidden_file",
                "config.toml",
                "empty_dir",
                "README.md",
                "src",
                "src/module",
                "src/module/submod",
                #[cfg(unix)]
                "src_link",
            ],
        );

        assert_matches(
            &root,
            WalkDirOptions {
                exclude: vec!["**/*.*".to_string()],
                ..Default::default()
            },
            [
                "empty_dir",
                "src",
                "src/module",
                "src/module/submod",
                #[cfg(unix)]
                "src_link",
            ],
        );
    }

    #[test]
    fn test_walk_dir_exclude_hidden() {
        let (_dir, root) = setup_test_dir();

        assert_matches(
            &root,
            WalkDirOptions {
                exclude_hidden: true,
                ..Default::default()
            },
            [
                "config.toml",
                "empty_dir",
                "README.md",
                "src",
                "src/lib.rs",
                "src/main.rs",
                "src/module",
                "src/module/mod.rs",
                "src/module/submod",
                "src/module/submod/mod.rs",
                #[cfg(unix)]
                "main_link.rs",
                #[cfg(unix)]
                "src_link",
            ],
        );
    }

    #[test]
    fn test_walk_dir_max_depth() {
        let (_dir, root) = setup_test_dir();

        assert_matches(
            &root,
            WalkDirOptions {
                max_depth: 1,
                ..Default::default()
            },
            [
                ".hidden_dir",
                ".hidden_file",
                "config.toml",
                "empty_dir",
                "README.md",
                "src",
                #[cfg(unix)]
                "main_link.rs",
                #[cfg(unix)]
                "src_link",
            ],
        );
    }

    #[test]
    fn test_walk_dir_exclude_types() {
        let (_dir, root) = setup_test_dir();

        assert_matches(
            &root,
            WalkDirOptions {
                exclude_files: true,
                exclude_symlinks: true,
                ..Default::default()
            },
            [
                ".hidden_dir",
                "empty_dir",
                "src",
                "src/module",
                "src/module/submod",
            ],
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_walk_dir_follow_symlinks() {
        let (_dir, root) = setup_test_dir();

        assert_matches(
            &root,
            WalkDirOptions {
                follow_symlinks: true,
                ..Default::default()
            },
            [
                ".hidden_dir",
                ".hidden_dir/file.txt",
                ".hidden_file",
                "config.toml",
                "empty_dir",
                "main_link.rs",
                "README.md",
                "src",
                "src/lib.rs",
                "src/main.rs",
                "src/module",
                "src/module/mod.rs",
                "src/module/submod",
                "src/module/submod/mod.rs",
                "src_link",
                // These are from following symlinks
                "src_link/lib.rs",
                "src_link/main.rs",
                "src_link/module",
                "src_link/module/mod.rs",
                "src_link/module/submod",
                "src_link/module/submod/mod.rs",
            ],
        );
    }
}
