use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use crate::util::crypto::sha256_hex_digest;
use crate::util::fs as fsutil;
use crate::would_block;

const FLAG_TEMPFILE: u32 = 1;
const FLAG_LOCKFILE: u32 = 1 << 1;
const FLAG_LOCKEXCL: u32 = 1 << 2;

/// A flexible [File] wrapper that abstracts away the location of a file into
/// a file store, as well as platform differences around file locking.
#[derive(Debug)]
pub(crate) struct FileHandle {
    /// Absolute path to the file.
    pub path: PathBuf,

    /// The open file descriptor.
    pub file: File,

    // this u32 is practically free due to alignment -- space that would
    // otherwise be occupied by zeros. In these 32 bits we store our 3 bits
    // worth of flags on the lower bits.
    flags: u32,
}

impl FileHandle {
    /// Create a file handle from the path with a shared lock
    pub fn shared(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).open(path)?;

        // try to lock the file
        match file.try_lock_shared() {
            Ok(_) => Ok(()),
            Err(TryLockError::Error(e)) => Err(e),
            Err(_) => Err(would_block()),
        }?;

        Ok(Self {
            file,
            path: path.to_path_buf(),
            flags: FLAG_LOCKFILE,
        })
    }

    /// Create a file handle from the path with an exclusive lock
    pub fn exclusive(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        // try to lock the file
        match file.try_lock() {
            Ok(_) => Ok(()),
            Err(TryLockError::Error(e)) => Err(e),
            Err(_) => Err(would_block()),
        }?;

        Ok(Self {
            file,
            path: path.to_path_buf(),
            flags: FLAG_LOCKFILE | FLAG_LOCKEXCL,
        })
    }

    pub fn tempfile(base: &Path) -> io::Result<Self> {
        let (path, file) =
            fsutil::safe_create_tempfile(base, |name| sha256_hex_digest(name.as_bytes()).into())?;
        assert!(path.is_absolute(), "path must be absolute: {:?}", path);

        // try to lock the file
        match file.try_lock() {
            Ok(_) => Ok(()),
            Err(TryLockError::Error(e)) => Err(e),
            Err(_) => Err(would_block()),
        }?;

        Ok(Self {
            path,
            file,
            flags: FLAG_TEMPFILE | FLAG_LOCKEXCL,
        })
    }
}

impl Drop for FileHandle {
    fn drop(&mut self) {
        if self.flags & FLAG_TEMPFILE != 0 {
            _ = fs::remove_file(&self.path);
        }
    }
}
