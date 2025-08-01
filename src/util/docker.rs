use regex::Regex;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ImageNameError {
    #[error("invalid image name: {0}")]
    InvalidImageName(String),
    #[error(
        "invalid image format, expected [domain.tld/]repo/image[:tag][@digest] format, got: {0}"
    )]
    InvalidImageFormat(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageNameParts {
    pub registry: Option<String>,
    pub image_name: String,
    pub tag_name: Option<String>,
    pub digest: Option<String>,
}

/// Separate string containing registry and image name into its parts.
/// Example: registry2.balena.io/balena/rpi
///          { registry: "registry2.balena.io", imageName: "balena/rpi" }
/// Moved here from
pub fn get_image_parts(uri: &str) -> Result<ImageNameParts, ImageNameError> {
    let re = Regex::new(r"^(?:(localhost|.*?[.:].*?)/)?(.+?)(?::(.*?))?(?:@(.*?))?$")
        .expect("regular expression should compile");

    let caps = re
        .captures(uri)
        .ok_or_else(|| ImageNameError::InvalidImageName(uri.into()))?;

    let registry = caps.get(1).map(|m| m.as_str().into());
    let image_name = caps
        .get(2)
        .map(|m| m.as_str().into())
        .ok_or_else(|| ImageNameError::InvalidImageFormat(uri.into()))?;
    let tag = caps.get(3).map(|m| m.as_str().into());
    let digest = caps.get(4).map(|m| m.as_str().to_owned());

    let tag_name = if digest.is_none() && tag.is_none() {
        Some("latest".to_string())
    } else {
        tag
    };

    if let Some(ref d) = digest {
        let digest_re =
            Regex::new(r"^[A-Za-z][A-Za-z0-9]*(?:[-_+.][A-Za-z][A-Za-z0-9]*)*:[0-9a-f-A-F]{32,}$")
                .expect("regular expression should compile");

        if !digest_re.is_match(d) {
            return Err(ImageNameError::InvalidImageFormat(uri.to_string()));
        }
    }

    Ok(ImageNameParts {
        registry,
        image_name,
        tag_name,
        digest,
    })
}

/// Normalise an image name to always have a tag, with :latest being the default
pub fn normalise_image_name(image_ref: &str) -> Result<String, ImageNameError> {
    let parts = get_image_parts(image_ref)?;

    let repository = if let Some(registry) = parts.registry {
        format!("{}/{}", registry, parts.image_name)
    } else {
        parts.image_name
    };

    if let Some(digest) = parts.digest {
        Ok(format!("{repository}@{digest}"))
    } else {
        let tag = parts.tag_name.unwrap_or_else(|| "latest".to_string());
        Ok(format!("{repository}:{tag}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_image_parts_simple_image() {
        let parts = get_image_parts("ubuntu").unwrap();
        assert_eq!(parts.registry, None);
        assert_eq!(parts.image_name, "ubuntu");
        assert_eq!(parts.tag_name, Some("latest".to_string()));
        assert_eq!(parts.digest, None);
    }

    #[test]
    fn test_get_image_parts_with_tag() {
        let parts = get_image_parts("ubuntu:20.04").unwrap();
        assert_eq!(parts.registry, None);
        assert_eq!(parts.image_name, "ubuntu");
        assert_eq!(parts.tag_name, Some("20.04".to_string()));
        assert_eq!(parts.digest, None);
    }

    #[test]
    fn test_get_image_parts_with_registry() {
        let parts = get_image_parts("docker.io/library/ubuntu:latest").unwrap();
        assert_eq!(parts.registry, Some("docker.io".to_string()));
        assert_eq!(parts.image_name, "library/ubuntu");
        assert_eq!(parts.tag_name, Some("latest".to_string()));
        assert_eq!(parts.digest, None);
    }

    #[test]
    fn test_get_image_parts_with_digest() {
        let parts =
            get_image_parts("ubuntu@sha256:1234567890abcdef1234567890abcdef12345678").unwrap();
        assert_eq!(parts.registry, None);
        assert_eq!(parts.image_name, "ubuntu");
        assert_eq!(parts.tag_name, None);
        assert_eq!(
            parts.digest,
            Some("sha256:1234567890abcdef1234567890abcdef12345678".to_string())
        );
    }

    #[test]
    fn test_get_image_parts_with_tag_and_digest() {
        let parts = get_image_parts("ubuntu:20.04@sha256:1234567890abcdef1234567890abcdef12345678")
            .unwrap();
        assert_eq!(parts.registry, None);
        assert_eq!(parts.image_name, "ubuntu");
        assert_eq!(parts.tag_name, Some("20.04".to_string()));
        assert_eq!(
            parts.digest,
            Some("sha256:1234567890abcdef1234567890abcdef12345678".to_string())
        );
    }

    #[test]
    fn test_get_image_parts_localhost() {
        let parts = get_image_parts("localhost:5000/myimage:latest").unwrap();
        assert_eq!(parts.registry, Some("localhost:5000".to_string()));
        assert_eq!(parts.image_name, "myimage");
        assert_eq!(parts.tag_name, Some("latest".to_string()));
        assert_eq!(parts.digest, None);
    }

    #[test]
    fn test_get_image_parts_invalid_digest() {
        let result = get_image_parts("ubuntu@invalid-digest");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ImageNameError::InvalidImageFormat(_)
        ));
    }

    #[test]
    fn test_normalise_image_name_simple() {
        let result = normalise_image_name("ubuntu").unwrap();
        assert_eq!(result, "ubuntu:latest");
    }

    #[test]
    fn test_normalise_image_name_with_tag() {
        let result = normalise_image_name("ubuntu:20.04").unwrap();
        assert_eq!(result, "ubuntu:20.04");
    }

    #[test]
    fn test_normalise_image_name_with_digest() {
        let result =
            normalise_image_name("ubuntu@sha256:1234567890abcdef1234567890abcdef12345678").unwrap();
        assert_eq!(
            result,
            "ubuntu@sha256:1234567890abcdef1234567890abcdef12345678"
        );
    }

    #[test]
    fn test_normalise_image_name_with_tag_and_digest() {
        let result =
            normalise_image_name("ubuntu:20.04@sha256:1234567890abcdef1234567890abcdef12345678")
                .unwrap();
        assert_eq!(
            result,
            "ubuntu@sha256:1234567890abcdef1234567890abcdef12345678"
        );
    }

    #[test]
    fn test_normalise_image_name_with_registry() {
        let result = normalise_image_name("docker.io/library/ubuntu").unwrap();
        assert_eq!(result, "docker.io/library/ubuntu:latest");
    }

    #[test]
    fn test_normalise_image_name_with_registry_and_tag() {
        let result = normalise_image_name("docker.io/library/ubuntu:20.04").unwrap();
        assert_eq!(result, "docker.io/library/ubuntu:20.04");
    }

    #[test]
    fn test_normalise_image_name_with_registry_and_digest() {
        let result = normalise_image_name(
            "docker.io/library/ubuntu@sha256:1234567890abcdef1234567890abcdef12345678",
        )
        .unwrap();
        assert_eq!(
            result,
            "docker.io/library/ubuntu@sha256:1234567890abcdef1234567890abcdef12345678"
        );
    }

    #[test]
    fn test_normalise_image_name_localhost() {
        let result = normalise_image_name("localhost:5000/myimage").unwrap();
        assert_eq!(result, "localhost:5000/myimage:latest");
    }
}
