use std::{
    collections::{BTreeSet, HashMap},
    fmt::Display,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;

use crate::oci::ImageUri;
use crate::util::http::{InvalidUriError, Uri};
use crate::util::request::{Get, GetConfig, GetError, RequestConfig};

// See: https://github.com/balena-io/open-balena-api/blob/master/src/lib/config.ts#L476-L479
const REGISTRY_TOKEN_EXPIRE_SECONDS: Duration = Duration::from_secs(4 * 3600);

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash)]
pub struct RegistryAuth {
    service: String,
    // use a BTree to serialize the scope in order
    scope: BTreeSet<ImageUri>,
}

impl RegistryAuth {
    pub fn needs_auth(image: &ImageUri) -> bool {
        image.registry().is_some()
    }

    /// Return true if the image uri is in scope
    pub fn in_scope(&self, image_uri: &ImageUri) -> bool {
        if image_uri
            .registry()
            .as_ref()
            .is_none_or(|service| &self.service != service)
        {
            return false;
        }
        self.scope.iter().any(|scope| scope == image_uri)
    }

    pub fn is_super_scope(&self, other: &RegistryAuth) -> bool {
        self.service == other.service && other.scope.is_subset(&self.scope)
    }
}

#[derive(Error, Debug)]
pub enum RegistryAuthConversionError {
    #[error("cannot create registry auth from empty image list")]
    EmptyImageList,
    #[error("images have different registries, cannot group into single auth")]
    MixedRegistries,
    #[error("image has no registry specified")]
    NoRegistry,
}

impl TryFrom<Vec<ImageUri>> for RegistryAuth {
    type Error = RegistryAuthConversionError;

    fn try_from(images: Vec<ImageUri>) -> Result<Self, Self::Error> {
        if images.is_empty() {
            return Err(RegistryAuthConversionError::EmptyImageList);
        }

        let service = images[0]
            .registry()
            .as_ref()
            .ok_or(RegistryAuthConversionError::NoRegistry)?;

        for image in &images {
            match image.registry().as_ref() {
                None => return Err(RegistryAuthConversionError::NoRegistry),
                Some(registry) if registry != service => {
                    return Err(RegistryAuthConversionError::MixedRegistries);
                }
                _ => {}
            }
        }

        Ok(RegistryAuth {
            service: service.clone(),
            scope: images.into_iter().collect(),
        })
    }
}

#[derive(Deserialize)]
struct ResponseToken {
    pub token: String,
}

#[derive(Clone, Debug)]
pub struct RegistryToken {
    expires: Instant,
    value: String,
}

impl RegistryToken {
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.expires
    }
}

impl Display for RegistryToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.value.fmt(f)
    }
}

impl AsRef<str> for RegistryToken {
    fn as_ref(&self) -> &str {
        &self.value
    }
}

#[derive(Clone, Debug)]
pub struct RegistryAuthClient {
    api_endpoint: Uri,
    config: RequestConfig,
    cached: HashMap<RegistryAuth, RegistryToken>,
}

#[derive(Error, Debug)]
pub enum RegistryAuthError {
    #[error(transparent)]
    InvalidUri(#[from] InvalidUriError),

    #[error(transparent)]
    RequestFailed(#[from] GetError),

    #[error("empty response from remote endpoint")]
    ResponseEmpty,

    #[error("cannot deserialize response into token: {0}")]
    ResponseInvalid(#[from] serde_json::Error),
}

impl RegistryAuthClient {
    pub fn new(api_endpoint: Uri, config: RequestConfig) -> Self {
        Self {
            api_endpoint,
            config,
            cached: HashMap::new(),
        }
    }

    pub fn token(&self, image_uri: &ImageUri) -> Option<&RegistryToken> {
        self.cached
            .iter()
            .find(|(auth, _)| auth.in_scope(image_uri))
            .map(|(_, token)| token)
    }

    /// Request a new authorization
    pub async fn request(&mut self, auth: &RegistryAuth) -> Result<(), RegistryAuthError> {
        let scope_set: BTreeSet<&ImageUri> = auth.scope.iter().collect();

        // Check if we already have a matching cached entry
        for (id, token) in &self.cached {
            if id.service == auth.service
                && !token.is_expired()
                && scope_set.is_subset(&id.scope.iter().collect())
            {
                return Ok(());
            }
        }

        // Create the request uri
        let scope_query = scope_set
            .iter()
            .map(|uri| format!("scope=repository:{}:pull", uri.image()))
            .collect::<Vec<String>>()
            .join("&");

        let endpoint = Uri::from_parts(
            self.api_endpoint.clone(),
            "/auth/v1/token",
            Some(format!("service={}&{scope_query}", auth.service).as_str()),
        )?;

        debug!("requesting token for {}", auth.service);
        let mut client = Get::new(
            endpoint.to_string(),
            GetConfig {
                request: self.config.clone(),
                // Do NOT persist cache or the service will slowly start using disk
                persist_cache: false,
            },
        );
        let response = client.get(None).await?;
        if let Some(value) = response.value {
            let ResponseToken { token } = serde_json::from_value(value)?;
            self.cached.insert(
                auth.clone(),
                RegistryToken {
                    value: token,
                    expires: Instant::now() + REGISTRY_TOKEN_EXPIRE_SECONDS,
                },
            );

            Ok(())
        } else {
            Err(RegistryAuthError::ResponseEmpty)
        }
    }

    /// Clear the client cache
    pub fn clear(&mut self) {
        self.cached.clear();
    }
}
