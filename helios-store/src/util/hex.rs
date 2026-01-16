//! Utilities for encoding byte slices into hexadecimal strings.
//!
//! This module provides functions for converting byte slices into their
//! hexadecimal string representations.

use std::fmt;

/// Returns the hex-encoded string representation of the input bytes.
///
/// ```rust,ignore
/// use helios_store::util::hex;
///
/// let bytes = &[0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe];
/// let str = hex::encode(bytes);
///
/// assert_eq!(str, "deadbeefcafebabe");
/// ```
#[inline]
pub fn encode(bytes: impl AsRef<[u8]>) -> String {
    let mut out = String::with_capacity(bytes.as_ref().len() * 2);
    encode_into(&mut out, bytes).unwrap(); // meets capacity constraint
    out
}

/// Writes the hex-encoded string representation of the input bytes into the
/// output buffer.
///
/// Returns a [`fmt::Error`] if the underlying writer returns an error.
///
/// ```rust,ignore
/// use helios_store::util::hex;
///
/// let bytes = &[0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe];
/// let mut str = String::with_capacity(bytes.as_ref().len() * 2);
/// hex::encode_into(&mut str, bytes).unwrap();
///
/// assert_eq!(str, "deadbeefcafebabe");
/// ```
#[inline]
pub fn encode_into<W: fmt::Write>(mut out: W, bytes: impl AsRef<[u8]>) -> fmt::Result {
    for byte in bytes.as_ref() {
        write!(out, "{byte:02x}")?;
    }
    Ok(())
}
