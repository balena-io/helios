use parse_size::Config;
use serde::{Deserialize, Deserializer};

/// Compose/Docker byte-value shapes accepted from the remote backend: a byte
/// string such as `"1g"`, `"512mb"`, or `"1.5GiB"`, or a bare integer of bytes
/// kept for backward compatibility with state already delivered as a number.
///
/// Strings are parsed with binary (1024-based) units to match
/// `docker/go-units` `RAMInBytes` and compose-go: units are case-insensitive,
/// the `b` suffix is optional (so `k`, `kb`, `kib` are all kilobytes), and an
/// absent unit means bytes.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawByteSize {
    Int(i64),
    Str(String),
}

/// A byte quantity from the remote model, stored canonically as whole bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteSize(i64);

impl ByteSize {
    /// Whole bytes, truncated toward zero.
    pub fn to_bytes(self) -> i64 {
        self.0
    }

    /// Parse a Docker `RAMInBytes`-style string (e.g. `"1g"`, `"512mb"`) into
    /// whole bytes using binary (1024-based) units.
    fn parse_str<E: serde::de::Error>(s: &str) -> Result<i64, E> {
        let bytes = Config::new()
            .with_binary()
            .parse_size(s)
            .map_err(|e| E::custom(format!("invalid byte value `{s}`: {e}")))?;
        i64::try_from(bytes).map_err(|_| E::custom(format!("byte value `{s}` exceeds i64::MAX")))
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(ByteSize(match RawByteSize::deserialize(deserializer)? {
            RawByteSize::Int(v) => v,
            RawByteSize::Str(s) => ByteSize::parse_str(&s)?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bare_int_is_bytes() {
        assert_eq!(
            ByteSize::deserialize(json!(67108864)).unwrap(),
            ByteSize(67108864)
        );
        assert_eq!(ByteSize::deserialize(json!(0)).unwrap(), ByteSize(0));
    }

    #[test]
    fn string_uses_binary_units() {
        assert_eq!(ByteSize::deserialize(json!("1b")).unwrap(), ByteSize(1));
        assert_eq!(ByteSize::deserialize(json!("1k")).unwrap(), ByteSize(1024));
        assert_eq!(
            ByteSize::deserialize(json!("1m")).unwrap(),
            ByteSize(1024 * 1024)
        );
        assert_eq!(
            ByteSize::deserialize(json!("1g")).unwrap(),
            ByteSize(1024 * 1024 * 1024)
        );
    }

    #[test]
    fn unit_suffix_and_case_are_flexible() {
        // `b`, `i`, and case are all optional/ignored for the unit letter.
        for s in ["1g", "1G", "1gb", "1GB", "1gib", "1GiB"] {
            assert_eq!(
                ByteSize::deserialize(json!(s)).unwrap(),
                ByteSize(1024 * 1024 * 1024),
                "{s}"
            );
        }
    }

    #[test]
    fn fractional_values_truncate_toward_zero() {
        assert_eq!(
            ByteSize::deserialize(json!("0.5g")).unwrap(),
            ByteSize(536870912)
        );
        assert_eq!(
            ByteSize::deserialize(json!("1.5k")).unwrap(),
            ByteSize(1536)
        );
    }

    #[test]
    fn optional_space_between_number_and_unit() {
        assert_eq!(
            ByteSize::deserialize(json!("512 mb")).unwrap(),
            ByteSize(512 * 1024 * 1024)
        );
    }

    #[test]
    fn absent_is_none() {
        assert_eq!(Option::<ByteSize>::deserialize(json!(null)).unwrap(), None);
    }

    #[test]
    fn garbage_string_is_rejected() {
        assert!(ByteSize::deserialize(json!("abc")).is_err());
        assert!(ByteSize::deserialize(json!("10x")).is_err());
        assert!(ByteSize::deserialize(json!("-5m")).is_err());
    }
}
