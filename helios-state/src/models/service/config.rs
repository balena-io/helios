use mahler::state::{List, Map, State};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::oci::{ContainerConfig, ImageConfig};

#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct ServiceConfig {
    /// Custom service container name
    pub container_name: Option<String>,

    /// Command to run specified as a list of strings.
    pub command: Option<List<String>>,

    /// User-defined key/value metadata
    pub labels: Map<String, String>,
}

impl State for ServiceConfig {
    type Target = Self;
}

impl ServiceConfig {
    /// Convert from an OCI container to a service configuration
    pub fn from_container_config(name: &str, config: ContainerConfig, image: &ImageConfig) -> Self {
        let mut labels = config.labels.unwrap_or_default();

        // Remove labels that are present on the image from the labels
        // object defined for the service. Make an exception if there is a
        // io.balena.private.label.<label> label, which means the label is in both
        // image and service composition
        let private_labels: HashSet<_> = labels
            .extract_if(|k, _| k.starts_with("io.balena.private.label."))
            .filter_map(|(k, _)| k.strip_prefix("io.balena.private.label.").map(String::from))
            .collect();

        labels.retain(|k, v| {
            image.labels.as_ref().and_then(|l| l.get(k)) != Some(v) || private_labels.contains(k)
        });

        // Remove the supervised label to skip it in the state comparison
        labels.remove("io.balena.supervised");

        // if the command on the container equals the command on the image, do
        // not add it to the service configuration
        let command = match (&image.cmd, config.cmd) {
            (None, Some(cmd)) => Some(cmd),
            (Some(_), None) => None,
            (Some(img_cmd), Some(svc_cmd)) => {
                // if the cmd is defined in both the image and the composition, a label
                // will be present indicating the re-definition
                let duplicate_cmd = labels.remove("io.balena.private.cmd");
                if img_cmd == &svc_cmd && duplicate_cmd.is_none() {
                    None
                } else {
                    Some(svc_cmd)
                }
            }
            (None, None) => None,
        };
        // convert to List<String>
        let command = command.map(|cmd| cmd.into_iter().collect());

        Self {
            container_name: Some(name.to_owned()),
            command,
            labels: labels.into_iter().collect(),
        }
    }

    /// Converts the service config into container configuration
    ///
    /// This adds `io.balena.private.<property>[.<name>]` labels if the property
    /// is defined in both the service config and image config.
    ///
    /// For instance if the label `some-label` is defined in both container config and image with the
    /// same name, it adds a `io.balena.private.some-label` label to the container config to mark the
    /// duplicate.
    pub fn into_container_config(self, image: &ImageConfig) -> ContainerConfig {
        let ServiceConfig {
            command,
            mut labels,
            ..
        } = self;

        // Set the supervised label when converting to a container
        labels.insert("io.balena.supervised".to_string(), "".to_string());

        // Mark duplicate cmd
        let cmd = command.map(|c| c.into_iter().collect());
        if let (Some(img_cmd), Some(ctr_cmd)) = (&image.cmd, &cmd)
            && img_cmd == ctr_cmd
        {
            labels.insert("io.balena.private.cmd".to_string(), String::new());
        }

        // Mark duplicate labels
        if let Some(img_labels) = &image.labels {
            let duplicate_keys: Vec<_> = labels
                .iter()
                .filter(|(k, v)| img_labels.get(*k) == Some(*v))
                .map(|(k, _)| k.clone())
                .collect();

            for key in duplicate_keys {
                labels.insert(format!("io.balena.private.label.{key}"), String::new());
            }
        }

        ContainerConfig {
            cmd,
            labels: Some(labels.into_iter().collect()),
        }
    }
}
