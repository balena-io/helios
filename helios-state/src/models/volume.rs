use std::ops::{Deref, DerefMut};

use crate::labels::{LABEL_APP_UUID, LABEL_SUPERVISED, LABEL_SUPERVISED_LEGACY, LABEL_VOLUME_NAME};
use crate::oci::{self, VolumeDriver};
use crate::remote_model::Volume as RemoteVolume;
use mahler::state::State;
use serde::{Deserialize, Serialize};

/// A volume that exists on the container engine
#[derive(Default, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LocalVolume {
    #[serde(default)]
    pub oci_name: String,
    #[serde(default)]
    pub config: VolumeConfig,
}

/// Serde representation of a volume that is either external or has a payload
///
/// External volumes serialize as `{"external": true}`. Internal ones flatten their
/// payload and omit the flag.
#[derive(Serialize, Deserialize)]
struct ExternalOr<T> {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    external: bool,
    #[serde(flatten)]
    internal: Option<T>,
}

impl<T> ExternalOr<T> {
    fn external() -> Self {
        Self {
            external: true,
            internal: None,
        }
    }

    fn internal(value: T) -> Self {
        Self {
            external: false,
            internal: Some(value),
        }
    }

    /// The payload of an internal volume, `None` if the volume is external
    fn into_internal(self) -> Option<T> {
        (!self.external).then_some(self.internal).flatten()
    }
}

/// A volume referenced by a release
///
/// External volumes are expected to already exist on the device, helios neither
/// creates nor removes them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(from = "ExternalOr<LocalVolume>", into = "ExternalOr<LocalVolume>")]
pub enum Volume {
    /// An external volume has no config, it is referenced in the composition by name
    External,
    /// An internal volume managed for the app
    Internal(LocalVolume),
}

impl From<Volume> for ExternalOr<LocalVolume> {
    fn from(vol: Volume) -> Self {
        match vol {
            Volume::External => Self::external(),
            Volume::Internal(local) => Self::internal(local),
        }
    }
}

impl From<ExternalOr<LocalVolume>> for Volume {
    fn from(repr: ExternalOr<LocalVolume>) -> Self {
        repr.into_internal()
            .map_or(Volume::External, Volume::Internal)
    }
}

impl Volume {
    /// Name of the volume on the container engine, `None` for external volumes
    pub fn oci_name(&self) -> Option<&str> {
        match self {
            Volume::External => None,
            Volume::Internal(local) => Some(&local.oci_name),
        }
    }

    /// Whether the volume already matches `tgt`
    ///
    /// An external volume never matches an internal one, even if the configurations
    /// are equal, as the mount source used by containers differs between the two.
    pub fn matches(&self, tgt: &VolumeTarget) -> bool {
        match (self, tgt) {
            (Volume::External, VolumeTarget::External) => true,
            (Volume::Internal(local), VolumeTarget::Internal(config)) => &local.config == config,
            _ => false,
        }
    }
}

impl State for Volume {
    type Target = VolumeTarget;
}

/// Target for a volume referenced by a release
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(
    from = "ExternalOr<VolumeTargetPayload>",
    into = "ExternalOr<VolumeTargetPayload>"
)]
pub enum VolumeTarget {
    External,
    Internal(VolumeConfig),
}

/// Payload of an internal [`VolumeTarget`]
#[derive(Serialize, Deserialize)]
struct VolumeTargetPayload {
    #[serde(default)]
    config: VolumeConfig,
}

impl From<VolumeTarget> for ExternalOr<VolumeTargetPayload> {
    fn from(tgt: VolumeTarget) -> Self {
        match tgt {
            VolumeTarget::External => Self::external(),
            VolumeTarget::Internal(config) => Self::internal(VolumeTargetPayload { config }),
        }
    }
}

impl From<ExternalOr<VolumeTargetPayload>> for VolumeTarget {
    fn from(repr: ExternalOr<VolumeTargetPayload>) -> Self {
        repr.into_internal()
            .map_or(VolumeTarget::External, |p| VolumeTarget::Internal(p.config))
    }
}

