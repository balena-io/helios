// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Adapted from https://github.com/containers/podlet/

use compose_spec::service::{Condition, Dependency};
use serde::Serialize;
use thiserror::Error;

use crate::podlet::serde::quadlet::seq_quote_whitespace;

/// The `[Unit]` section of a systemd unit / Quadlet file.
///
/// Includes common systemd unit options.
///
/// From [systemd.unit](https://www.freedesktop.org/software/systemd/man/systemd.unit.html).
#[allow(clippy::doc_markdown)]
#[derive(Serialize, Default, Debug, Clone, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct Unit {
    /// Add a description to the unit.
    ///
    /// A description should be a short, human readable title of the unit.
    ///
    /// Converts to "Description=DESCRIPTION".
    pub description: Option<String>,

    /// Add (weak) requirement dependencies to the unit.
    ///
    /// Converts to "Wants=WANTS[ ...]".
    ///
    /// Can be specified multiple times.
    #[serde(serialize_with = "seq_quote_whitespace")]
    pub wants: Vec<String>,

    /// Similar to wants, but adds stronger requirement dependencies.
    ///
    /// Converts to "Requires=REQUIRES[ ...]".
    ///
    /// Can be specified multiple times.
    #[serde(serialize_with = "seq_quote_whitespace")]
    pub requires: Vec<String>,

    /// Similar to requires, but when the dependency stops, this unit also stops.
    ///
    /// Converts to "BindsTo=BINDS_TO[ ...]".
    ///
    /// Can be specified multiple times.
    #[serde(serialize_with = "seq_quote_whitespace")]
    pub binds_to: Vec<String>,

    /// Similar to binds_to, but this unit only stops when the dependency is explicitly stopped.
    ///
    /// Converts to "PartOf=PART_OF[ ...]".
    ///
    /// Can be specified multiple times.
    #[serde(serialize_with = "seq_quote_whitespace")]
    pub part_of: Vec<String>,

    /// Similar to wants, but dependencies are continuously started when inactive or failed.
    ///
    /// Converts to "Upholds=UPHOLDS[ ...]".
    ///
    /// Can be specified multiple times.
    #[serde(serialize_with = "seq_quote_whitespace")]
    pub upholds: Vec<String>,

    /// Configure ordering dependency between units.
    ///
    /// Converts to "Before=BEFORE[ ...]".
    ///
    /// Can be specified multiple times.
    #[serde(serialize_with = "seq_quote_whitespace")]
    pub before: Vec<String>,

    /// Configure ordering dependency between units.
    ///
    /// Converts to "After=AFTER[ ...]".
    ///
    /// Can be specified multiple times.
    #[serde(serialize_with = "seq_quote_whitespace")]
    pub after: Vec<String>,
}

impl Unit {
    /// Returns `true` if all fields are empty or [`None`].
    pub fn is_empty(&self) -> bool {
        let Self {
            description,
            wants,
            requires,
            binds_to,
            part_of,
            upholds,
            before,
            after,
        } = self;

        description.is_none()
            && wants.is_empty()
            && requires.is_empty()
            && binds_to.is_empty()
            && part_of.is_empty()
            && upholds.is_empty()
            && before.is_empty()
            && after.is_empty()
    }

    /// Add a [`Dependency`] to the unit.
    ///
    /// # Errors
    ///
    /// Returns an error if the [`Condition`] is not [`ServiceStarted`](Condition::ServiceStarted)
    /// or the [`Dependency`] is set to `restart` but is not `required`.
    pub fn add_dependency(
        &mut self,
        mut name: String,
        dependency: &Dependency,
    ) -> Result<(), AddDependencyError> {
        match dependency.condition {
            Condition::ServiceStarted => {}
            Condition::ServiceHealthy => {
                return Err(AddDependencyError::UnsupportedCondition {
                    condition: "service_healthy",
                    suggestion: "try using `Notify=healthy` in the [Container] section of the dependency",
                });
            }
            Condition::ServiceCompletedSuccessfully => {
                return Err(AddDependencyError::UnsupportedCondition {
                    condition: "service_completed_successfully",
                    suggestion: "try using `Type=oneshot` in the [Service] section of the dependency",
                });
            }
        }

        // Which list to add the dependency to depends on whether to restart this unit and if the
        // dependency is required (in label format, required is always true).
        let list = match dependency.restart {
            true => &mut self.binds_to,
            false => &mut self.requires,
        };

        name.push_str(".service");
        list.push(name.clone());
        self.after.push(name);

        Ok(())
    }
}

/// Error returned when adding a dependency to a [`Unit`] fails.
#[derive(Error, Debug)]
pub enum AddDependencyError {
    /// Unsupported dependency condition.
    #[error("dependency condition `{condition}` is not directly supported; {suggestion}")]
    UnsupportedCondition {
        condition: &'static str,
        suggestion: &'static str,
    },
}
