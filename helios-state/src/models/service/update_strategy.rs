use super::ServiceConfig;

const LABEL_UPDATE_STRATEGY: &str = "io.balena.update.strategy";

/// How a service is replaced by its counterpart in a new release.
///
/// See <https://docs.balena.io/learn/deploy/release-strategy/update-strategies>
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpdateStrategy {
    /// Pull the new image, then stop the old service
    #[default]
    DownloadThenKill,
    /// Stop the old service before pulling the new image
    KillThenDownload,
    /// As `KillThenDownload`, also deleting the old image before the pull
    DeleteThenDownload,
}

impl UpdateStrategy {
    /// True for the strategies that stop the old service before pulling
    pub fn kills_before_download(self) -> bool {
        matches!(self, Self::KillThenDownload | Self::DeleteThenDownload)
    }
}

impl ServiceConfig {
    /// The update strategy set by the service labels. Target values are
    /// validated by helios-remote-model; unknown values on existing containers
    /// fall back to the default.
    pub fn update_strategy(&self) -> UpdateStrategy {
        match self.labels.get(LABEL_UPDATE_STRATEGY).map(String::as_str) {
            Some("kill-then-download") => UpdateStrategy::KillThenDownload,
            Some("delete-then-download") => UpdateStrategy::DeleteThenDownload,
            _ => UpdateStrategy::DownloadThenKill,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::ContainerConfig;

    fn config_with(labels: &[(&str, &str)]) -> ServiceConfig {
        ServiceConfig(ContainerConfig {
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        })
    }

    #[test]
    fn reads_the_strategy_from_the_labels() {
        for (value, expected) in [
            ("download-then-kill", UpdateStrategy::DownloadThenKill),
            ("kill-then-download", UpdateStrategy::KillThenDownload),
            ("delete-then-download", UpdateStrategy::DeleteThenDownload),
        ] {
            let config = config_with(&[(LABEL_UPDATE_STRATEGY, value)]);
            assert_eq!(config.update_strategy(), expected, "label value {value}");
        }
    }

    #[test]
    fn falls_back_to_download_then_kill() {
        assert_eq!(
            config_with(&[]).update_strategy(),
            UpdateStrategy::DownloadThenKill
        );
        assert_eq!(
            config_with(&[(LABEL_UPDATE_STRATEGY, "invalid")]).update_strategy(),
            UpdateStrategy::DownloadThenKill
        );
    }
}
