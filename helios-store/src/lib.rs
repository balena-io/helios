//! path-based document storage.
//!
//! This crate provides a `DocumentStore` storage backend which is
//! A key-value store where keys are filesystem paths. It's suitable
//! for configuration files and database-like resources.
//!
//! The store is built on a filesystem-based database and are designed to be
//! safe for concurrent access from both threads and processes.
//!
//! # Example
//!
//! ```
//! use serde_json::json;
//! use tempfile::tempdir;
//! use helios_store::DocumentStore;
//!
//! let dir = tempdir().unwrap();
//! let store = DocumentStore::create_with_root(dir.path()).unwrap();
//! store.put("my/document", &json!({ "hello": "world" })).unwrap();
//! ```

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use tracing::trace;

mod util;

mod db;
pub use db::{CommitError, Digest};

mod view;
pub use view::View;

mod document;
pub use document::*;

mod async_store;
pub use async_store::{AsyncDocumentStore, AsyncView};

/// The error type for operations within this crate.
#[derive(Debug)]
pub enum Error {
    /// An I/O error occurred, potentially while working on a user-specified path.
    Io {
        path: Option<PathBuf>,
        source: io::Error,
    },

    /// A user-specified path was not found.
    /// All `io::Error`s of kind `NotFound` are converted to this one.
    NotFound { path: PathBuf },

    /// An invalid path was provided.
    InvalidPath { path: PathBuf, reason: String },

    /// An invalid filter pattern was provided for listing files.
    InvalidFilter { filter: String, reason: String },

    /// A serialization error while reading or writing a document
    Serialization {
        line: usize,
        column: usize,
        reason: String,
    },
}

impl std::error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { source, .. } => source.fmt(f),
            Self::NotFound { .. } => "file not found".fmt(f),
            Self::InvalidPath { reason, .. } => reason.fmt(f),
            Self::InvalidFilter { reason, .. } => reason.fmt(f),
            Self::Serialization { reason, .. } => reason.fmt(f),
        }
    }
}

impl Error {
    #[inline]
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }

    fn with_path(path: &Path) -> impl Fn(io::Error) -> Self {
        |err| match err.kind() {
            io::ErrorKind::NotFound => Self::NotFound {
                path: path.to_path_buf(),
            },
            _ => Self::Io {
                path: Some(path.to_path_buf()),
                source: err,
            },
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Serialization {
            line: value.line(),
            column: value.column(),
            reason: value.to_string(),
        }
    }
}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io {
            path: None,
            source: value,
        }
    }
}

/// A specialized [`Result`] type for this crate's operations.
pub type Result<T> = std::result::Result<T, Error>;

fn would_block() -> io::Error {
    io::Error::from(io::ErrorKind::WouldBlock)
}

fn trace_ev(name: &Path, event: &'static str) {
    trace!(
        db.name = name.parent().unwrap().file_name().unwrap().to_str(),
        db.event = event
    );
}

fn trace_ev_detail(name: &Path, event: &'static str, detail: &str) {
    trace!(
        db.name = name.parent().unwrap().file_name().unwrap().to_str(),
        db.event = event,
        db.detail = detail
    );
}

fn trace_ev_path(name: &Path, event: &'static str, path: &Path) {
    trace!(
        db.name = name.parent().unwrap().file_name().unwrap().to_str(),
        db.event = event,
        db.path = path.as_os_str().to_str()
    );
}
