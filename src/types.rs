use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::ops::Deref;

use crate::util::crypto::{pseudorandom_string, ALPHA_NUM};

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

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApiKey(String);

impl Deref for ApiKey {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Default for ApiKey {
    fn default() -> Self {
        Self(pseudorandom_string(ALPHA_NUM, 32))
    }
}

impl Display for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<String> for ApiKey {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<ApiKey> for String {
    fn from(value: ApiKey) -> Self {
        value.0
    }
}
