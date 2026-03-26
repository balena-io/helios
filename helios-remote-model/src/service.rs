use std::collections::HashMap;

use serde::Deserialize;

use crate::common_types::ImageUri;

use super::command::Command;
use super::labels::Labels;
use super::restart_policy::RestartPolicy;

/// Target service as defined by the remote backend
#[derive(Deserialize, Clone, Debug)]
pub struct Service {
    pub id: u32,
    pub image: ImageUri,

    #[serde(default)]
    pub labels: HashMap<String, String>,

    #[serde(default)]
    pub composition: ServiceComposition,
}

// FIXME: add remaining fields
#[derive(Deserialize, Clone, Debug, Default)]
pub struct ServiceComposition {
    #[serde(default)]
    pub restart: RestartPolicy,

    #[serde(default)]
    pub command: Option<Command>,

    #[serde(default)]
    pub labels: Labels,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_defaults_restart_to_always() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(comp.restart, RestartPolicy::Always);
    }

    #[test]
    fn composition_with_restart_policy() {
        let comp: ServiceComposition =
            serde_json::from_value(serde_json::json!({"restart": "on-failure:5"})).unwrap();
        assert_eq!(
            comp.restart,
            RestartPolicy::OnFailure {
                max_retries: Some(5)
            }
        );
    }
}
