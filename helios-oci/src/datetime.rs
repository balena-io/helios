use std::fmt;
use std::str::FromStr;

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
