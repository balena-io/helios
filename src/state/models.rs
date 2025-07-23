use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Display;
use std::ops::Deref;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct Uuid(String);

impl Deref for Uuid {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Default for Uuid {
    fn default() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }
}

impl Display for Uuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<String> for Uuid {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<Uuid> for String {
    fn from(value: Uuid) -> Self {
        value.0
    }
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Image {
    pub docker_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct App {
    pub name: String,
}

pub type DeviceConfig = HashMap<String, String>;

/// Current state of a device
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Device {
    /// The device UUID
    pub uuid: Uuid,

    /// List of docker images on the device
    pub images: HashMap<String, Image>,

    /// Apps on the device
    pub apps: HashMap<Uuid, App>,

    /// Config vars
    pub config: DeviceConfig,
}

impl Device {
    /// Read the host and apps state from the underlying system
    pub fn initial_for(uuid: Uuid) -> Self {
        // TODO: read initial state from the engine
        Self {
            uuid,
            images: HashMap::new(),
            apps: HashMap::new(),
            config: HashMap::new(),
        }
    }
}

// Alias the App for now, the target app will have
// its own structure eventually
pub type TargetApp = App;

/// Target state of a device
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct TargetDevice {
    pub apps: HashMap<Uuid, TargetApp>,
    pub config: DeviceConfig,
}

impl From<Device> for TargetDevice {
    fn from(device: Device) -> Self {
        let Device { apps, config, .. } = device;
        Self { apps, config }
    }
}
