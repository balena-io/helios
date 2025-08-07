use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::util::docker::normalise_image_name;

/// Deserializes and normalizes the image name
fn deserialize_image_name_from_str<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let img: &str = serde::Deserialize::deserialize(deserializer)?;
    let img = normalise_image_name(img).map_err(serde::de::Error::custom)?;
    Ok(img)
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Service {
    pub id: u32,
    pub image: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TargetService {
    /// Service ID on the remote backend
    #[serde(default)]
    pub id: u32,

    /// Service image URI
    #[serde(deserialize_with = "deserialize_image_name_from_str")]
    pub image: String,
}

impl From<Service> for TargetService {
    fn from(s: Service) -> Self {
        let Service { id, image } = s;
        Self { id, image }
    }
}

pub type ServiceMap = HashMap<String, Service>;
pub type TargetServiceMap = HashMap<String, TargetService>;
