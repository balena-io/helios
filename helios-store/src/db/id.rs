use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use crate::Error;

/// Uniquely identifies a file stored into a file store.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct FileId(PathBuf);

impl FileId {
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_os_str().as_encoded_bytes()
    }

    pub fn validate_path(path: &Path) -> Result<(), Error> {
        if path.is_absolute() {
            return Err(Error::InvalidPath {
                path: path.to_path_buf(),
                reason: "path must be relative to store root".to_string(),
            });
        }

        for c in path.components() {
            if !matches!(c, Component::Normal(_)) {
                return Err(Error::InvalidPath {
                    path: path.to_path_buf(),
                    reason: "path contains non-normal components".to_string(),
                });
            }
        }

        Ok(())
    }
}

impl FromStr for FileId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(Path::new(s))
    }
}

impl TryFrom<&Path> for FileId {
    type Error = Error;

    fn try_from(value: &Path) -> Result<Self, Self::Error> {
        Self::validate_path(value)?;
        Ok(Self(value.to_path_buf()))
    }
}

impl AsRef<Path> for FileId {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl From<FileId> for PathBuf {
    fn from(value: FileId) -> Self {
        value.0
    }
}

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(f)
    }
}
