//! Service mount configuration as defined by the Compose spec.
//!
//! The `volumes` field on a service is an array of mount specs, each of which
//! is either a short-form string (`"src:dst"`, `"src:dst:ro"`) or a long-form
//! object with a `type`. Supported types are `volume`, `bind`, and `tmpfs`.
//!
//! Bind mounts are restricted to a fixed allowlist of host paths so app
//! services cannot arbitrarily mount the root filesystem.

use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor, value::MapAccessDeserializer},
};
use std::fmt;

/// Bind mount propagation mode.
#[derive(Deserialize, Clone, Debug, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BindPropagation {
    Private,
    Rprivate,
    Shared,
    Rshared,
    Slave,
    Rslave,
}

/// Volume-type mount (references a named volume declared in the top-level `volumes`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeMount {
    pub source: String,
    pub target: String,
    pub read_only: bool,
    pub nocopy: bool,
    pub subpath: Option<String>,
}

/// Bind-type mount (maps a host path into the container).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindMount {
    pub source: String,
    pub target: String,
    pub read_only: bool,
    pub propagation: Option<BindPropagation>,
}

/// Tmpfs-type mount (in-memory filesystem).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmpfsMount {
    pub target: String,
    pub size: Option<i64>,
    pub mode: Option<u32>,
}

/// A single service mount entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mount {
    Volume(VolumeMount),
    Bind(BindMount),
    Tmpfs(TmpfsMount),
}

impl Mount {
    /// Container path this mount binds to.
    pub fn target(&self) -> &str {
        match self {
            Mount::Volume(m) => &m.target,
            Mount::Bind(m) => &m.target,
            Mount::Tmpfs(m) => &m.target,
        }
    }
}

/// Host paths that are permitted as bind-mount sources, these are the
/// same bind mounts allowed from labels
const BIND_SOURCE_ALLOWLIST: &[&str] = &[
    "/var/run/docker.sock",
    "/run/dbus",
    "/sys",
    "/proc",
    "/lib/modules",
    "/lib/firmware",
    "/var/log/journal",
    "/run/log/journal",
    "/etc/machine-id",
];

fn is_allowed_bind_source(source: &str) -> bool {
    BIND_SOURCE_ALLOWLIST.contains(&source)
}

/// Service mount list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VolumesConfig(Vec<Mount>);

impl VolumesConfig {
    pub fn as_slice(&self) -> &[Mount] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Mount> {
        self.0.iter()
    }
}

impl IntoIterator for VolumesConfig {
    type Item = Mount;
    type IntoIter = std::vec::IntoIter<Mount>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// Long-form mount object used during deserialization.
#[derive(Deserialize)]
struct LongMount {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    source: Option<String>,
    target: String,
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    volume: Option<VolumeOptions>,
    #[serde(default)]
    bind: Option<BindOptions>,
    #[serde(default)]
    tmpfs: Option<TmpfsOptions>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct VolumeOptions {
    nocopy: bool,
    subpath: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct BindOptions {
    propagation: Option<BindPropagation>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TmpfsOptions {
    size: Option<i64>,
    mode: Option<u32>,
}

enum RawMount {
    Short(String),
    Long(LongMount),
}

// Custom Deserialize so that errors inside the long-form object preserve their
// field path (e.g. `volumes[0].read_only`)
impl<'de> Deserialize<'de> for RawMount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RawMountVisitor;

        impl<'de> Visitor<'de> for RawMountVisitor {
            type Value = RawMount;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a short-form mount string or long-form mount object")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(RawMount::Short(v.to_string()))
            }

            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(RawMount::Short(v))
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                LongMount::deserialize(MapAccessDeserializer::new(map)).map(RawMount::Long)
            }
        }

        deserializer.deserialize_any(RawMountVisitor)
    }
}

impl<'de> Deserialize<'de> for VolumesConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: Vec<RawMount> = Vec::deserialize(deserializer)?;
        let mut mounts = Vec::with_capacity(raw.len());
        for entry in raw {
            mounts.push(parse_mount(entry).map_err(serde::de::Error::custom)?);
        }
        // Canonicalize by container-side target so the downstream serialized form
        // is stable against reorderings in the remote composition.
        mounts.sort_by(|a, b| a.target().cmp(b.target()));
        Ok(VolumesConfig(mounts))
    }
}

fn parse_mount(raw: RawMount) -> Result<Mount, String> {
    match raw {
        RawMount::Short(s) => parse_short(&s),
        RawMount::Long(m) => parse_long(m),
    }
}

