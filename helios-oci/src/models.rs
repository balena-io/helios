use std::{
    fmt::Display,
    hash::{Hash, Hasher},
    str::FromStr,
    sync::LazyLock,
};

use mahler::State;
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
#[error("invalid uri format, expected [domain.tld/]repo/image[:tag][@digest] format, got: {0}")]
pub struct InvalidImageUriError(String);

#[derive(State, Debug, Clone, Eq)]
pub struct ImageUri {
    registry: Option<String>,
    image: String,
    tag: Option<String>,
    digest: Option<String>,
    normalized: Box<str>,
}

impl PartialOrd for ImageUri {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ImageUri {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.normalized.cmp(&other.normalized)
    }
}

impl ImageUri {
    pub fn from_static(uri: &str) -> Self {
        uri.parse()
            .expect("URI should have format [domain.tld/]repo/image[:tag][@digest]")
    }

    pub fn registry(&self) -> &Option<String> {
        &self.registry
    }

    pub fn image(&self) -> &String {
        &self.image
    }

    pub fn tag(&self) -> &Option<String> {
        &self.tag
    }

    pub fn digest(&self) -> &Option<String> {
        &self.digest
    }

    pub fn repo(&self) -> String {
        let Self {
            image, registry, ..
        } = self;
        if let Some(registry) = registry {
            format!("{registry}/{image}")
        } else {
            image.clone()
        }
    }

    pub fn as_str(&self) -> &str {
        &self.normalized
    }
}

impl PartialEq for ImageUri {
    fn eq(&self, other: &Self) -> bool {
        self.normalized == other.normalized
    }
}

impl Hash for ImageUri {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.normalized.hash(state);
    }
}

impl Display for ImageUri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.normalized.fmt(f)
    }
}

static IMAGE_URI_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(localhost|.*?[.:].*?)/)?(.+?)(?::(.*?))?(?:@(.*?))?$").unwrap()
});

static IMAGE_DIGEST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z][A-Za-z0-9]*(?:[-_+.][A-Za-z][A-Za-z0-9]*)*:[0-9a-f-A-F]{32,}$").unwrap()
});

impl FromStr for ImageUri {
    type Err = InvalidImageUriError;

    fn from_str(uri: &str) -> Result<Self, Self::Err> {
        let caps = IMAGE_URI_RE
            .captures(uri)
            .ok_or_else(|| InvalidImageUriError(uri.into()))?;

        let registry = caps.get(1).map(|m| m.as_str().into());
        let image: String = caps
            .get(2)
            .map(|m| m.as_str().into())
            .ok_or_else(|| InvalidImageUriError(uri.into()))?;
        let tag = caps.get(3).map(|m| m.as_str());
        let digest = caps.get(4).map(|m| m.as_str().to_owned());

        // omit the tag if explicitely set to `latest`
        let tag = if let Some("latest") = tag {
            None
        } else {
            tag.map(|t| t.to_owned())
        };

        if let Some(ref d) = digest {
            if !IMAGE_DIGEST_RE.is_match(d) {
                return Err(InvalidImageUriError(uri.into()));
            }
        }

        let normalized = {
            let repo: String = if let Some(ref registry) = registry {
                format!("{registry}/{image}")
            } else {
                image.clone()
            };

            if let Some(ref digest) = digest {
                format!("{repo}@{digest}")
            } else {
                let tag = tag.as_deref().unwrap_or("latest");
                format!("{repo}:{tag}")
            }
        };

        Ok(ImageUri {
            registry,
            image,
            tag,
            digest,
            normalized: normalized.into_boxed_str(),
        })
    }
}

impl From<ImageUri> for String {
    fn from(uri: ImageUri) -> Self {
        uri.normalized.into_string()
    }
}

impl Serialize for ImageUri {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.normalized.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ImageUri {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s: String = String::deserialize(deserializer)?;
        let uri: ImageUri = s.parse().map_err(serde::de::Error::custom)?;
        Ok(uri)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_uri_simple_image() {
        let uri = ImageUri::from_static("ubuntu");
        assert_eq!(uri.registry, None);
        assert_eq!(uri.image, "ubuntu");
        assert_eq!(uri.tag, None);
        assert_eq!(uri.digest, None);
    }

    #[test]
    fn test_registry_uri_with_tag() {
        let uri = ImageUri::from_static("ubuntu:20.04");
        assert_eq!(uri.registry, None);
        assert_eq!(uri.image, "ubuntu");
        assert_eq!(uri.tag, Some("20.04".to_string()));
        assert_eq!(uri.digest, None);
    }

