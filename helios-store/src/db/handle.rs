use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::util::digest::sha256_hex_digest;
use crate::util::fs as fsutil;

use super::FileDb;
use super::id::FileId;

const FLAG_NONE: u32 = 0;
const FLAG_TEMPFILE: u32 = 1;
const FLAG_LOCKFILE: u32 = 1 << 1;
const FLAG_LOCKEXCL: u32 = 1 << 2;

/// A flexible [File] wrapper that abstracts away the location of a file into
/// a file store, as well as platform differences around file locking.
#[derive(Debug)]
pub(crate) struct FileHandle<'db, T> {
    /// A reference to the store this handle originates from.
    pub db: &'db FileDb<T>,

    /// Absolute path to the file.
    pub path: PathBuf,

    /// The open file descriptor.
    pub file: File,

    // this u32 is practically free due to alignment -- space that would
    // otherwise be occupied by zeros. In these 32 bits we store our 3 bits
    // worth of flags on the lower bits.
    flags: u32,
}

impl<'db, T> FileHandle<'db, T> {
    pub fn new(db: &'db FileDb<T>, path: PathBuf, file: File) -> Self {
        assert!(path.is_absolute(), "path must be absolute: {:?}", path);
        Self {
            db,
            path,
            file,
            flags: FLAG_NONE,
        }
    }

    pub fn tempfile(db: &'db FileDb<T>, base: &Path) -> io::Result<Self> {
        let (path, file) = fsutil::safe_create_tempfile(base, |name| {
            sha256_hex_digest(name.as_bytes()).value.into()
        })?;
        assert!(path.is_absolute(), "path must be absolute: {:?}", path);

        Ok(Self {
            db,
            path,
            file,
            flags: FLAG_TEMPFILE,
        })
    }

    const GLOBAL_LOCK_NAME: &'static str = "root.lock";

    pub fn lockfile(
        db: &'db FileDb<T>,
        base: &Path,
        id: Option<FileId>,
        excl: bool,
    ) -> io::Result<Self> {
        let digest = sha256_hex_digest(
            id.as_ref()
                .map(|id| id.as_bytes())
                .unwrap_or(Self::GLOBAL_LOCK_NAME.as_bytes()),
        );
        let path = base.join(&digest.value);
        assert!(path.is_absolute(), "path must be absolute: {:?}", path);

        let file = db.lock(path.to_path_buf(), excl)?;

        Ok(Self {
            db,
            path,
            file,
            flags: FLAG_LOCKFILE | if excl { FLAG_LOCKEXCL } else { FLAG_NONE },
        })
    }
}

impl<T> Drop for FileHandle<'_, T> {
    fn drop(&mut self) {
        if self.flags & FLAG_TEMPFILE != 0 {
            _ = fs::remove_file(&self.path);
        } else if self.flags & FLAG_LOCKFILE != 0 {
            let excl = self.flags & FLAG_LOCKEXCL != 0;
            _ = self.db.unlock(self.path.to_path_buf(), excl);
        }
    }
}
