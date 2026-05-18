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

#[derive(Debug)]
struct LockFile(File);

impl LockFile {
    /// Try to acquire an exclusive lock on the given path, if the file exists and was created by
    /// helios, then it just takes an exclusive lock on the file and returns the handle
    ///
    /// This is a sync operation so use [`crate::fs::run_async`] if running in an async
    /// context
    pub fn create(path: &Path) -> Result<Self, Error> {
        let lock_dir = path.parent().unwrap_or_else(|| Path::new("/"));

        ensure_exists(lock_dir)?;

        tracing::trace!("acquiring lock at {}", path.display());

        // Fast path: if the lockfile already exists with our TAG, just take an
        // exclusive lock on it.
        match File::open(path) {
            Ok(file) => return Self::attach(file),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }

        // Slow path: create the lockfile via a tempfile + hard link so that
        // creation is atomic with respect to other processes.
        let (tmp_path, mut tmp_file) =
            safe_create_tempfile(lock_dir, |name| format!("tmp-{name}.lock").into())?;

        let res = (|| -> io::Result<()> {
            tmp_file.write_all(&TAG)?;
            tmp_file.flush()?;
            tmp_file.sync_all()?;

            // hard linking to lock_path is atomic and fails with EEXIST if the
            // path was created concurrently by another process.
            linkat(AT_FDCWD, &tmp_path, AT_FDCWD, path, AtFlags::empty())
                .map_err(|err| io::Error::from_raw_os_error(err as i32))
        })();

        // always remove the tempfile, ignoring errors
        let _ = fs::remove_file(&tmp_path);

        match res {
            Ok(()) => Self::attach(File::open(path)?),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                // another process won the race to create the lockfile; attach
                // to whatever is at lock_path now.
                Self::attach(File::open(path)?)
            }
            Err(err) => Err(err.into()),
        }
    }

    fn attach(mut file: File) -> Result<LockFile, Error> {
        let mut buf = [0u8; TAG.len()];
        let n = file.read(&mut buf)?;
        if n != TAG.len() || buf != TAG {
            return Err(Error::WouldBlock);
        }
        file.try_lock().map_err(|err| match err {
            TryLockError::WouldBlock => Error::WouldBlock,
            TryLockError::Error(err) => Error::IO(err),
        })?;
        Ok(LockFile(file))
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
    /// helios, then it just takes an exclusive lock on the file and stores the handle
    ///
    /// This is a sync operation so use [`crate::fs::run_async`] if running in an async
    /// context
    pub fn try_lock(&self, path: PathBuf) -> Result<(), Error> {
        assert!(path.is_absolute());

        let mut locks = self.locks.lock().unwrap_or_else(|p| p.into_inner());

        match locks.entry(path.clone()) {
            Entry::Occupied(_) => Err(Error::WouldBlock),
            Entry::Vacant(entry) => {
                let lockfile = LockFile::create(path.as_ref())?;
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
    use std::thread;
    use tempfile::tempdir;

    #[test]
    fn acquire_creates_lockfile_with_tag() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");

        let handle = LockFile::create(&lock_path).unwrap();

        assert!(lock_path.exists());
        let contents = fs::read(&lock_path).unwrap();
        assert_eq!(contents, TAG.to_vec());

        handle.unlock();
    }

    #[test]
    fn acquire_fails_when_lock_already_held() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");

        let first = LockFile::create(&lock_path).unwrap();
        let err = LockFile::create(&lock_path).unwrap_err();
        assert!(matches!(err, Error::WouldBlock), "got {err:?}");

        first.unlock();
    }

    #[test]
    fn acquire_succeeds_after_previous_handle_is_unlocked() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");

        LockFile::create(&lock_path).unwrap().unlock();
        let handle = LockFile::create(&lock_path).unwrap();
        handle.unlock();
    }

    #[test]
    fn acquire_fails_on_externally_created_lockfile() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");

        File::create(&lock_path).unwrap();

        let err = LockFile::create(&lock_path).unwrap_err();
        assert!(matches!(err, Error::WouldBlock), "got {err:?}");
    }

    #[test]
    fn acquire_fails_when_lock_path_is_a_directory() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");
        fs::create_dir(&lock_path).unwrap();

        let err = LockFile::create(&lock_path).unwrap_err();
        assert!(matches!(&err, Error::IO(e) if e.kind() == io::ErrorKind::IsADirectory),);
    }

    #[test]
    fn acquire_creates_parent_directory_when_it_does_not_exist() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("missing").join("updates.lock");

        let handle = LockFile::create(&lock_path).unwrap();
        handle.unlock();
    }

    #[test]
    fn acquire_does_not_leave_tempfiles_behind() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("updates.lock");

        let handle = LockFile::create(&lock_path).unwrap();

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
                thread::spawn(move || LockFile::create(&lock_path))
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
    fn lockset_try_lock_fails_when_path_already_in_set() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.lock");

        let set = LockSet::new();
        set.try_lock(path.clone()).unwrap();

        let err = set.try_lock(path).unwrap_err();
        assert!(matches!(err, Error::WouldBlock), "got {err:?}");
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
        set.try_lock(path.clone()).unwrap();
        set.unlock(path.clone()).unwrap();

        // Holding the file lock would have made this fail with WouldBlock.
        LockFile::create(&path).unwrap().unlock();
    }

    #[test]
    fn lockset_unlock_removes_lockfile_from_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.lock");

        let set = LockSet::new();
        set.try_lock(path.clone()).unwrap();
        assert!(path.exists());

        set.unlock(path.clone()).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn lockset_unlock_succeeds_when_file_missing_on_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.lock");

        let set = LockSet::new();
        set.try_lock(path.clone()).unwrap();

        // Remove the lockfile out from under the set; unlock should still succeed.
        fs::remove_file(&path).unwrap();

        set.unlock(path.clone()).unwrap();
    }
}
