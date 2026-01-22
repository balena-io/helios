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

use std::fs;
use std::io::Write;
use std::path::Path;
use tokio::runtime;

use super::crypto::{ALPHA_NUM, pseudorandom_string};

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
        let tmp_ext = "sync-".to_owned() + &pseudorandom_string(ALPHA_NUM, 6);
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
