use std::io::{self, Write as _};

#[cfg(test)]
use std::path::Path;

use super::Hooks;
use super::handle::FileHandle;
use super::write::Blob;

#[derive(Debug)]
pub(crate) struct StagedFile<'db, T> {
    handle: FileHandle<'db, T>,
}

impl<'db, T: Hooks> StagedFile<'db, T> {
    pub fn new(handle: FileHandle<'db, T>) -> Self {
        Self { handle }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.handle.path
    }

    pub fn write_all<R: io::Read>(mut self, stream: R) -> io::Result<Blob<'db, T>> {
        let handle = &mut self.handle;
        let mut hasher = handle.db.hasher(stream);

        let size = io::copy(&mut hasher, &mut handle.file)?;
        let digest = hasher.digest();

        handle.file.flush()?;
        handle.file.sync_all()?;

        Ok(Blob::new(self.handle, digest, size))
    }
}
