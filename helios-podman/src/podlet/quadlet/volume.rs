// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Adapted from https://github.com/containers/podlet/
use std::{ops::Not, path::PathBuf};

use serde::Serialize;

use crate::podlet::serde::quadlet::seq_quote_whitespace;

use super::{Downgrade, DowngradeError, HostPaths, PodmanVersion, push_arg};

#[derive(Serialize, Debug, Default, Clone, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct Volume {
    /// The (optional) name of the Podman volume.
    #[allow(clippy::struct_field_names)]
    pub volume_name: Option<String>,

    /// If enabled, the content of the image located at the mount point of the volume
    /// is copied into the volume on the first run.
    #[serde(skip_serializing_if = "Not::not")]
    pub copy: bool,

    /// The path of a device which is mounted for the volume.
    pub device: Option<PathBuf>,

    /// Specify the volume driver name.
    pub driver: Option<String>,

    /// The host (numeric) GID, or group name to use as the group for the volume.
    pub group: Option<String>,

    /// Specifies the image the volume is based on when `Driver` is set to the `image`.
    pub image: Option<String>,

    /// Set one or more OCI labels on the volume.
    #[serde(serialize_with = "seq_quote_whitespace")]
    pub label: Vec<String>,

    /// The mount options to use for a filesystem as used by the `mount` command -o option.
    pub options: Option<String>,

    /// This key contains a list of arguments passed directly to the end of the `podman volume create`
    /// command in the generated file, right before the name of the network in the command line.
    pub podman_args: Option<String>,

    /// The filesystem type of `Device` as used by the `mount` commands `-t` option.
    #[serde(rename = "Type")]
    pub fs_type: Option<String>,

    /// The host (numeric) UID, or user name to use as the owner for the volume.
    pub user: Option<String>,
}

impl HostPaths for Volume {
    fn host_paths(&mut self) -> impl Iterator<Item = &mut PathBuf> {
        self.device.iter_mut()
    }
}

impl Volume {
    /// Add `--{flag} {arg}` to `PodmanArgs=`.
    fn push_arg(&mut self, flag: &str, arg: &str) {
        let podman_args = self.podman_args.get_or_insert_default();
        push_arg(podman_args, flag, arg);
    }
}

impl Downgrade for Volume {
    fn downgrade(&mut self, version: PodmanVersion) -> Result<(), DowngradeError> {
        if version < PodmanVersion::V4_8 {
            if let Some(driver) = self.driver.take() {
                self.push_arg("driver", &driver);
            }

            if let Some(image) = self.image.take() {
                self.push_arg("opt", &format!("image={image}"));
            }
        }

        if version < PodmanVersion::V4_6
            && let Some(podman_args) = self.podman_args.take()
        {
            return Err(DowngradeError::Option {
                quadlet_option: "PodmanArgs",
                value: podman_args,
                supported_version: PodmanVersion::V4_6,
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_default_empty() -> Result<(), crate::podlet::serde::quadlet::Error> {
        let volume = Volume::default();
        assert_eq!(
            crate::podlet::serde::quadlet::to_string_join_all(volume)?,
            "[Volume]\n"
        );
        Ok(())
    }
}
