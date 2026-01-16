use std::any;
use std::fmt;
use std::path::Path;

use crate::util::digest::Digest;

use crate::Error;

use super::Hooks;
use super::handle::FileHandle;

#[derive(Debug)]
pub(crate) struct Blob<'db, T> {
    handle: FileHandle<'db, T>,
    digest: Digest,
    size: u64,
}

impl<'db, T: Hooks> Blob<'db, T> {
    pub fn new(handle: FileHandle<'db, T>, digest: Digest, size: u64) -> Self {
        Self {
            handle,
            digest,
            size,
        }
    }

    pub fn digest(&self) -> &Digest {
        &self.digest
    }

    pub fn size(&self) -> u64 {
        self.size
    }
}

pub struct CommitError<T> {
    pub inner: Box<T>,
    pub source: Error,
}

impl<T> std::error::Error for CommitError<T> {}

impl<T> fmt::Display for CommitError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(f)
    }
}

impl<T> fmt::Debug for CommitError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommitError")
            .field("inner", &any::type_name::<T>())
            .field("source", &self.source)
            .finish()
    }
}

type CommitResult<'a, T> = Result<(Digest, u64), CommitError<Blob<'a, T>>>;

impl<'db, T: Hooks> Blob<'db, T> {
    pub fn commit(self, id: &Path) -> CommitResult<'db, T> {
        let handle = &self.handle;

        match handle.db.commit_file(handle, id) {
            Ok(_) => Ok((self.digest, self.size)),
            Err(err) => Err(CommitError {
                inner: self.into(),
                source: err,
            }),
        }
    }
}
