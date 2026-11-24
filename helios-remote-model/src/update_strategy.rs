//! Validation of the `io.balena.update.strategy` service label

use super::Service;

const LABEL_UPDATE_STRATEGY: &str = "io.balena.update.strategy";

const UPDATE_STRATEGIES: [&str; 3] = [
    "download-then-kill",
    "kill-then-download",
    "delete-then-download",
];

/// Reject an unknown update strategy, rather than silently falling back to the
/// default. Top level service labels take priority over the composition labels.
pub(crate) fn validate_update_strategy(svc_name: &str, svc: &Service) -> Result<(), String> {
    let Some(strategy) = svc
        .labels
        .get(LABEL_UPDATE_STRATEGY)
        .or_else(|| svc.composition.labels.get(LABEL_UPDATE_STRATEGY))
    else {
        return Ok(());
    };
    if UPDATE_STRATEGIES.contains(&strategy.as_str()) {
        return Ok(());
    }
    Err(format!(
        "service '{svc_name}' has unknown update strategy '{strategy}', expected one of {}",
        UPDATE_STRATEGIES.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::Release;

    fn release_with(
        labels: Value,
        composition_labels: Value,
    ) -> Result<Release, serde_json::Error> {
        serde_json::from_value(json!({
            "services": {
                "main": {
                    "id": 1,
                    "image": "alpine:latest",
                    "labels": labels,
                    "composition": {"labels": composition_labels}
                }
            }
        }))
    }

    #[test]
    fn accepts_known_strategies() {
        for strategy in super::UPDATE_STRATEGIES {
            let labels = json!({"io.balena.update.strategy": strategy});
            release_with(labels.clone(), json!({})).unwrap();
            release_with(json!({}), labels).unwrap();
        }
        release_with(json!({}), json!({})).unwrap();
    }

    #[test]
    fn rejects_an_unknown_strategy() {
        let labels = json!({"io.balena.update.strategy": "invalid"});
        for (labels, composition_labels) in [(labels.clone(), json!({})), (json!({}), labels)] {
            let err = release_with(labels, composition_labels).unwrap_err();
            assert!(
                err.to_string()
                    .contains("service 'main' has unknown update strategy 'invalid'"),
                "{err}"
            );
        }
    }

    #[test]
    fn top_level_label_takes_priority() {
        release_with(
            json!({"io.balena.update.strategy": "kill-then-download"}),
            json!({"io.balena.update.strategy": "bogus"}),
        )
        .unwrap();
        release_with(
            json!({"io.balena.update.strategy": "bogus"}),
            json!({"io.balena.update.strategy": "kill-then-download"}),
        )
        .unwrap_err();
    }
}
