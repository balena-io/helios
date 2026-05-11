use std::fmt::Write as _;

use sha2::{Digest as _, Sha256};

pub fn sha256_hex_digest<D: AsRef<[u8]>>(data: D) -> String {
    let mut hasher = Sha256::default();
    hasher.update(data.as_ref());
    let digest = hasher.finalize();
    digest
        .iter()
        .fold(String::with_capacity(digest.len() * 2), |mut s, b| {
            write!(s, "{b:02x}").unwrap();
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // These vectors are standard SHA-256 outputs and must remain stable —
    // helios-store paths and helios-remote provisioning IDs are derived from
    // them, so any change here would invalidate on-disk state on existing
    // devices.
    #[test]
    fn sha256_hex_digest_matches_known_vectors() {
        assert_eq!(
            sha256_hex_digest(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex_digest("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex_digest("hello world"),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        assert_eq!(
            sha256_hex_digest("https://api.balena-cloud.com"),
            "6fd309b6db563863d07e75805cd2f98c1a7ca157287b902a223b21a5295f1a58"
        );
    }

    #[test]
    fn sha256_hex_digest_output_is_64_lowercase_hex_chars() {
        let digest = sha256_hex_digest([0u8; 1024]);
        assert_eq!(digest.len(), 64);
        assert!(
            digest
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }
}
