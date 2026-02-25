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

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tokio::runtime;

use super::rand::{self, ALPHA_NUM, PseudoRng, RngExt as _};

mod openflags {
    #[cfg(unix)]
    pub use std::os::unix::fs::OpenOptionsExt;

    #[cfg(unix)]
    pub const O_DIRECTORY: libc::c_int = libc::O_DIRECTORY;
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
    /// ```rust,no_run
    /// use std::fs;
    /// use helios_util::fs::OpenOptionsExt;
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
    fn directory(&mut self) -> &mut fs::OpenOptions;
}

impl OpenOptionsExt for fs::OpenOptions {
    fn directory(&mut self) -> &mut fs::OpenOptions {
        use openflags::OpenOptionsExt;
        self.custom_flags(openflags::O_DIRECTORY);
        self
    }
}

/// Atomically creates a file with the given contents, overwriting
/// it if one exists.
///
/// This function will first write the buffer into a new file that
/// resides in the same directory as the desired file and then do
/// the complete sync/rename dance to ensure the buffer is safely
/// written to disk. If this function returns successfully, you can
/// be reasonably sure the write completed durably.
///
/// Read: [Ensuring data reaches to disk](https://lwn.net/Articles/457667/).
pub async fn safe_write_all<P: AsRef<Path>, B: AsRef<[u8]>>(
    path: P,
    buf: B,
) -> std::io::Result<()> {
    let path = path.as_ref().to_path_buf();
    let buf: Vec<u8> = buf.as_ref().into();

    // perform all synchronous operation in the same task/thread
    run_async(move || {
        // create temp file
        let mut rng = PseudoRng::new();
        let tmp_ext = "sync-".to_owned() + &rng.string(ALPHA_NUM, 6);
        let tmp_path = path.with_extension(tmp_ext);
        let mut tmp_file = fs::File::create(&tmp_path)?;

        // write given contents and sync to disk
        let res = {
            tmp_file.write_all(&buf)?;
            tmp_file.flush()?;
            tmp_file.sync_all()?;
            drop(tmp_file);

            // rename tmp file to destination
            fs::rename(&tmp_path, path)
        };

        if res.is_err() {
            // try to delete the tmp-file ignoring any errors
            let _ = fs::remove_file(&tmp_path);
        }

        res
    })
    .await
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
//
/// ```
/// # async fn example() -> std::io::Result<()> {
/// use helios_util::fs::run_async;
/// use std::fs;
/// use std::io::Write;
/// use std::path::Path;
///
/// let tmp_path = Path::new("foo.txt");
/// let data = b"Hello, world!";
///
/// run_async(move || {
///     let mut tmp_file = fs::File::create(&tmp_path)?;
///
///     // write given contents and sync to disk
///     tmp_file.write_all(data)?;
///     tmp_file.flush()?;
///     tmp_file.sync_all()?;
///     drop(tmp_file);
///
///     Ok(())
/// }).await?;
/// # Ok(())
/// # }
/// ```
#[inline]
pub async fn run_async<F, T>(f: F) -> std::io::Result<T>
where
    F: FnOnce() -> std::io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match runtime::Handle::try_current() {
        Ok(runtime) => runtime.spawn_blocking(f).await?,
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
/// ```
/// use helios_util::fs;
///
/// # fn example() -> std::io::Result<()> {
/// fs::ensure_exists("/tmp/some/new/dir")?;
/// # Ok(())
/// # }
/// ```
pub fn ensure_exists<P: AsRef<Path>>(dir: P) -> std::io::Result<()> {
    fs::create_dir_all(dir.as_ref())
}

/// Sets the permissions of a file or directory.
///
/// On Unix, this sets the mode of the file to the provided permissions.
///
/// # Example
///
/// ```
/// use helios_util::fs;
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
    Ok(())
}

/// Attempts to sync all OS-internal directory metadata to disk.
///
/// This is a synchronous function. For an asynchronous version, wrap this
/// function in [`run_async`].
///
/// # Example
///
/// ```
/// use helios_util::fs;
///
/// # fn example() -> std::io::Result<()> {
/// fs::sync_dir("/path/to/directory")?;
/// # Ok(())
/// # }
/// ```
pub fn sync_dir(path: impl AsRef<Path>) -> io::Result<()> {
    OpenOptions::new()
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
/// ```
/// use std::path::PathBuf;
/// use helios_util::fs;
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
            // AddrInUse can happen if we're creating a UNIX domain socket and
            // the path already exists.
            Err(err) if err.kind() == io::ErrorKind::AddrInUse => {}
            Err(err) => return Err(err),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many files",
    ))
}
