
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Image {
    /// Container engine id
    pub engine_id: Option<String>,
}
