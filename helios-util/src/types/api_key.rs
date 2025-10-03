use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::ops::Deref;

use crate::crypto::{ALPHA_NUM, pseudorandom_string};

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
