// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Adapted from https://github.com/containers/podlet/
use serde::Serialize;

use crate::podlet::serde::quadlet::seq_quote_whitespace;

/// The `[Install]` section of a systemd unit / Quadlet file.
#[derive(Serialize, Default, Debug, Clone, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct Install {
    /// Add weak parent dependencies to the unit.
    #[serde(serialize_with = "seq_quote_whitespace")]
    pub wanted_by: Vec<String>,

    /// Add stronger parent dependencies to the unit.
    #[serde(serialize_with = "seq_quote_whitespace")]
    pub required_by: Vec<String>,

    /// Add stronger parent dependencies to the unit.
    #[serde(serialize_with = "seq_quote_whitespace")]
    pub upheld_by: Vec<String>,
}

impl Install {
    /// Returns `true` if all fields are empty.
    pub fn is_empty(&self) -> bool {
        let Self {
            wanted_by,
            required_by,
            upheld_by,
        } = self;

        wanted_by.is_empty() && required_by.is_empty() && upheld_by.is_empty()
    }
}
