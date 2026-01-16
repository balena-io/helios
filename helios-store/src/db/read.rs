use std::io;

use super::Hooks;
use super::handle::FileHandle;

#[derive(Debug)]
pub(crate) struct StoredFile<'db, T> {
    handle: FileHandle<'db, T>,
    _lock: FileHandle<'db, T>, // just need to keep it alive
}

impl<'db, T: Hooks> StoredFile<'db, T> {
    pub fn new(handle: FileHandle<'db, T>, lock: FileHandle<'db, T>) -> Self {
        Self {
            handle,
            _lock: lock,
        }
    }
}

impl<T> io::Read for StoredFile<'_, T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.handle.file.read(buf)
    }

    fn read_vectored(&mut self, bufs: &mut [io::IoSliceMut<'_>]) -> io::Result<usize> {
        self.handle.file.read_vectored(bufs)
    }
}

impl<T> io::Seek for StoredFile<'_, T> {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.handle.file.seek(pos)
    }

    fn stream_position(&mut self) -> io::Result<u64> {
        self.handle.file.stream_position()
    }
}
