use std::fs;
use std::io;

use super::handle::FileHandle;

#[derive(Debug)]
pub(crate) struct StoredFile {
    pub(crate) metadata: fs::Metadata,
    handle: FileHandle,
}

impl StoredFile {
    pub fn new(handle: FileHandle, metadata: fs::Metadata) -> Self {
        Self { handle, metadata }
    }
}

impl io::Read for StoredFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.handle.file.read(buf)
    }

    fn read_vectored(&mut self, bufs: &mut [io::IoSliceMut<'_>]) -> io::Result<usize> {
        self.handle.file.read_vectored(bufs)
    }
}

impl io::Seek for StoredFile {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.handle.file.seek(pos)
    }

    fn stream_position(&mut self) -> io::Result<u64> {
        self.handle.file.stream_position()
    }
}
