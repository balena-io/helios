use std::io::{self, Write as _};

#[cfg(test)]
use std::path::Path;

use super::handle::FileHandle;
use super::write::Blob;
use super::{Context, Hooks};

#[derive(Debug)]
pub(crate) struct StagedFile<T> {
    context: Context<T>,
    handle: FileHandle,
}

impl<T: Hooks> StagedFile<T> {
    pub fn new(context: Context<T>, handle: FileHandle) -> Self {
        Self { context, handle }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.handle.path
    }

    pub fn write_all<R: io::Read>(mut self, mut stream: R) -> io::Result<Blob<T>> {
        let handle = &mut self.handle;
        let size = io::copy(&mut stream, &mut handle.file)?;

        handle.file.flush()?;
        handle.file.sync_all()?;

        Ok(Blob::new(self.context, self.handle, size))
    }
}
