//! Service ulimits as defined by the Compose spec.
//!
//! `ulimits` is a mapping from a limit name (e.g. `nofile`) to either a
//! single integer applied to both soft and hard limits, or an object with
//! the limits set separately.

use serde::{Deserialize, Deserializer};

/// A single resource limit override, with the soft/hard values already
/// resolved. The limit name is the key of the enclosing `ulimits` map.
#[derive(Debug, Clone, PartialEq)]
pub struct Ulimit {
    /// Soft limit, enforced by the kernel for the container's processes
    pub soft: i64,
    /// Hard limit, the ceiling the soft limit can be raised to
    pub hard: i64,
}

impl<'de> Deserialize<'de> for Ulimit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Single(i64),
            SoftHard { soft: i64, hard: i64 },
        }

        Ok(match Raw::deserialize(deserializer)? {
            Raw::Single(v) => Ulimit { soft: v, hard: v },
            Raw::SoftHard { soft, hard } => Ulimit { soft, hard },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn single_value_sets_soft_and_hard() {
        let ulimit: Ulimit = serde_json::from_value(json!(65535)).unwrap();
        assert_eq!(
            ulimit,
            Ulimit {
                soft: 65535,
                hard: 65535,
            }
        );
    }

    #[test]
    fn object_sets_separate_soft_and_hard() {
        let ulimit: Ulimit = serde_json::from_value(json!({"soft": 20000, "hard": 40000})).unwrap();
        assert_eq!(
            ulimit,
            Ulimit {
                soft: 20000,
                hard: 40000,
            }
        );
    }

    #[test]
    fn rejects_object_missing_soft_or_hard() {
        assert!(serde_json::from_value::<Ulimit>(json!({"soft": 1})).is_err());
        assert!(serde_json::from_value::<Ulimit>(json!({"hard": 1})).is_err());
    }
}