    #[test]
    fn test_registry_uri_with_registry() {
        let uri = ImageUri::from_static("docker.io/library/ubuntu:latest");
        assert_eq!(uri.registry, Some("docker.io".to_string()));
        assert_eq!(uri.image, "library/ubuntu");
        assert_eq!(uri.tag, None);
        assert_eq!(uri.digest, None);
    }

    #[test]
    fn test_registry_uri_with_digest() {
        let uri = ImageUri::from_static("ubuntu@sha256:1234567890abcdef1234567890abcdef12345678");
        assert_eq!(uri.registry, None);
        assert_eq!(uri.image, "ubuntu");
        assert_eq!(uri.tag, None);
        assert_eq!(
            uri.digest,
            Some("sha256:1234567890abcdef1234567890abcdef12345678".to_string())
        );
    }

    #[test]
    fn test_registry_uri_with_tag_and_digest() {
        let uri =
            ImageUri::from_static("ubuntu:20.04@sha256:1234567890abcdef1234567890abcdef12345678");
        assert_eq!(uri.registry, None);
        assert_eq!(uri.image, "ubuntu");
        assert_eq!(uri.tag, Some("20.04".to_string()));
        assert_eq!(
            uri.digest,
            Some("sha256:1234567890abcdef1234567890abcdef12345678".to_string())
        );
    }

    #[test]
    fn test_registry_uri_localhost() {
        let uri = ImageUri::from_static("localhost:5000/myimage:latest");
        assert_eq!(uri.registry, Some("localhost:5000".to_string()));
        assert_eq!(uri.image, "myimage");
        assert_eq!(uri.tag, None);
        assert_eq!(uri.digest, None);
    }

    #[test]
    fn test_registry_uri_invalid_digest() {
        let result: Result<ImageUri, _> = "ubuntu@invalid-digest".parse();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), InvalidImageUriError(_)));
    }

    #[test]
    fn test_registry_uri_to_string_simple() {
        let uri = ImageUri::from_static("ubuntu");
        assert_eq!(uri.as_str(), "ubuntu:latest");
    }

    #[test]
    fn test_registry_uri_to_string_with_tag() {
        let uri = ImageUri::from_static("ubuntu:20.04");
        assert_eq!(uri.as_str(), "ubuntu:20.04");
    }

    #[test]
    fn test_registry_uri_to_string_with_digest() {
        let uri = ImageUri::from_static("ubuntu@sha256:1234567890abcdef1234567890abcdef12345678");
        assert_eq!(
            uri.as_str(),
            "ubuntu@sha256:1234567890abcdef1234567890abcdef12345678"
        );
    }

    #[test]
    fn test_registry_uri_to_string_with_tag_and_digest() {
        let uri =
            ImageUri::from_static("ubuntu:20.04@sha256:1234567890abcdef1234567890abcdef12345678");
        assert_eq!(
            uri.as_str(),
            "ubuntu@sha256:1234567890abcdef1234567890abcdef12345678"
        );
    }

    #[test]
    fn test_registry_uri_to_string_with_registry() {
        let uri = ImageUri::from_static("docker.io/library/ubuntu");
        assert_eq!(uri.as_str(), "docker.io/library/ubuntu:latest");
    }

    #[test]
    fn test_registry_uri_to_string_with_registry_and_tag() {
        let uri = ImageUri::from_static("docker.io/library/ubuntu:20.04");
        assert_eq!(uri.as_str(), "docker.io/library/ubuntu:20.04");
    }

    #[test]
    fn test_registry_uri_to_string_with_registry_and_digest() {
        let uri = ImageUri::from_static(
            "docker.io/library/ubuntu@sha256:1234567890abcdef1234567890abcdef12345678",
        );
        assert_eq!(
            uri.as_str(),
            "docker.io/library/ubuntu@sha256:1234567890abcdef1234567890abcdef12345678"
        );
    }

    #[test]
    fn test_registry_uri_to_string_localhost() {
        let uri = ImageUri::from_static("localhost:5000/myimage");
        assert_eq!(uri.as_str(), "localhost:5000/myimage:latest");
    }

    #[test]
    fn test_registry_uri_image_with_registry() {
        let uri = ImageUri::from_static("docker.io/library/ubuntu:20.04");
        assert_eq!(uri.repo(), "docker.io/library/ubuntu");
    }

    #[test]
    fn test_registry_uri_image_with_registry_no_registry() {
        let uri = ImageUri::from_static("ubuntu:20.04");
        assert_eq!(uri.repo(), "ubuntu");
    }
}