fn parse_short(s: &str) -> Result<Mount, String> {
    // `src:dst` or `src:dst:mode`
    let parts: Vec<&str> = s.splitn(3, ':').collect();
    if parts.len() < 2 {
        return Err(format!(
            "invalid short-form mount '{s}': expected `src:dst`"
        ));
    }
    let source = parts[0];
    let target = parts[1];
    let mode = parts.get(2).copied().unwrap_or("");

    if source.is_empty() || target.is_empty() {
        return Err(format!("invalid short-form mount '{s}'"));
    }

    let mut read_only = false;
    for token in mode.split(',').filter(|t| !t.is_empty()) {
        match token {
            "ro" => read_only = true,
            "rw" => read_only = false,
            // SELinux/other flags are ignored on platforms that don't enforce them;
            // accept them silently for compatibility with the short-form spec.
            "z" | "Z" => {}
            other => {
                return Err(format!(
                    "unsupported short-form mount option '{other}' in '{s}'"
                ));
            }
        }
    }

    // Host paths start with `/` or `.`; anything else is a named volume.
    if source.starts_with('/') {
        if !is_allowed_bind_source(source) {
            return Err(format!(
                "bind mount source '{source}' is not in the allowed list"
            ));
        }
        Ok(Mount::Bind(BindMount {
            source: source.to_string(),
            target: target.to_string(),
            read_only,
            propagation: None,
        }))
    } else if source.starts_with('.') {
        Err(format!(
            "relative bind paths are not supported (got '{source}')"
        ))
    } else {
        Ok(Mount::Volume(VolumeMount {
            source: source.to_string(),
            target: target.to_string(),
            read_only,
            nocopy: false,
            subpath: None,
        }))
    }
}

