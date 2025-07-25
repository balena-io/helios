use fastrand::Rng;
use sha2::{Digest as _, Sha256};
use std::iter::repeat_with;

pub fn sha256_hex_digest<D: AsRef<[u8]>>(data: D) -> String {
    let mut hasher = Sha256::default();
    hasher.update(data.as_ref());
    let digest = hasher.finalize();
    format!("{:x}", digest)
}

pub const ALPHA_NUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
// pub const LC_ALPHA_NUM: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
// pub const UC_ALPHA_NUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

pub fn pseudorandom_string<C: AsRef<[u8]>>(charset: C, len: usize) -> String {
    let mut rng = Rng::new();
    repeat_with(|| {
        let index = rng.usize(..charset.as_ref().len());
        charset.as_ref()[index] as char
    })
    .take(len)
    .collect()
}
