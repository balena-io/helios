use std::any;
use std::fmt;
use std::io;
use std::path::Path;

use super::handle::FileHandle;
use super::{Context, Hooks};

#[derive(Debug)]
pub(crate) struct Blob<T> {
    context: Context<T>,
    handle: FileHandle,
    size: u64,
}

impl<T> Blob<T> {
    pub fn new(context: Context<T>, handle: FileHandle, size: u64) -> Self {
        Self {
            context,
            handle,
            size,
        }
    }

    pub fn size(&self) -> u64 {
        self.size
    }
}

pub struct CommitError<T> {
    pub inner: Box<T>,
    pub source: io::Error,
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

type CommitResult<T> = Result<u64, CommitError<Blob<T>>>;

impl<T: Hooks> Blob<T> {
    pub fn commit(self, path: &Path) -> CommitResult<T> {
        match self.context.commit_file(&self.handle, path) {
            Ok(_) => Ok(self.size),
            Err(err) => Err(CommitError {
                inner: self.into(),
                source: err,
            }),
        }
    }
}
