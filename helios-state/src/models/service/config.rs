use std::ops::{Deref, DerefMut};

use mahler::state::State;
use serde::{Deserialize, Serialize};
use serde_json as json;

use crate::common_types::Uuid;
use crate::labels::{LABEL_APP_UUID, LABEL_SERVICE_ID, LABEL_SERVICE_NAME, LABEL_SUPERVISED};
use crate::oci;

const LABEL_CONFIG_FIELDS: &str = "io.balena.private.config.fields";
const LABEL_CONFIG_LABELS: &str = "io.balena.private.config.labels";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct ServiceConfig(pub(super) oci::ContainerConfig);

impl State for ServiceConfig {
    type Target = Self;
}

impl Deref for ServiceConfig {
    type Target = oci::ContainerConfig;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ServiceConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<oci::ContainerConfig> for ServiceConfig {
    /// Convert from an OCI container to a service configuration
    fn from(mut config: oci::ContainerConfig) -> Self {
        let labels = &mut config.labels;

        // Remove the supervised label to skip it in the state comparison
        labels.remove(LABEL_SUPERVISED);
        labels.remove(LABEL_APP_UUID);
        labels.remove(LABEL_SERVICE_NAME);

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

        // Keep the command the command if part of the composition fields
        if !label_config_fields.contains(&"command".to_string()) {
            config.command = None;
        }

        Self(config)
    }
}

impl ServiceConfig {
    /// Converts the service config into container configuration
    ///
    /// Because some configurations may be defined on the image and the the composition,
    /// this creates custom labels [`LABEL_CONFIG_FIELDS`], [`LABEL_CONFIG_LABELS`] containing a
    /// list of composition defined keys and labels. When reading the container state, these fields
    /// are used to determine if the specific field/label should be read back into the state.
    pub fn into_oci_config(
        self,
        svc_id: u32,
        svc_name: &str,
        app_uuid: &Uuid,
    ) -> oci::ContainerConfig {
        let mut config = self.0;
        let labels = &mut config.labels;

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

        if config.command.is_some() {
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

        // Set app and service metadata as labels when creating the container
        labels.insert(LABEL_SUPERVISED.to_string(), "".to_string());
        labels.insert(LABEL_APP_UUID.to_string(), app_uuid.to_string());
        labels.insert(LABEL_SERVICE_NAME.to_string(), svc_name.to_string());
        labels.insert(LABEL_SERVICE_ID.to_string(), svc_id.to_string());

        config
    }
}
