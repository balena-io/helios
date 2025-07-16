use axum::http::uri::PathAndQuery;
use axum::http::Uri;
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum UriError {
    #[error(transparent)]
    InvalidUri(#[from] axum::http::uri::InvalidUri),

    #[error(transparent)]
    InvalidUriParts(#[from] axum::http::uri::InvalidUriParts),
}

pub fn make_uri(base_uri: Uri, path: &str, query: Option<&str>) -> Result<Uri, UriError> {
    // Build the URI from the address parts
    let mut parts = base_uri.into_parts();
    parts.path_and_query = if let Some(qs) = query {
        Some(PathAndQuery::from_maybe_shared(format!("{path}?{qs}",))?)
    } else {
        Some(PathAndQuery::from_str(path)?)
    };
    Uri::from_parts(parts).map_err(|err| err.into())
}
