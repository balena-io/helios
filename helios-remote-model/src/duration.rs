use go_parse_duration::parse_duration;
use serde::{Deserialize, Deserializer};

/// Compose/Go-style duration shapes accepted from the remote backend: a Go
/// `time.ParseDuration` string such as `"1m30s"`, or a bare integer kept for
/// backward compatibility with state already delivered as a number.
#[derive(Deserialize)]
#[serde(untagged)]
enum Duration {
    Int(i64),
    Str(String),
}

impl Duration {
    /// Parse a Go `time.ParseDuration` string into whole nanoseconds.
    fn parse_str<E: serde::de::Error>(s: &str) -> Result<i64, E> {
        parse_duration(s).map_err(|e| E::custom(format!("invalid duration `{s}`: {e:?}")))
    }
}

/// Defines a duration wrapper storing canonical nanoseconds.
/// - `$name`: the duration wrapper type name
/// - `$nanos_per_unit`: the number of nanoseconds per unit
macro_rules! duration {
    ($(#[$doc:meta])* $name:ident, $nanos_per_unit:expr) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct $name(i64);

        impl $name {
            /// Whole units in this type's named unit, truncated toward zero.
            pub fn to_i64(self) -> i64 {
                self.0 / $nanos_per_unit
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Ok($name(match Duration::deserialize(deserializer)? {
                    Duration::Int(v) => v * $nanos_per_unit,
                    Duration::Str(s) => Duration::parse_str(&s)?,
                }))
            }
        }
    };
}

duration!(
    /// Bare int is nanoseconds, as in Engine healthcheck
    DurationNanos, 1
);
duration!(
    /// Bare int is seconds, as in `stop_grace_period`
    DurationSecs, 1_000_000_000
);
duration!(
    /// Bare int is microseconds, as in `cpu_rt_period` and `cpu_rt_runtime`
    DurationMicros, 1_000
);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nanos_string_parses_with_go_semantics() {
        assert_eq!(
            DurationNanos::deserialize(json!("30s")).unwrap(),
            DurationNanos(30_000_000_000)
        );
        assert_eq!(
            DurationNanos::deserialize(json!("1m30s")).unwrap(),
            DurationNanos(90_000_000_000)
        );
        assert_eq!(
            DurationNanos::deserialize(json!("1500ms")).unwrap(),
            DurationNanos(1_500_000_000)
        );
        // Microseconds use the `us` unit (Compose supports us/ms/s/m/h)
        assert_eq!(
            DurationNanos::deserialize(json!("5us")).unwrap(),
            DurationNanos(5_000)
        );
        // Units can combine without a separator per Compose spec
        assert_eq!(
            DurationNanos::deserialize(json!("1h5m30s20ms")).unwrap(),
            DurationNanos(3_930_020_000_000)
        );
    }

    #[test]
    fn bare_int_unit_is_per_type() {
        assert_eq!(
            DurationNanos::deserialize(json!(5_000_000_000i64)).unwrap(),
            DurationNanos(5_000_000_000)
        );
        assert_eq!(
            DurationSecs::deserialize(json!(30)).unwrap(),
            DurationSecs(30_000_000_000)
        );
        assert_eq!(
            DurationMicros::deserialize(json!(950_000)).unwrap(),
            DurationMicros(950_000_000)
        );
    }

    #[test]
    fn string_form_parses_for_every_unit() {
        assert_eq!(
            DurationSecs::deserialize(json!("1m30s")).unwrap(),
            DurationSecs(90_000_000_000)
        );
        assert_eq!(
            DurationMicros::deserialize(json!("400ms")).unwrap(),
            DurationMicros(400_000_000)
        );
    }

    #[test]
    fn conversions_truncate_toward_zero() {
        assert_eq!(DurationNanos(1_500_000_000).to_i64(), 1_500_000_000);
        assert_eq!(DurationSecs(1_500_000_000).to_i64(), 1);
        assert_eq!(DurationMicros(1_500_000_500).to_i64(), 1_500_000);
    }

    #[test]
    fn absent_is_none() {
        assert_eq!(
            Option::<DurationNanos>::deserialize(json!(null)).unwrap(),
            None
        );
        assert_eq!(
            Option::<DurationSecs>::deserialize(json!(null)).unwrap(),
            None
        );
        assert_eq!(
            Option::<DurationMicros>::deserialize(json!(null)).unwrap(),
            None
        );
    }

    #[test]
    fn garbage_string_is_rejected() {
        assert!(DurationNanos::deserialize(json!("15x")).is_err());
        assert!(DurationSecs::deserialize(json!("abc")).is_err());
        assert!(DurationMicros::deserialize(json!("abc")).is_err());
    }
}
