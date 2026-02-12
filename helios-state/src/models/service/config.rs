use mahler::state::{List, Map, State};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::oci::{ContainerConfig, ImageConfig, LocalContainer};

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

impl From<(&ImageConfig, LocalContainer)> for ServiceConfig {
    /// Convert from an OCI container to a service configuration
    fn from((image, container): (&ImageConfig, LocalContainer)) -> Self {
        // for now we only use the container config, but as the
        // ServiceConfig type grows, there will be other conversions from the
        // container metadata into the service config
        let config = container.config;

        let mut labels = config.labels;

        // Remove labels that are present on the image from the labels
        // object defined for the service. Make an exception if there is a
        // io.balena.private.label.<label> label, which means the label is in both
        // image and service composition
        if let Some(labels) = labels.as_mut() {
            let private_labels: HashSet<_> = labels
                .extract_if(|k, _| k.starts_with("io.balena.private.label."))
                .filter_map(|(k, _)| k.strip_prefix("io.balena.private.label.").map(String::from))
                .collect();

            labels.retain(|k, v| {
                image.labels.as_ref().and_then(|l| l.get(k)) != Some(v)
                    || private_labels.contains(k)
            });
        }

        // Remove the supervised label to skip it in the state comparison
        labels
            .as_mut()
            .and_then(|value| value.remove("io.balena.supervised"));

        // if the command on the container equals the command on the image, do
        // not add it to the service configuration
        let command = match (&image.cmd, config.cmd) {
            (None, Some(cmd)) => Some(cmd),
            (Some(_), None) => None,
            (Some(img_cmd), Some(svc_cmd)) => {
                // if the cmd is defined in both the image and the composition, a label
                // will be present indicating the re-definition
                let duplicate_cmd = labels
                    .as_mut()
                    .and_then(|value| value.remove("io.balena.private.cmd"));
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
            container_name: Some(container.name),
            command,
            labels: labels.unwrap_or_default().into_iter().collect(),
        }
    }
}

/// Mark duplicate configuration between container and image by adding private labels.
///
/// This adds `io.balena.private.<property>[.<name>]` if the property is defined in both.
///
/// For instance if the label `some-label` is defined in both container config and image with the
/// same name, it adds a `io.balena.private.some-label` label to the container config to mark the
/// duplicate.
/// `io.balena.private.label.<label>` for each label with the same value in both.
pub fn mark_duplicate_service_config(container: &mut ContainerConfig, image: &ImageConfig) {
    let labels = container.labels.get_or_insert_with(Default::default);

    // Mark duplicate cmd
    if let (Some(img_cmd), Some(ctr_cmd)) = (&image.cmd, &container.cmd)
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
}

impl From<ServiceConfig> for ContainerConfig {
    /// Convert from a service configuration to container options
    fn from(svc_config: ServiceConfig) -> ContainerConfig {
        let ServiceConfig {
            command,
            mut labels,
            ..
        } = svc_config;

        // Set the supervised label when converting to a container
        let _ = labels.insert("io.balena.supervised".to_string(), "".to_string());

        Self {
            cmd: command.map(|c| c.into_iter().collect()),
            labels: Some(labels.into_iter().collect()),
        }
    }
}
