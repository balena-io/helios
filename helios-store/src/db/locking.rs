use std::collections::{HashMap, hash_map::Entry};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::would_block;

// FIXME: cleanup lockfiles
#[derive(Debug)]
pub(crate) struct LockSet {
    inner: Mutex<LockSetInner>,
}

impl LockSet {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            inner: Mutex::new(LockSetInner::default()),
        })
    }

    pub fn lock(&self, path: PathBuf, excl: bool) -> io::Result<File> {
        debug_assert!(path.is_absolute());

        let mut inner = match self.inner.try_lock() {
            Ok(inner) => inner,
            Err(_) => return Err(would_block()),
        };

        if inner.excl_locks.contains_key(&path) {
            return Err(would_block());
        }

        let lockset = if excl {
            &mut inner.excl_locks
        } else {
            &mut inner.shared_locks
        };

        match lockset.entry(path) {
            Entry::Occupied(mut entry) => {
                let lock = entry.get_mut();
                let lockfile = lock.file.try_clone()?;
                lock.refcnt += 1;
                Ok(lockfile)
            }
            Entry::Vacant(entry) => {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(entry.key())?;

                match if excl {
                    file.try_lock()
                } else {
                    file.try_lock_shared()
                } {
                    Ok(()) => {}
                    Err(fs::TryLockError::WouldBlock) => return Err(would_block()),
                    Err(fs::TryLockError::Error(err)) => return Err(err),
                }

                let lockfile = file.try_clone()?;
                entry.insert(Lock { refcnt: 1, file });
                Ok(lockfile)
            }
        }
    }

    pub fn unlock(&self, path: PathBuf, excl: bool) -> io::Result<()> {
        debug_assert!(path.is_absolute());

        let mut inner = match self.inner.try_lock() {
            Ok(inner) => inner,
            Err(_) => return Err(would_block()),
        };

        let lockset = if excl {
            &mut inner.excl_locks
        } else {
            &mut inner.shared_locks
        };

        let should_delete = match lockset.get_mut(&path) {
            Some(lock) => {
                lock.refcnt -= 1;
                if lock.refcnt == 0 {
                    lock.file.unlock()?;
                    true
                } else {
                    false
                }
            }
            None => panic!("unbalanced file lock/unlock"),
        };

        if should_delete {
            lockset.remove(&path);
        }

        Ok(())
    }
}

#[derive(Debug, Default)]
struct LockSetInner {
    shared_locks: HashMap<PathBuf, Lock>,
    excl_locks: HashMap<PathBuf, Lock>,
}

#[derive(Debug)]
struct Lock {
    refcnt: usize,
    file: File,
}
