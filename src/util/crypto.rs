use sha2::{Digest as _, Sha256};

pub fn sha256_hex_digest<D: AsRef<[u8]>>(data: D) -> String {
    let mut hasher = Sha256::default();
    hasher.update(data.as_ref());
    let digest = hasher.finalize();
    format!("{digest:x}")
}