fn parse_long(m: LongMount) -> Result<Mount, String> {
    let LongMount {
        kind,
        source,
        target,
        read_only,
        volume,
        bind,
        tmpfs,
    } = m;

    if target.is_empty() {
        return Err("mount target cannot be empty".to_string());
    }

    match kind.as_str() {
        "volume" => {
            let source = source.ok_or_else(|| "volume mount requires a `source`".to_string())?;
            let VolumeOptions { nocopy, subpath } = volume.unwrap_or_default();
            Ok(Mount::Volume(VolumeMount {
                source,
                target,
                read_only,
                nocopy,
                subpath,
            }))
        }
        "bind" => {
            let source = source.ok_or_else(|| "bind mount requires a `source`".to_string())?;
            if !is_allowed_bind_source(&source) {
                return Err(format!(
                    "bind mount source '{source}' is not in the allowed list"
                ));
            }
            let BindOptions { propagation } = bind.unwrap_or_default();
            Ok(Mount::Bind(BindMount {
                source,
                target,
                read_only,
                propagation,
            }))
        }
        "tmpfs" => {
            let TmpfsOptions { size, mode } = tmpfs.unwrap_or_default();
            Ok(Mount::Tmpfs(TmpfsMount { target, size, mode }))
        }
        "image" | "npipe" | "cluster" => Err(format!("mount type `{kind}` is not supported")),
        other => Err(format!("unknown mount type `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn short_form_named_volume() {
        let v: VolumesConfig = serde_json::from_value(json!(["my-vol:/data"])).unwrap();
        assert_eq!(
            v.as_slice(),
            &[Mount::Volume(VolumeMount {
                source: "my-vol".to_string(),
                target: "/data".to_string(),
                read_only: false,
                nocopy: false,
                subpath: None,
            })]
        );
    }

    #[test]
    fn short_form_named_volume_readonly() {
        let v: VolumesConfig = serde_json::from_value(json!(["my-vol:/data:ro"])).unwrap();
        match &v.as_slice()[0] {
            Mount::Volume(m) => assert!(m.read_only),
            _ => panic!("expected volume mount"),
        }
    }

    #[test]
    fn short_form_allowed_bind() {
        let v: VolumesConfig =
            serde_json::from_value(json!(["/etc/machine-id:/etc/machine-id"])).unwrap();
        assert_eq!(
            v.as_slice(),
            &[Mount::Bind(BindMount {
                source: "/etc/machine-id".to_string(),
                target: "/etc/machine-id".to_string(),
                read_only: false,
                propagation: None,
            })]
        );
    }

    #[test]
    fn short_form_bind_not_in_allowlist_rejected() {
        let err = serde_json::from_value::<VolumesConfig>(json!(["/root:/root"])).unwrap_err();
        assert!(err.to_string().contains("not in the allowed list"));
    }

    #[test]
    fn short_form_bind_allows_custom_target() {
        // Allowlisted source + arbitrary target is permitted.
        let v: VolumesConfig =
            serde_json::from_value(json!(["/etc/machine-id:/app/machine-id:ro"])).unwrap();
        assert_eq!(
            v.as_slice(),
            &[Mount::Bind(BindMount {
                source: "/etc/machine-id".to_string(),
                target: "/app/machine-id".to_string(),
                read_only: true,
                propagation: None,
            })]
        );
    }

    #[test]
    fn short_form_relative_bind_rejected() {
        let err = serde_json::from_value::<VolumesConfig>(json!(["./data:/data"])).unwrap_err();
        assert!(err.to_string().contains("relative bind paths"));
    }

    #[test]
    fn long_form_volume_with_options() {
        let v: VolumesConfig = serde_json::from_value(json!([{
            "type": "volume",
            "source": "db-data",
            "target": "/etc/data",
            "read_only": true,
            "volume": { "nocopy": true, "subpath": "sub" }
        }]))
        .unwrap();
        assert_eq!(
            v.as_slice(),
            &[Mount::Volume(VolumeMount {
                source: "db-data".to_string(),
                target: "/etc/data".to_string(),
                read_only: true,
                nocopy: true,
                subpath: Some("sub".to_string()),
            })]
        );
    }

    #[test]
    fn long_form_bind_allowed() {
        let v: VolumesConfig = serde_json::from_value(json!([{
            "type": "bind",
            "source": "/var/run/docker.sock",
            "target": "/host/run/docker.sock"
        }]))
        .unwrap();
        assert!(matches!(v.as_slice()[0], Mount::Bind(_)));
    }

    #[test]
    fn long_form_bind_rejected_when_not_allowlisted() {
        let err = serde_json::from_value::<VolumesConfig>(json!([{
            "type": "bind",
            "source": "/home/user",
            "target": "/data"
        }]))
        .unwrap_err();
        assert!(err.to_string().contains("not in the allowed list"));
    }

    #[test]
    fn long_form_bind_allows_custom_target() {
        let v: VolumesConfig = serde_json::from_value(json!([{
            "type": "bind",
            "source": "/proc",
            "target": "/app/proc"
        }]))
        .unwrap();
        match &v.as_slice()[0] {
            Mount::Bind(b) => {
                assert_eq!(b.source, "/proc");
                assert_eq!(b.target, "/app/proc");
            }
            _ => panic!("expected bind mount"),
        }
    }

    #[test]
    fn long_form_tmpfs() {
        let v: VolumesConfig = serde_json::from_value(json!([{
            "type": "tmpfs",
            "target": "/tmp",
            "tmpfs": { "size": 65536, "mode": 448 }
        }]))
        .unwrap();
        assert_eq!(
            v.as_slice(),
            &[Mount::Tmpfs(TmpfsMount {
                target: "/tmp".to_string(),
                size: Some(65536),
                mode: Some(448),
            })]
        );
    }

    #[test]
    fn long_form_image_rejected() {
        let err = serde_json::from_value::<VolumesConfig>(json!([{
            "type": "image",
            "source": "some/image",
            "target": "/data"
        }]))
        .unwrap_err();
        assert!(err.to_string().contains("not supported"));
    }

    #[test]
    fn long_form_npipe_rejected() {
        let err = serde_json::from_value::<VolumesConfig>(json!([{
            "type": "npipe",
            "source": "foo",
            "target": "/data"
        }]))
        .unwrap_err();
        assert!(err.to_string().contains("not supported"));
    }

    #[test]
    fn long_form_cluster_rejected() {
        let err = serde_json::from_value::<VolumesConfig>(json!([{
            "type": "cluster",
            "source": "foo",
            "target": "/data"
        }]))
        .unwrap_err();
        assert!(err.to_string().contains("not supported"));
    }

    #[test]
    fn empty_default() {
        let v: VolumesConfig = serde_json::from_value(json!([])).unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn deserialize_sorts_by_target() {
        // Deserialization canonicalizes by target so reordering in the remote
        // composition does not propagate downstream.
        let v: VolumesConfig = serde_json::from_value(json!([
            "vol-c:/c",
            {"type": "tmpfs", "target": "/a"},
            "vol-b:/b",
        ]))
        .unwrap();
        let targets: Vec<&str> = v.iter().map(|m| m.target()).collect();
        assert_eq!(targets, vec!["/a", "/b", "/c"]);
    }
}
