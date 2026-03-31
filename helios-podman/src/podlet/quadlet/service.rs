// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Adapted from https://github.com/containers/podlet/
use serde::Serialize;

/// The `[Service]` section of a systemd unit / Quadlet file.
///
/// Only includes options needed to convert [Podman CLI](crate::cli::PodmanCommands) and
/// [`Compose`](compose_spec::Compose) files.
#[derive(Serialize, Default, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub struct Service {
    /// Configure if and when the service should be restarted.
    pub restart: Option<RestartConfig>,
}

impl Service {
    /// Returns `true` if all fields are [`None`].
    pub fn is_empty(&self) -> bool {
        let Self { restart } = self;

        restart.is_none()
    }
}

impl From<RestartConfig> for Service {
    fn from(restart: RestartConfig) -> Self {
        Self {
            restart: Some(restart),
        }
    }
}

/// Possible service restart configurations.
///
/// From [systemd.service](https://www.freedesktop.org/software/systemd/man/systemd.service.html#Restart=).
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RestartConfig {
    No,
    OnSuccess,
    OnFailure,
    OnAbnormal,
    OnWatchdog,
    OnAbort,
    Always,
}
