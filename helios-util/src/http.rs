pub use uri::{Domain, InvalidUriError, Uri};

mod uri {
    use std::fmt::Display;
    use std::str::FromStr;

    use addr::parse_domain_name;
    use axum::http;
    use serde::{Deserialize, Serialize};
    use thiserror::Error;

    pub use addr::domain::Name as Domain;

    #[derive(Debug, Error)]
    pub struct InvalidUriError(String);

    impl InvalidUriError {
        pub fn reason(&self) -> &str {
            self.0.as_str()
        }
    }

    impl Display for InvalidUriError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.fmt(f)
        }
    }

    impl From<http::uri::InvalidUri> for InvalidUriError {
        fn from(value: http::uri::InvalidUri) -> Self {
            InvalidUriError(value.to_string())
        }
    }

    impl From<http::uri::InvalidUriParts> for InvalidUriError {
        fn from(value: http::uri::InvalidUriParts) -> Self {
            InvalidUriError(value.to_string())
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub struct Uri(http::Uri);

    impl Uri {
        pub fn new(uri: http::Uri) -> Self {
            Self(uri)
        }

        pub fn domain<'a>(&'a self) -> Option<Domain<'a>> {
            self.0.host().and_then(|host| parse_domain_name(host).ok())
        }

        pub fn from_static(src: &'static str) -> Self {
            Self(http::Uri::from_static(src))
        }

        pub fn from_string(src: String) -> Result<Self, InvalidUriError> {
            Ok(Self(http::uri::Uri::from_maybe_shared(src)?))
        }

        pub fn from_parts(
            base_uri: Uri,
            path: &str,
            query: Option<&str>,
        ) -> Result<Self, InvalidUriError> {
            let path_and_query = if let Some(qs) = query {
                http::uri::PathAndQuery::from_maybe_shared(format!("{path}?{qs}",))?
            } else {
                http::uri::PathAndQuery::from_str(path)?
            };
            let mut parts = base_uri.0.into_parts();
            parts.path_and_query = Some(path_and_query);

            Ok(http::Uri::from_parts(parts).map(Self::new)?)
        }
    }

    impl Display for Uri {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.fmt(f)
        }
    }

    impl FromStr for Uri {
        type Err = InvalidUriError;

        fn from_str(s: &str) -> Result<Self, Self::Err> {
            Ok(http::Uri::from_str(s).map(Self::new)?)
        }
    }

    impl TryFrom<String> for Uri {
        type Error = InvalidUriError;

        fn try_from(value: String) -> Result<Self, Self::Error> {
            Ok(Self(http::Uri::from_maybe_shared(value)?))
        }
    }

    impl From<http::Uri> for Uri {
        fn from(value: http::Uri) -> Self {
            Self(value)
        }
    }

    impl From<Uri> for http::Uri {
        fn from(value: Uri) -> Self {
            value.0
        }
    }

    impl Serialize for Uri {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            serializer.serialize_str(&self.to_string())
        }
    }

    impl<'de> Deserialize<'de> for Uri {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            let s = String::deserialize(deserializer)?;
            s.parse().map_err(serde::de::Error::custom)
        }
    }
}
