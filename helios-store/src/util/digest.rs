//! Utilities for computing and verifying cryptographic digests.
//!
//! This module provides a flexible framework for working with digests, commonly
//! known as hashes. It is designed to be extensible with different hashing
//! algorithms while providing a consistent API.
//!
//! The core components are:
//!
//! - [`Digest`]: A representation of a digest, combining an algorithm and a
//!   hex-encoded value (e.g., `sha256:deadbeef...`).
//! - [`Hasher`]: A trait for hash algorithms, with [`Sha256`] provided as a
//!   concrete implementation.
//! - [`HashStream`]: A wrapper that computes a digest as data is read from or
//!   written to an underlying stream.
//! - [`VerifyStream`]: A wrapper that verifies data against a known digest as it
//!   is read from a stream.

use std::fmt;
use std::io;
use std::pin::Pin;
use std::str::FromStr;
use std::task::{Context, Poll, ready};

use pin_project_lite::pin_project;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::hex;

/// An error that can occur when working with digests.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The digest algorithm is not supported.
    #[error("unsupported digest algo: {algo}")]
    UnsupportedAlgo { algo: String },

    /// The digest string is not in the correct format.
    #[error("invalid digest format: expected = '<algo>:<value>', actual = '{value}'")]
    InvalidFormat { value: String },

    /// The digest of the data does not match the expected digest.
    #[error("digest mismatch: expected = '{expected}', actual = '{received}'")]
    Mismatch { expected: Digest, received: Digest },
}

impl From<Error> for io::Error {
    fn from(value: Error) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, value)
    }
}

/// A hex-encoded digest value and the algorithm used to generate it.
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest {
    /// The algorithm used to generate the digest.
    pub algo: Algo,
    /// The hex-encoded value of the digest.
    pub value: String,
}

// Note: The `Digest` struct is serialized and deserialized as a string in the
// format `<algo>:<value>`, making it easy to use in JSON and other text-based formats.

impl Digest {
    /// Consumes the [`Digest`] and returns the algorithm name and the hex-encoded value.
    #[inline]
    pub fn into_inner(self) -> (&'static str, String) {
        (self.algo.name(), self.value)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algo, self.value)
    }
}

impl FromStr for Digest {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((algo, value)) = s.split_once(":") else {
            return Err(Error::InvalidFormat {
                value: s.to_owned(),
            });
        };

        match algo {
            "sha256" => Ok(Self {
                algo: Algo::Sha256,
                value: value.to_owned(),
            }),
            algo => Err(Error::UnsupportedAlgo {
                algo: algo.to_owned(),
            }),
        }
    }
}

impl TryFrom<String> for Digest {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Digest> for String {
    fn from(value: Digest) -> Self {
        value.to_string()
    }
}

/// The algorithm used to generate a digest.
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Algo {
    /// The SHA-256 algorithm.
    #[serde(rename = "sha256")]
    Sha256,
}

impl fmt::Display for Algo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.name().fmt(f)
    }
}

impl Algo {
    /// Returns a new [`Hasher`] for this algorithm.
    pub fn hasher(&self) -> AnyHasher {
        match self {
            Self::Sha256 => Box::new(Sha256::new()),
        }
    }

    /// Returns the name of the algorithm.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
        }
    }
}

/// A trait for hashing data.
pub trait Hasher {
    /// Returns the algorithm used by this hasher.
    fn algo(&self) -> Algo;

    /// Returns the output size of the hash in bytes.
    fn output_size(&self) -> usize;

    /// Updates the hasher with the given data.
    fn update(&mut self, data: &[u8]);

    /// Finalizes the hash and returns the raw digest.
    fn finalize(&mut self) -> &[u8];

    /// Finalizes the hash and returns a [`Digest`].
    fn digest(&mut self) -> Digest {
        let algo = self.algo();
        let digest = self.finalize();
        Digest {
            algo,
            value: hex::encode(digest),
        }
    }

    /// Computes the hex-encoded digest of the given data.
    fn hex_digest(data: &[u8]) -> Digest
    where
        Self: Default,
    {
        let mut hasher = Self::default();
        hasher.update(data);
        hasher.digest()
    }
}

/// A type-erased [`Hasher`].
pub type AnyHasher = Box<dyn Hasher>;

/// A [`Hasher`] for the SHA-256 algorithm.
#[derive(Debug, Default)]
pub struct Sha256 {
    inner: sha2::Sha256,
    buf: [u8; 32],
}

impl Sha256 {
    /// Creates a new `Sha256` hasher.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Hasher for Sha256 {
    fn algo(&self) -> Algo {
        Algo::Sha256
    }

    fn output_size(&self) -> usize {
        32
    }

    fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    fn finalize(&mut self) -> &[u8] {
        <sha2::Sha256 as sha2::digest::DynDigest>::finalize_into_reset(
            &mut self.inner,
            &mut self.buf,
        )
        .expect("buffer length should equal hash length");

        &self.buf
    }
}

/// Computes the SHA-256 hex-encoded digest of the given data.
pub fn sha256_hex_digest(data: &[u8]) -> Digest {
    Sha256::hex_digest(data)
}

pin_project! {
    /// A stream that hashes the data as it passes through.
    #[derive(Debug)]
    pub struct HashStream<H: Hasher, S> {
        hasher: H,

        #[pin]
        stream: S,
    }
}

impl<H: Hasher, S> HashStream<H, S> {
    /// Creates a new `HashStream`.
    pub fn new(hasher: H, stream: S) -> Self {
        Self { hasher, stream }
    }

    /// Consumes the stream and returns the computed [`Digest`].
    pub fn digest(mut self) -> Digest {
        self.hasher.digest()
    }
}

impl<H: Hasher, R: io::Read> io::Read for HashStream<H, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.stream.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

impl<H: Hasher, W: io::Write> io::Write for HashStream<H, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.stream.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        let n = self.stream.write_vectored(bufs)?;

        let mut remaining = n;
        for buf in bufs {
            let end = remaining.min(buf.len());
            self.hasher.update(&buf[..end]);

            remaining -= end;
            if remaining == 0 {
                break;
            }
        }

        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

impl<H: Hasher, R: AsyncRead> AsyncRead for HashStream<H, R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.project();
        ready!(this.stream.poll_read(cx, buf))?;
        this.hasher.update(buf.filled());
        Poll::Ready(Ok(()))
    }
}

impl<H: Hasher, W: AsyncWrite> AsyncWrite for HashStream<H, W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.project();
        let n = ready!(this.stream.poll_write(cx, buf))?;
        this.hasher.update(&buf[..n]);
        Poll::Ready(Ok(n))
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.project();
        let n = ready!(this.stream.poll_write_vectored(cx, bufs))?;

        let mut remaining = n;
        for buf in bufs {
            let end = remaining.min(buf.len());
            this.hasher.update(&buf[..end]);
            remaining -= end;
            if remaining == 0 {
                break;
            }
        }

        Poll::Ready(Ok(n))
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.project().stream.poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.project().stream.poll_shutdown(cx)
    }
}
