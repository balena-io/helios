//! Target types received from the remote backend
//!
//! All input validations should happen here
//!
//! the types are based  from https://github.com/balena-io/open-balena-api/blob/master/src/features/device-state/routes/state-get-v3.ts#L48

use serde::Deserialize;
use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
};

use helios_util::types::{ImageUri, Uuid};

/// Target device as defined by the remote backend
#[derive(Deserialize, Clone, Debug)]
pub struct DeviceTarget {
    pub name: String,

    #[serde(default)]
    pub apps: HashMap<Uuid, AppTarget>,
}

/// Target app as defined by the remote backend
#[derive(Deserialize, Clone, Debug)]
pub struct AppTarget {
    pub id: u32,
    pub name: String,

    #[serde(default)]
    pub is_host: bool,

    #[serde(default)]
    pub releases: ReleaseTargetMap,
}

#[derive(Clone, Debug, Default)]
pub struct ReleaseTargetMap(HashMap<Uuid, ReleaseTarget>);

impl Deref for ReleaseTargetMap {
    type Target = HashMap<Uuid, ReleaseTarget>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ReleaseTargetMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl IntoIterator for ReleaseTargetMap {
    type Item = (Uuid, ReleaseTarget);
    type IntoIter = std::collections::hash_map::IntoIter<Uuid, ReleaseTarget>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'de> Deserialize<'de> for ReleaseTargetMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let releases: HashMap<Uuid, ReleaseTarget> = HashMap::deserialize(deserializer)?;

        if releases.len() > 1 {
            return Err(serde::de::Error::custom(
                "target releases should only contain one release",
            ));
        }

        Ok(Self(releases))
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct ReleaseTarget {
    #[serde(default)]
    pub services: HashMap<String, ServiceTarget>,

    #[serde(default)]
    pub volumes: HashMap<String, VolumeTarget>,

    #[serde(default)]
    pub networks: HashMap<String, NetworkTarget>,
}

/// Target app as defined by the remote backend
// FIXME: add remaining fields
#[derive(Deserialize, Clone, Debug)]
pub struct ServiceTarget {
    pub id: u32,
    pub image: ImageUri,
}

// FIXME: add remaining fields
#[derive(Deserialize, Clone, Debug)]
pub struct VolumeTarget;

// FIXME: add remaining fields
#[derive(Deserialize, Clone, Debug)]
pub struct NetworkTarget;
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_accepts_empty_releases() {
        let json = json!({});

        let releases: ReleaseTargetMap = serde_json::from_value(json).unwrap();
        assert_eq!(releases.len(), 0)
    }

    #[test]
    fn test_accepts_single_release() {
        let json = json!({
            "release-one": {}
        });

        let releases: ReleaseTargetMap = serde_json::from_value(json).unwrap();
        assert_eq!(releases.len(), 1);
        assert!(releases.contains_key(&"release-one".into()));
    }

    #[test]
    fn test_rejects_target_releases_with_more_than_one_release() {
        let json = json!({
                "relase-one": {
                },
                "release-two": {
                },
        });

        let release = serde_json::from_value::<ReleaseTargetMap>(json);
        assert!(release.is_err());
    }
}
