//! Service device mappings as defined by the Compose spec.
//!
//! The `devices` field on a service is an array of mappings in the form of
//! `HOST_PATH:CONTAINER_PATH[:CGROUP_PERMISSIONS]`. The permissions default
//! to `rwm`.
//!
//! Only host path mappings are supported: the CDI syntax the spec also allows
//! (`vendor1.com/device=gpu`) is rejected at parse time. Since a CDI device
//! name never starts with `/`, any entry that is not an absolute path is
//! rejected as one.

use serde::{Deserialize, Deserializer};

/// A host device mapped into the container, with the cgroup permissions
/// default already resolved.
#[derive(Debug, PartialEq)]
pub struct DeviceMapping {
    /// Path of the device on the host
    pub source: String,
    /// Path of the device inside the container
    pub target: String,
    /// Cgroup permissions for the device (combination of `r`, `w` and `m`)
    pub permissions: String,
}

/// Validate a cgroup permissions string: a non-empty combination of `r`, `w`
/// and `m` without repetition.
fn is_device_permissions(s: &str) -> bool {
    let (mut r, mut w, mut m) = (false, false, false);
    !s.is_empty()
        && s.chars().all(|c| {
            let flag = match c {
                'r' => &mut r,
                'w' => &mut w,
                'm' => &mut m,
                _ => return false,
            };
            !std::mem::replace(flag, true)
        })
}

/// Build a mapping from its parts, validating that the target path is
/// absolute and the permissions are a valid cgroup mode. The source is
/// validated before the parts are split.
fn device(source: &str, target: &str, permissions: &str) -> Result<DeviceMapping, String> {
    if !target.starts_with('/') {
        return Err(format!("target must be an absolute path, got `{target}`"));
    }
    if !is_device_permissions(permissions) {
        return Err(format!(
            "permissions must be a combination of `r`, `w` and `m`, got `{permissions}`"
        ));
    }
    Ok(DeviceMapping {
        source: source.to_string(),
        target: target.to_string(),
        permissions: permissions.to_string(),
    })
}

impl<'de> Deserialize<'de> for DeviceMapping {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let spec = String::deserialize(deserializer)?;

        // An entry that does not map an absolute host path is a CDI device
        // name (`vendor1.com/device=gpu`), which may be a lone token with no
        // target. Reject it before enforcing the mapping shape.
        if !spec.starts_with('/') {
            return Err(serde::de::Error::custom(format!(
                "source must be an absolute path (CDI syntax is not yet supported), got `{spec}`"
            )));
        }

        let parts: Vec<&str> = spec.split(':').collect();
        match parts.as_slice() {
            [source, target] => device(source, target, "rwm"),
            [source, target, permissions] => device(source, target, permissions),
            _ => Err(format!(
                "expected `HOST_PATH:CONTAINER_PATH[:CGROUP_PERMISSIONS]` got `{spec}`"
            )),
        }
        .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn devices(value: serde_json::Value) -> Vec<DeviceMapping> {
        serde_json::from_value(value).unwrap()
    }

    fn mapping(source: &str, target: &str, permissions: &str) -> DeviceMapping {
        DeviceMapping {
            source: source.to_string(),
            target: target.to_string(),
            permissions: permissions.to_string(),
        }
    }

    #[test]
    fn source_and_target_default_permissions() {
        assert_eq!(
            devices(json!(["/dev/ttyUSB0:/dev/ttyUSB1"])),
            vec![mapping("/dev/ttyUSB0", "/dev/ttyUSB1", "rwm")]
        );
    }

    #[test]
    fn full_mapping() {
        assert_eq!(
            devices(json!(["/dev/sda:/dev/xvda:rw"])),
            vec![mapping("/dev/sda", "/dev/xvda", "rw")]
        );
    }

    #[test]
    fn rejects_cdi_syntax() {
        let err = serde_json::from_value::<Vec<DeviceMapping>>(json!(["vendor1.com/device=gpu"]))
            .unwrap_err();
        assert!(
            err.to_string().contains("CDI syntax is not yet supported"),
            "{err}"
        );
    }

    #[test]
    fn rejects_invalid_mappings() {
        for invalid in [
            json!([""]),
            // the container path is required
            json!(["/dev/ttyUSB0"]),
            json!(["/dev/sda:rwm"]),
            json!(["dev/ttyUSB0:/dev/ttyUSB0"]),
            json!(["/dev/sda:xvda"]),
            json!(["/dev/sda:/dev/xvda:rwx"]),
            json!(["/dev/sda:/dev/xvda:rr"]),
            json!(["/dev/sda:/dev/xvda:"]),
            json!(["/dev/sda:/dev/xvda:rw:extra"]),
            // long-form objects are not part of the spec
            json!([{ "source": "/dev/sda", "target": "/dev/xvda" }]),
        ] {
            assert!(
                serde_json::from_value::<Vec<DeviceMapping>>(invalid.clone()).is_err(),
                "{invalid}"
            );
        }
    }
}
