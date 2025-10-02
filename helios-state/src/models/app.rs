use mahler::State;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::util::types::Uuid;

use super::release::Release;

/// The internal state of the app
#[derive(State, Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct App {
    /// App id on the remote backend. This only exists for legacy reasons
    /// and should be removed at some point.
    pub id: u32,

    /// The app name provided by the backend
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// App releases
    #[serde(default)]
    pub releases: BTreeMap<Uuid, Release>,
}

pub type TargetAppMap = BTreeMap<Uuid, AppTarget>;
