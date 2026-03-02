use mahler::state::{List, Map, State};
use serde::{Deserialize, Serialize};
use serde_json as json;

use crate::oci::ContainerConfig;

const LABEL_CONFIG_FIELDS: &str = "io.balena.private.config.fields";
const LABEL_CONFIG_LABELS: &str = "io.balena.private.config.labels";

#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct ServiceConfig {
    /// Command to run specified as a list of strings.
    pub command: Option<List<String>>,

    /// User-defined key/value metadata
    pub labels: Map<String, String>,
}

impl State for ServiceConfig {
    type Target = Self;
}

impl From<ContainerConfig> for ServiceConfig {
    /// Convert from an OCI container to a service configuration
    fn from(config: ContainerConfig) -> Self {
        let mut labels = config.labels.unwrap_or_default();

        // Remove the supervised label to skip it in the state comparison
        labels.remove("io.balena.supervised");

        // Read the list of fields defined in the composition used to create
        // this container
        let label_config_fields: Vec<String> = labels
            .remove(LABEL_CONFIG_FIELDS)
            .and_then(|s| json::from_str(&s).ok())
            .unwrap_or_default();

        // Remove labels from the container that were not defined in
        // the composition. These are coming from the image and should not be
        // read into the service config
        let label_config_labels_value: Vec<String> = labels
            .remove(LABEL_CONFIG_LABELS)
            .and_then(|s| json::from_str(&s).ok())
            .unwrap_or_default();
        labels.retain(|k, _| label_config_labels_value.contains(k));

        // Read the command if part of the composition fields
        let command = if label_config_fields.contains(&"command".to_string()) {
            // convert the command to List<String>
            config.cmd.map(|cmd| cmd.into_iter().collect())
        } else {
            None
        };

        Self {
            command,
            labels: labels.into_iter().collect(),
        }
    }
}

impl From<ServiceConfig> for ContainerConfig {
    /// Converts the service config into container configuration
    ///
    /// Because some configurations may be defined on the image and the the composition,
    /// this creates custom labels [`LABEL_CONFIG_FIELDS`], [`LABEL_CONFIG_LABELS`] containing a
    /// list of composition defined keys and labels. When reading the container state, these fields
    /// are used to determine if the specific field/label should be read back into the state.
    fn from(svc: ServiceConfig) -> Self {
        let ServiceConfig {
            command,
            mut labels,
            ..
        } = svc;

        // We create a label LABEL_CONFIG_LABELS containing user defined labels on the composition
        // we will use these when reading the state to remove labels coming from
        // the image
        let label_config_labels_value = labels
            .keys()
            .map(|s| json::Value::String(s.to_owned()))
            .collect::<json::Value>();
        labels.insert(
            LABEL_CONFIG_LABELS.to_string(),
            label_config_labels_value.to_string(),
        );

        // List of config fields coming from the composition. This is only necessary for fields that
        // may be shared between the image and service, since docker will use the image version as
        // the default unless overriden
        let mut fields = Vec::new();

        let cmd = command.map(|c| c.into_iter().collect());
        if cmd.is_some() {
            fields.push("command");
        }

        // Create a label LABEL_CONFIG_FIELDS label with all the custom fields
        let label_config_fields_value = fields
            .into_iter()
            .map(|s| json::Value::String(s.to_owned()))
            .collect::<json::Value>();

        labels.insert(
            LABEL_CONFIG_FIELDS.to_string(),
            label_config_fields_value.to_string(),
        );

        // Set the supervised label when converting to a container
        labels.insert("io.balena.supervised".to_string(), "".to_string());

        ContainerConfig {
            cmd,
            labels: Some(labels.into_iter().collect()),
        }
    }
}
