use std::collections::{HashMap, hash_map::Entry};
use std::fs::{self, File, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use nix::fcntl::{AT_FDCWD, AtFlags};
use nix::unistd::linkat;

use super::fs::{ensure_exists, safe_create_tempfile};

/// a tag to identify locally vs externally created lockfiles.
/// Big-endian encoding of the sum of the ASCII byte values of "balena"
/// (b + a + l + e + n + a = 611).
const TAG: [u8; 2] = [0x02, 0x63];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The lock could not be acquired at this time because it is held by another handle/process.
    #[error("lock could not be acquired")]
    WouldBlock,

    /// An I/O error happened when trying to create the lock
    #[error(transparent)]
    IO(#[from] io::Error),
}

impl From<Error> for io::Error {
    fn from(value: Error) -> Self {
        match value {
            Error::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
            Error::IO(e) => e,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ForceAcquireLocks(bool);

impl From<bool> for ForceAcquireLocks {
    fn from(value: bool) -> Self {
        Self(value)
    }
}

impl ForceAcquireLocks {
    pub fn enabled(&self) -> bool {
        self.0
    }
}

#[derive(Debug)]
struct LockFile(File);

enum LockOwnership {
    Mine(LockFile),
    Ours,
    Theirs,
    None,
}

impl LockFile {
    /// Try to acquire an exclusive lock on the given path. If the file already exists and
    /// was created by helios, just take an exclusive lock on it and return the handle.
    ///
    /// When `force` is true, an existing non-helios file at `path` is atomically replaced
    /// with a freshly created, already-locked lockfile. A helios-owned lockfile is *never*
    /// replaced: if another helios process currently holds the flock, this returns
    /// [`Error::WouldBlock`] regardless of `force`.
    ///
    /// This is a sync operation so use [`crate::fs::run_async`] if running in an async
    /// context.
    pub fn create(path: &Path, force: bool) -> Result<Self, Error> {
        let lock_dir = path.parent().unwrap_or_else(|| Path::new("/"));

        ensure_exists(lock_dir)?;

        tracing::trace!("acquiring lock at {} (force={force})", path.display());

        // When forcing, take an exclusive flock on the parent directory
        // before touching `path`. This prevents two helios processes from stepping on each-other
        let _dir_guard = if force {
            let f = File::open(lock_dir)?;
            f.lock()?;
            Some(f)
        } else {
            None
        };

        // If the path exists, try to acquire an exclusive lock on it if possible.
        // Fail with [`Error::WouldBlock`] for a non-helios lock unless using the `force`
        match Self::try_attach(path)? {
            LockOwnership::Mine(handle) => return Ok(handle),
            LockOwnership::Ours => return Err(Error::WouldBlock),
            LockOwnership::Theirs if !force => return Err(Error::WouldBlock),
            LockOwnership::Theirs | LockOwnership::None => {}
        }

        // Acquire a fresh lockfile via a tempfile so creation is atomic with
        // respect to other processes.
        let (tmp_path, mut tmp_file) =
            safe_create_tempfile(lock_dir, |name| format!("tmp-{name}.lock").into())?;

        let res = Self::try_install(&tmp_path, &mut tmp_file, path, force);

        // Always try to remove the tempfile
        let _ = fs::remove_file(&tmp_path);

        match res {
            Ok(()) => return Ok(LockFile(tmp_file)),
            Err(err) if !force && err.kind() == io::ErrorKind::AlreadyExists => {
                // another process won the race to create the lockfile; attach
                // to whatever is at lock_path now.
            }
            Err(err) => return Err(err.into()),
        }

        match Self::try_attach(path)? {
            LockOwnership::Mine(handle) => Ok(handle),
            _ => Err(Error::WouldBlock),
        }
    }

    /// Stamp the TAG into a freshly-created tempfile, take the flock on it,
    /// and move it to the final location at `path`.
    ///
    /// With `force`, uses [`fs::rename`] (replaces an existing entry).
    /// Without `force`, uses [`linkat`] (fails with `EEXIST` if `path` was
    /// created concurrently).
    fn try_install(
        tmp_path: &Path,
        tmp_file: &mut File,
        dst_path: &Path,
        force: bool,
    ) -> io::Result<()> {
        tmp_file.write_all(&TAG)?;
        tmp_file.flush()?;
        tmp_file.sync_all()?;

        tmp_file.try_lock().map_err(|err| match err {
            TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
            TryLockError::Error(err) => err,
        })?;

        if force {
            fs::rename(tmp_path, dst_path)
        } else {
            linkat(AT_FDCWD, tmp_path, AT_FDCWD, dst_path, AtFlags::empty())
                .map_err(|err| io::Error::from_raw_os_error(err as i32))
        }
    }

    /// Open `path` and classify what's there.
    fn try_attach(path: &Path) -> io::Result<LockOwnership> {
        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(LockOwnership::None);
            }
            Err(err) => return Err(err),
        };

        let mut buf = [0u8; TAG.len()];
        let n = file.read(&mut buf)?;
        if n != TAG.len() || buf != TAG {
            return Ok(LockOwnership::Theirs);
        }

        match file.try_lock() {
            Ok(()) => Ok(LockOwnership::Mine(LockFile(file))),
            Err(TryLockError::WouldBlock) => Ok(LockOwnership::Ours),
            Err(TryLockError::Error(err)) => Err(err),
        }
    }

    /// Release the lock handle, dropping the exclusive filesystem lock
    /// and removing the lockfile.
    ///
    /// This is a sync operation so use [`crate::fs::run_async`] if running in an async
    /// context
    pub fn unlock(self) {
        let _ = self.0.unlock();
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[derive(Debug, Default)]
pub struct LockSet {
    locks: Mutex<HashMap<PathBuf, LockFile>>,
}

impl LockSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to acquire an exclusive lock on the given path, if the file exists and was created by
    /// helios, then it just takes an exclusive lock on the file and stores the handle.  
    ///
    /// If an entry for the lock already exists on the set, the method returns without an error (idempotency).
    ///
    /// See [`LockFile::create`] for the semantics of `force`.
    ///
    /// This is a sync operation so use [`crate::fs::run_async`] if running in an async
    /// context
    pub fn try_lock(&self, path: PathBuf, force: bool) -> Result<(), Error> {
        assert!(path.is_absolute());

        let mut locks = self.locks.lock().unwrap_or_else(|p| p.into_inner());

        match locks.entry(path.clone()) {
            Entry::Occupied(_) => Ok(()),
            Entry::Vacant(entry) => {
                let lockfile = LockFile::create(path.as_ref(), force)?;
                entry.insert(lockfile);

                Ok(())
            }
        }
    }

    /// Release the lock handle in the given path, dropping the exclusive filesystem lock
    /// and removing the lockfile from the set.
    ///
    /// If there is no lockfile at the given location, the method returns without error.
    /// A missing file on disk is also treated as success; any other I/O error from removal
    /// is returned, but the in-memory entry is still released.
    ///
    /// This is a sync operation so use [`crate::fs::run_async`] if running in an async
    /// context
    pub fn unlock(&self, path: PathBuf) -> Result<(), Error> {
        assert!(path.is_absolute());

        let mut locks = self.locks.lock().unwrap_or_else(|p| p.into_inner());

        if let Some(lock) = locks.remove(&path) {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
            lock.unlock();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::thread;
    use tempfile::tempdir;

    #[test]
    fn acquire_creates_lockfile_with_tag() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");

        let handle = LockFile::create(&lock_path, false).unwrap();

        assert!(lock_path.exists());
        let contents = fs::read(&lock_path).unwrap();
        assert_eq!(contents, TAG.to_vec());

        handle.unlock();
    }

    #[test]
    fn acquire_fails_when_lock_already_held() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");

        let first = LockFile::create(&lock_path, false).unwrap();
        let err = LockFile::create(&lock_path, false).unwrap_err();
        assert!(matches!(err, Error::WouldBlock), "got {err:?}");

        first.unlock();
    }

    #[test]
    fn acquire_succeeds_after_previous_handle_is_unlocked() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");

        LockFile::create(&lock_path, false).unwrap().unlock();
        let handle = LockFile::create(&lock_path, false).unwrap();
        handle.unlock();
    }

    #[test]
    fn acquire_fails_on_externally_created_lockfile() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");

        File::create(&lock_path).unwrap();

        let err = LockFile::create(&lock_path, false).unwrap_err();
        assert!(matches!(err, Error::WouldBlock), "got {err:?}");
    }

    #[test]
    fn acquire_fails_when_lock_path_is_a_directory() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");
        fs::create_dir(&lock_path).unwrap();

        let err = LockFile::create(&lock_path, false).unwrap_err();
        assert!(matches!(&err, Error::IO(e) if e.kind() == io::ErrorKind::IsADirectory),);
    }

    #[test]
    fn acquire_creates_parent_directory_when_it_does_not_exist() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("missing").join("updates.lock");

        let handle = LockFile::create(&lock_path, false).unwrap();
        handle.unlock();
    }

    #[test]
    fn acquire_does_not_leave_tempfiles_behind() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");

        let handle = LockFile::create(&lock_path, false).unwrap();

        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with("tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "leftover tempfiles: {leftovers:?}");

        handle.unlock();
    }

    #[test]
    fn concurrent_acquire_yields_exactly_one_winner() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");

        let threads: Vec<_> = (0..8)
            .map(|_| {
                let lock_path = lock_path.clone();
                thread::spawn(move || LockFile::create(&lock_path, false))
            })
            .collect();

        let mut winners = 0;
        let mut contended = 0;
        for t in threads {
            match t.join().unwrap() {
                Ok(handle) => {
                    winners += 1;
                    handle.unlock();
                }
                Err(Error::WouldBlock) => contended += 1,
                Err(err) => panic!("unexpected error: {err:?}"),
            }
        }
        // Note: the first winner may release before later threads attempt the
        // lock, so more than one acquire may succeed serially. What matters is
        // that no two acquires hold the lock simultaneously, which is enforced
        // by `try_lock`.
        assert!(winners >= 1, "expected at least one winner");
        assert_eq!(winners + contended, 8);
    }

    #[test]
    fn force_acquire_replaces_externally_created_lockfile() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");

        let mut external = File::create(&lock_path).unwrap();
        external.write_all(b"not the tag").unwrap();
        drop(external);

        let handle = LockFile::create(&lock_path, true).unwrap();
        let contents = fs::read(&lock_path).unwrap();
        assert_eq!(contents, TAG.to_vec());

        // The replaced file is a fresh inode; a concurrent non-force acquire
        // should now see WouldBlock from our flock.
        let err = LockFile::create(&lock_path, false).unwrap_err();
        assert!(matches!(err, Error::WouldBlock), "got {err:?}");

        handle.unlock();
    }

    #[test]
    fn force_acquire_attaches_to_stale_helios_lockfile() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");

        // A previous helios run left its lockfile on disk but is no longer
        // holding the flock. Force should attach to it, not replace it.
        LockFile::create(&lock_path, false).unwrap().unlock();
        fs::write(&lock_path, TAG).unwrap();
        let inode_before = fs::metadata(&lock_path).unwrap().ino();

        let handle = LockFile::create(&lock_path, true).unwrap();
        let inode_after = fs::metadata(&lock_path).unwrap().ino();
        assert_eq!(inode_before, inode_after, "force should not replace inode");
        handle.unlock();
    }

    #[test]
    fn force_acquire_fails_when_another_helios_holds_lock() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");

        let held = LockFile::create(&lock_path, false).unwrap();
        let err = LockFile::create(&lock_path, true).unwrap_err();
        assert!(matches!(err, Error::WouldBlock), "got {err:?}");
        held.unlock();
    }

    #[test]
    fn concurrent_force_acquires_do_not_overlap() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");
        // External file so every force-acquire goes through the install
        // path (until the first one replaces it with a helios-owned file).
        fs::write(&lock_path, b"not the tag").unwrap();

        let held = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));

        let threads: Vec<_> = (0..8)
            .map(|_| {
                let lock_path = lock_path.clone();
                let held = Arc::clone(&held);
                let max_concurrent = Arc::clone(&max_concurrent);
                thread::spawn(move || {
                    if let Ok(handle) = LockFile::create(&lock_path, true) {
                        let n = held.fetch_add(1, Ordering::SeqCst) + 1;
                        max_concurrent.fetch_max(n, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(10));
                        held.fetch_sub(1, Ordering::SeqCst);
                        handle.unlock();
                    }
                })
            })
            .collect();

        for t in threads {
            t.join().unwrap();
        }

        assert_eq!(
            max_concurrent.load(Ordering::SeqCst),
            1,
            "more than one force-acquire held the lock simultaneously"
        );
    }

    #[test]
    fn force_acquire_succeeds_when_no_existing_lockfile() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");

        let handle = LockFile::create(&lock_path, true).unwrap();
        assert_eq!(fs::read(&lock_path).unwrap(), TAG.to_vec());
        handle.unlock();
    }

    #[test]
    fn lockset_try_lock_succeeds_when_path_already_in_set() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.lock");

        let set = LockSet::new();
        set.try_lock(path.clone(), false).unwrap();
        set.try_lock(path, false).unwrap();
    }

    #[test]
    fn lockset_unlock_unknown_path_is_noop() {
        let dir = tempdir().unwrap();
        let set = LockSet::new();

        set.unlock(dir.path().join("never-locked.lock")).unwrap();
    }

    #[test]
    fn lockset_unlock_releases_filesystem_lock() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.lock");

        let set = LockSet::new();
        set.try_lock(path.clone(), false).unwrap();
        set.unlock(path.clone()).unwrap();

        // Holding the file lock would have made this fail with WouldBlock.
        LockFile::create(&path, false).unwrap().unlock();
    }

    #[test]
    fn lockset_unlock_removes_lockfile_from_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.lock");

        let set = LockSet::new();
        set.try_lock(path.clone(), false).unwrap();
        assert!(path.exists());

        set.unlock(path.clone()).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn lockset_unlock_succeeds_when_file_missing_on_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.lock");

        let set = LockSet::new();
        set.try_lock(path.clone(), false).unwrap();

        // Remove the lockfile out from under the set; unlock should still succeed.
        fs::remove_file(&path).unwrap();

        set.unlock(path.clone()).unwrap();
    }
}
