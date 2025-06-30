use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub type Uuid = String;

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Image {
    docker_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
/// Current state of a device
pub struct Device {
    /// The device UUID
    pub uuid: Uuid,

    /// List of docker images on the device
    pub images: HashMap<String, Image>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
/// Target state of a device
pub struct TargetDevice {}
