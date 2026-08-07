use std::fmt;
use std::str::FromStr;
use std::time::SystemTime;

use chrono::Utc;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime(chrono::DateTime<Utc>);

impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.to_rfc3339())
    }
}

impl FromStr for DateTime {
    type Err = chrono::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(DateTime(chrono::DateTime::parse_from_rfc3339(s)?.to_utc()))
    }
}

impl DateTime {
    /// Convert to a `std::time::SystemTime` for comparison against host clocks
    pub fn as_system_time(&self) -> SystemTime {
        self.0.into()
    }
}

impl From<SystemTime> for DateTime {
    /// Build a `DateTime` from a `std::time::SystemTime`.
    fn from(v: SystemTime) -> Self {
        DateTime(v.into())
    }
}

impl Serialize for DateTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for DateTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn roundtrip_display_fromstr() {
        let dt: DateTime = "2024-01-15T10:30:00+00:00".parse().unwrap();
        let s = dt.to_string();
        let dt2: DateTime = s.parse().unwrap();
        assert_eq!(dt, dt2);
    }

    #[test]
    fn roundtrip_serde() {
        let dt: DateTime = "2024-01-15T10:30:00+00:00".parse().unwrap();
        let json = serde_json::to_string(&dt).unwrap();
        let dt2: DateTime = serde_json::from_str(&json).unwrap();
        assert_eq!(dt, dt2);
    }

    #[test]
    fn invalid_string_fails() {
        assert!("not-a-date".parse::<DateTime>().is_err());
    }

    #[test]
    fn roundtrips_system_time_across_the_epoch() {
        // The conversions delegate to chrono, which has to keep handling times
        // before the epoch: a container created on a device whose clock had not
        // yet synced reports one.
        for offset in [
            Duration::from_secs(0),
            Duration::from_nanos(1),
            Duration::from_secs(1_700_000_000),
        ] {
            for t in [UNIX_EPOCH + offset, UNIX_EPOCH - offset] {
                assert_eq!(DateTime::from(t).as_system_time(), t);
            }
        }
    }
}
