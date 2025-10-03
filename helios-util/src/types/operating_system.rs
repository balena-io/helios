use serde::{Deserialize, Serialize};
use std::{fmt::Display, str::FromStr};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("failed to parse OS version string, expected '<name> [<semver>]': got '{0}'")]
pub struct OperatingSystemParseError(String);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct OperatingSystem {
    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl FromStr for OperatingSystem {
    type Err = OperatingSystemParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();

        if s.is_empty() {
            return Err(OperatingSystemParseError(s.to_string()));
        }

        if let Some(last_space_idx) = s.rfind(' ') {
            let name = s[..last_space_idx].trim().to_string();
            let version_part = s[last_space_idx + 1..].trim();

            if name.is_empty() {
                return Err(OperatingSystemParseError(s.to_string()));
            }

            let version = if version_part.is_empty() {
                None
            } else {
                Some(version_part.to_string())
            };

            Ok(OperatingSystem { name, version })
        } else {
            Ok(OperatingSystem {
                name: s.to_string(),
                version: None,
            })
        }
    }
}

impl Display for OperatingSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(version) = &self.version {
            write!(f, "{} {}", self.name, version)
        } else {
            write!(f, "{}", self.name)
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
}
