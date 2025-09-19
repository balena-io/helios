use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

use serde::{Deserialize, Serialize};

use crate::types::Uuid;

use super::service::{ServiceMap, TargetServiceMap};

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Release {
    #[serde(default)]
    pub services: ServiceMap,
}

pub type ReleaseMap = BTreeMap<Uuid, Release>;

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub struct TargetRelease {
    #[serde(default)]
    pub services: TargetServiceMap,
}

impl From<Release> for TargetRelease {
    fn from(r: Release) -> Self {
        let Release { services } = r;

        Self {
            services: services
                .into_iter()
                .map(|(name, svc)| (name, svc.into()))
                .collect(),
        }
    }
}

#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
pub struct TargetReleaseMap(BTreeMap<Uuid, TargetRelease>);

impl Deref for TargetReleaseMap {
    type Target = BTreeMap<Uuid, TargetRelease>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for TargetReleaseMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl FromIterator<(Uuid, TargetRelease)> for TargetReleaseMap {
    fn from_iter<T: IntoIterator<Item = (Uuid, TargetRelease)>>(iter: T) -> Self {
        Self(BTreeMap::from_iter(iter))
    }
}

impl<'de> Deserialize<'de> for TargetReleaseMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let releases: BTreeMap<Uuid, TargetRelease> = BTreeMap::deserialize(deserializer)?;

        if releases.len() > 1 {
            return Err(serde::de::Error::custom(
                "target releases should only contain one release",
            ));
        }

        Ok(Self(releases))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_accepts_empty_releases() {
        let json = json!({});

        let releases: TargetReleaseMap = serde_json::from_value(json).unwrap();
        assert_eq!(releases.len(), 0)
    }

    #[test]
    fn test_accepts_single_release() {
        let json = json!({
            "release-one": {}
        });

        let releases: TargetReleaseMap = serde_json::from_value(json).unwrap();
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

        let release = serde_json::from_value::<TargetReleaseMap>(json);
        assert!(release.is_err());
    }
}
