use axum::http::Uri;
use std::time::Duration;

pub fn deserialize_uri<'de, D>(deserializer: D) -> std::result::Result<Uri, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let s = String::deserialize(deserializer)?;
    s.parse().map_err(serde::de::Error::custom)
}

pub fn serialize_uri<S>(uri: &Uri, serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&uri.to_string())
}

pub fn deserialize_duration_from_ms<'de, D>(
    deserializer: D,
) -> std::result::Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let ms: u64 = serde::Deserialize::deserialize(deserializer)?;
    Ok(Duration::from_millis(ms))
}

pub fn serialize_duration_to_ms<S>(
    duration: &Duration,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_u64(duration.as_millis() as u64)
}