impl From<Volume> for VolumeTarget {
    fn from(value: Volume) -> Self {
        match value {
            Volume::External => VolumeTarget::External,
            Volume::Internal(LocalVolume { config, .. }) => VolumeTarget::Internal(config),
        }
    }
}

impl From<VolumeTarget> for Volume {
    fn from(value: VolumeTarget) -> Self {
        match value {
            VolumeTarget::External => Volume::External,
            VolumeTarget::Internal(config) => Volume::Internal(LocalVolume {
                // the name is only known once the volume is created on the engine
                oci_name: String::new(),
                config,
            }),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct VolumeConfig(oci::VolumeConfig);

impl Deref for VolumeConfig {
    type Target = oci::VolumeConfig;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for VolumeConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<RemoteVolume> for VolumeConfig {
    fn from(vol: RemoteVolume) -> Self {
        VolumeConfig(oci::VolumeConfig {
            driver: vol.driver.map(VolumeDriver::from).unwrap_or_default(),
            driver_opts: vol.driver_opts.into_iter().collect(),
            labels: vol.labels.into_iter().collect(),
        })
    }
}

impl From<RemoteVolume> for VolumeTarget {
    fn from(vol: RemoteVolume) -> Self {
        VolumeTarget::Internal(vol.into())
    }
}

impl<N> From<oci::LocalVolume<N>> for LocalVolume {
    fn from(vol: oci::LocalVolume<N>) -> Self {
        let volume_name = vol.name;
        let mut labels = vol.labels;

        // Remove supervisor metadata including legacy labels
        labels.retain(|label, _| {
            [
                LABEL_SUPERVISED,
                LABEL_VOLUME_NAME,
                LABEL_APP_UUID,
                LABEL_SUPERVISED_LEGACY,
            ]
            .iter()
            .all(|l| l != label)
        });

        LocalVolume {
            oci_name: volume_name,
            config: VolumeConfig(oci::VolumeConfig {
                driver: vol.driver,
                driver_opts: vol.driver_opts,
                labels,
            }),
        }
    }
}

impl<N> From<oci::LocalVolume<N>> for Volume {
    fn from(vol: oci::LocalVolume<N>) -> Self {
        Volume::Internal(vol.into())
    }
}

impl VolumeConfig {
    pub fn into_oci_config(self, vol_name: &str) -> oci::VolumeConfig {
        let mut inner = self.0;

        // Mark the volume as supervised
        inner
            .labels
            .insert(LABEL_SUPERVISED.to_string(), "".to_string());

        // Add app metadata
        inner
            .labels
            .insert(LABEL_VOLUME_NAME.to_string(), vol_name.to_string());

        inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_model;

    #[test]
    fn test_conversion_preserves_all_fields() {
        let remote: remote_model::Volume = serde_json::from_value(serde_json::json!({
            "driver": "local",
            "driver_opts": {
                "o": "bind",
                "type": "none",
                "device": "/tmp/helios"
            },
            "labels": {"com.foo.bar": "app-label"}
        }))
        .unwrap();

        let config: VolumeConfig = remote.into();
        assert_eq!(config.driver.to_string(), "local");
        assert_eq!(config.driver_opts.get("o"), Some(&"bind".to_string()));
        assert_eq!(config.driver_opts.get("type"), Some(&"none".to_string()));
        assert_eq!(
            config.driver_opts.get("device"),
            Some(&"/tmp/helios".to_string())
        );
        assert_eq!(
            config.labels.get("com.foo.bar"),
            Some(&"app-label".to_string())
        );
    }

    #[test]
    fn test_volume_config_default() {
        let config = VolumeConfig::default();
        assert_eq!(config.driver, VolumeDriver::default());
        assert!(config.driver_opts.is_empty());
        assert!(config.labels.is_empty());
    }

    #[test]
    fn test_conversion_defaults_driver_when_absent() {
        let remote: remote_model::Volume = serde_json::from_value(serde_json::json!({})).unwrap();

        let config: VolumeConfig = remote.into();
        assert_eq!(config.driver.to_string(), "local");
    }

    #[test]
    fn test_to_oci_config_injects_supervised_label() {
        let config = VolumeConfig(oci::VolumeConfig {
            driver: VolumeDriver::from("local".to_string()),
            driver_opts: [("o".to_string(), "bind".to_string())]
                .into_iter()
                .collect(),
            labels: [("com.example.label".to_string(), "value".to_string())]
                .into_iter()
                .collect(),
        });

        let oci_config: oci::VolumeConfig = config.into_oci_config("my-vol");

        assert_eq!(oci_config.driver.to_string(), "local");
        assert_eq!(oci_config.driver_opts.get("o"), Some(&"bind".to_string()));
        assert_eq!(
            oci_config.labels.get("com.example.label"),
            Some(&"value".to_string())
        );
        assert_eq!(
            oci_config.labels.get("io.balena.supervised"),
            Some(&"".to_string())
        );
        assert_eq!(
            oci_config.labels.get(LABEL_VOLUME_NAME),
            Some(&"my-vol".to_string())
        );
    }

    #[test]
    fn test_from_local_volume_strips_injected_labels() {
        let local: oci::LocalVolume = oci::LocalVolume {
            name: "app1_my-vol".to_string(),
            driver: VolumeDriver::from("local".to_string()),
            driver_opts: [("o".to_string(), "bind".to_string())]
                .into_iter()
                .collect(),
            labels: [
                (LABEL_SUPERVISED.to_string(), "".to_string()),
                ("com.example.label".to_string(), "value".to_string()),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };

        let volume: LocalVolume = local.into();

        assert_eq!(volume.oci_name, "app1_my-vol");
        assert!(!volume.config.labels.contains_key(LABEL_SUPERVISED));
        assert_eq!(
            volume.config.labels.get("com.example.label"),
            Some(&"value".to_string())
        );
        assert_eq!(volume.config.driver.to_string(), "local");
        assert_eq!(
            volume.config.driver_opts.get("o"),
            Some(&"bind".to_string())
        );
    }

    #[test]
    fn test_external_volume_serialization() {
        let volume = Volume::External;

        assert_eq!(
            serde_json::to_value(&volume).unwrap(),
            serde_json::json!({"external": true})
        );
        assert_eq!(
            serde_json::from_value::<Volume>(serde_json::json!({"external": true})).unwrap(),
            volume
        );
    }

    #[test]
    fn test_managed_volume_serialization() {
        let volume = Volume::Internal(LocalVolume {
            oci_name: "app1_my-vol".to_string(),
            config: VolumeConfig(oci::VolumeConfig {
                driver: VolumeDriver::from("local".to_string()),
                driver_opts: [("o".to_string(), "bind".to_string())]
                    .into_iter()
                    .collect(),
                labels: Default::default(),
            }),
        });

        let serialized = serde_json::to_value(&volume).unwrap();
        assert_eq!(
            serialized,
            serde_json::json!({
                "oci_name": "app1_my-vol",
                "config": {
                    "driver": "local",
                    "driver_opts": {"o": "bind"}
                }
            })
        );
        assert_eq!(
            serde_json::from_value::<Volume>(serialized).unwrap(),
            volume
        );
    }

    #[test]
    fn test_volume_target_serialization() {
        assert_eq!(
            serde_json::to_value(VolumeTarget::External).unwrap(),
            serde_json::json!({"external": true})
        );
        assert_eq!(
            serde_json::from_value::<VolumeTarget>(serde_json::json!({"external": true})).unwrap(),
            VolumeTarget::External
        );
        assert_eq!(
            serde_json::to_value(VolumeTarget::Internal(VolumeConfig::default())).unwrap(),
            serde_json::json!({"config": {"driver": "local"}})
        );
    }

    #[test]
    fn test_volume_deserialization_defaults_to_managed() {
        let volume: Volume = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(volume, Volume::Internal(LocalVolume::default()));

        let target: VolumeTarget = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(target, VolumeTarget::Internal(VolumeConfig::default()));
    }
}
