//! Marker files in the runtime directory, recording a fact that has to outlive
//! the process but not the next boot.
//!
//! The runtime directory is a tmpfs, so the reboot removes a breadcrumb by
//! itself and nothing here clears one.

use std::io;
use std::path::PathBuf;

use crate::dirs::{ensure_runtime_dir, runtime_dir};
use crate::fs::run_async;

/// Path of the breadcrumb with the given name.
fn path(name: &str) -> PathBuf {
    runtime_dir().join(name)
}

/// Record the breadcrumb, creating the runtime directory if it is missing.
///
/// Re-recording a breadcrumb that is already there is a success: the callers
/// are tasks that get retried, so this has to be idempotent.
pub async fn set(name: &str) -> io::Result<()> {
    let path = path(name);
    run_async(move || {
        ensure_runtime_dir()?;
        std::fs::File::create(path)?;
        Ok(())
    })
    .await
}

/// Whether the breadcrumb is present.
pub async fn exists(name: &str) -> io::Result<bool> {
    let path = path(name);
    run_async(move || path.try_exists()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Remove the breadcrumb so a test starts from a known state. Only tests
    /// need this: in production the reboot is what removes a breadcrumb.
    fn remove(name: &str) {
        match std::fs::remove_file(path(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => panic!("could not remove breadcrumb: {e}"),
        }
    }

    #[tokio::test]
    async fn it_sets_and_reports_a_breadcrumb() {
        let name = "helios-breadcrumb-test-basic";
        // start from a known state in case a previous run left the file behind
        remove(name);
        assert!(!exists(name).await.unwrap());

        set(name).await.unwrap();
        assert!(exists(name).await.unwrap());

        remove(name);
    }

    #[tokio::test]
    async fn it_is_idempotent() {
        let name = "helios-breadcrumb-test-idempotent";
        remove(name);

        set(name).await.unwrap();
        set(name).await.unwrap();
        assert!(exists(name).await.unwrap());

        remove(name);
    }
}
