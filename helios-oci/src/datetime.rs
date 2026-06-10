use std::fmt;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{TimeZone, Utc};
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
        let sec = self.0.timestamp();
        let nsec = self.0.timestamp_subsec_nanos();
        if sec < 0 {
            UNIX_EPOCH - Duration::new(-sec as u64, 0) + Duration::new(0, nsec)
        } else {
            UNIX_EPOCH + Duration::new(sec as u64, nsec)
        }
    }
}

impl From<SystemTime> for DateTime {
    /// Build a `DateTime` from a `std::time::SystemTime`.
    fn from(v: SystemTime) -> Self {
        let (sec, nsec) = match v.duration_since(UNIX_EPOCH) {
            Ok(dur) => (dur.as_secs() as i64, dur.subsec_nanos()),
            Err(e) => {
                let dur = e.duration();
                let (sec, nsec) = (dur.as_secs() as i64, dur.subsec_nanos());
                if nsec == 0 {
                    (-sec, 0)
                } else {
                    (-sec - 1, 1_000_000_000 - nsec)
                }
            }
        };
        DateTime(Utc.timestamp_opt(sec, nsec).unwrap())
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
}
