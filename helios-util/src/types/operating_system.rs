use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("failed to parse OS version string, expected '<name> [<semver>[@<build>]]': got '{0}'")]
pub struct OperatingSystemParseError(String);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct OperatingSystem {
    /// Operating system name, e.g. balenaOS
    pub name: String,

    /// Optional OS version. Should be valid semver
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Build identifier, separated by `@` in the string representation.
    /// In balenaOS, this is the board revision commit hash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
}

impl FromStr for OperatingSystem {
    type Err = OperatingSystemParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();

        if s.is_empty() {
            return Err(OperatingSystemParseError(s.to_string()));
        }

        // Parse name and version+build part
        if let Some(last_space_idx) = s.rfind(' ') {
            let name = s[..last_space_idx].trim().to_string();
            let version_build_part = s[last_space_idx + 1..].trim();

            if name.is_empty() {
                return Err(OperatingSystemParseError(s.to_string()));
            }

            if version_build_part.is_empty() {
                return Ok(OperatingSystem {
                    name,
                    version: None,
                    build: None,
                });
            }

            // Check for build identifier in version_build_part
            let (version, build) = if let Some(at_idx) = version_build_part.rfind('@') {
                let version_part = version_build_part[..at_idx].trim();
                let build_part = version_build_part[at_idx + 1..].trim();

                if build_part.is_empty() || version_part.is_empty() {
                    return Err(OperatingSystemParseError(s.to_string()));
                }

                (Some(version_part.to_string()), Some(build_part.to_string()))
            } else {
                (Some(version_build_part.to_string()), None)
            };

            Ok(OperatingSystem {
                name,
                version,
                build,
            })
        } else {
            Ok(OperatingSystem {
                name: s.to_string(),
                version: None,
                build: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hostos_parse_basic() {
        let input = "balenaOS 6.5.6";
        let result = input.parse::<OperatingSystem>().unwrap();

        assert_eq!(result.name, "balenaOS");
        assert_eq!(result.version, Some("6.5.6".to_string()));
    }

    #[test]
    fn test_hostos_parse_with_rev() {
        let input = "balenaOS 6.5.6+rev1";
        let result = input.parse::<OperatingSystem>().unwrap();

        assert_eq!(result.name, "balenaOS");
        assert_eq!(result.version, Some("6.5.6+rev1".to_string()));
    }

    #[test]
    fn test_hostos_parse_name_with_spaces() {
        let input = "Balena Cloud OS 2.0.3+rev1";
        let result = input.parse::<OperatingSystem>().unwrap();

        assert_eq!(result.name, "Balena Cloud OS");
        assert_eq!(result.version, Some("2.0.3+rev1".to_string()));
    }

    #[test]
    fn test_hostos_parse_whitespace() {
        let input = "  Ubuntu Server   20.04.1+rev2  ";
        let result = input.parse::<OperatingSystem>().unwrap();

        assert_eq!(result.name, "Ubuntu Server");
        assert_eq!(result.version, Some("20.04.1+rev2".to_string()));
    }

    #[test]
    fn test_hostos_parse_error_empty_string() {
        let input = "";
        let result = input.parse::<OperatingSystem>();

        assert!(result.is_err());
    }

    #[test]
    fn test_hostos_parse_name_only() {
        let input = "balenaOS";
        let result = input.parse::<OperatingSystem>().unwrap();

        assert_eq!(result.name, "balenaOS");
        assert_eq!(result.version, None);
    }

    #[test]
    fn test_hostos_parse_name_with_spaces_only() {
        let input = "Ubuntu Server";
        let result = input.parse::<OperatingSystem>().unwrap();

        assert_eq!(result.name, "Ubuntu");
        assert_eq!(result.version, Some("Server".to_string()));
    }

    #[test]
    fn test_hostos_parse_empty_version() {
        let input = "balenaOS ";
        let result = input.parse::<OperatingSystem>().unwrap();

        assert_eq!(result.name, "balenaOS");
        assert_eq!(result.version, None);
    }

    #[test]
    fn test_hostos_parse_version_only() {
        let input = " 6.5.6";
        let result = input.parse::<OperatingSystem>().unwrap();

        // we don't assume any format for the name
        assert_eq!(result.name, "6.5.6");
        assert_eq!(result.version, None);
    }

    #[test]
    fn test_hostos_parse_with_build() {
        let input = "balenaOS 6.5.6+rev1@abc123f";
        let result = input.parse::<OperatingSystem>().unwrap();

        assert_eq!(result.name, "balenaOS");
        assert_eq!(result.version, Some("6.5.6+rev1".to_string()));
        assert_eq!(result.build, Some("abc123f".to_string()));
    }

    #[test]
    fn test_hostos_parse_error_empty_build() {
        let input = "balenaOS 6.5.6@";
        let result = input.parse::<OperatingSystem>();

        assert!(result.is_err());
    }

    #[test]
    fn test_hostos_parse_error_build_without_version() {
        let input = "balenaOS @abc123f";
        let result = input.parse::<OperatingSystem>();

        assert!(result.is_err());
    }
}
