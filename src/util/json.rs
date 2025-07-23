use axum::http::Uri;
use std::time::Duration;

pub fn deserialize_optional_uri<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Uri>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let s: Option<String> = Option::deserialize(deserializer)?;
    match s {
        Some(s) => s.parse().map(Some).map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

pub fn serialize_optional_uri<S>(
    uri: &Option<Uri>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match uri {
        Some(uri) => serializer.serialize_some(&uri.to_string()),
        None => serializer.serialize_none(),
    }
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
