use mahler::State;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::service::Service;

#[derive(State, Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub struct Release {
    #[serde(default)]
    pub services: BTreeMap<String, Service>,
}
