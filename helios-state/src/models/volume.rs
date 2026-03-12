use std::ops::{Deref, DerefMut};

use mahler::state::State;
use serde::{Deserialize, Serialize};

use crate::common_types::Uuid;
use crate::labels::{LABEL_APP_UUID, LABEL_SUPERVISED};
use crate::oci::{self, LocalVolume, VolumeDriver};
use crate::remote_model::Volume as RemoteVolume;

#[derive(Default, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    #[serde(default)]
    pub volume_name: String,
    #[serde(default)]
    pub config: VolumeConfig,
}

impl State for Volume {
    type Target = Self;
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

impl From<RemoteVolume> for Volume {
    fn from(vol: RemoteVolume) -> Self {
        Volume {
            // Filled in during normalization
            volume_name: String::new(),
            config: vol.into(),
        }
    }
}

impl From<LocalVolume> for Volume {
    fn from(vol: LocalVolume) -> Self {
        let volume_name = vol.name;
        let mut labels = vol.labels;

        // Remove labels injected during create that are not part of the
        // compose definition
        labels.remove(LABEL_SUPERVISED);
        labels.remove(LABEL_APP_UUID);

        Volume {
            volume_name,
            config: VolumeConfig(oci::VolumeConfig {
                driver: vol.driver,
                driver_opts: vol.driver_opts,
                labels,
            }),
        }
    }
}

impl VolumeConfig {
    pub fn into_oci_config(self, app_uuid: &Uuid) -> oci::VolumeConfig {
        let mut inner = self.0;

        // Mark the volume as supervised
        inner
            .labels
            .insert(LABEL_SUPERVISED.to_string(), "".to_string());

        // Add app metadata
        inner
            .labels
            .insert(LABEL_APP_UUID.to_string(), app_uuid.to_string());

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

        let oci_config: oci::VolumeConfig = config.into_oci_config(&Uuid::from("aaa123"));

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
            oci_config.labels.get("io.balena.app-uuid"),
            Some(&"aaa123".to_string())
        );
    }

    #[test]
    fn test_from_local_volume_strips_injected_labels() {
        let local = LocalVolume {
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
        };

        let volume: Volume = local.into();

        assert_eq!(volume.volume_name, "app1_my-vol");
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
}
