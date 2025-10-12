use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{Span, debug, field, instrument};

use crate::util::config::StoredConfig;
use crate::util::crypto::sha256_hex_digest;
use crate::util::http::{InvalidUriError, Uri};
use crate::util::store;
use crate::util::types::{ApiKey, DeviceType, Uuid};

use super::config::{ProvisioningConfig, RemoteConfig};

#[derive(Debug, Error)]
pub enum ProvisioningError {
    #[error("Invalid remote endpoint URI: {0}")]
    InvalidRemote(#[from] InvalidUriError),

    #[error("Request encoding failed: {0}")]
    RequestEncoding(#[from] serde_json::Error),

    #[error("Response decoding failed: {0}")]
    ResponseDecoding(#[from] reqwest::Error),

    #[error("Failed to read/write provisioning config: {0}")]
    ReadWriteConfig(#[from] store::StoreError),

    #[error("Remote returned error: ({0}) {1}")]
    Status(StatusCode, String),
}

pub async fn provision(
    provisioning_key: &String,
    provisioning_config: &ProvisioningConfig,
    config_store: &store::Store,
) -> Result<(Uuid, RemoteConfig, DeviceType), ProvisioningError> {
    // Remote expects us to provide a UUID and API key during registration,
    // and we comply by auto-generating random values if they aren't provided
    // as arguments to the CLI.
    //
    // This however means that should an error or a crash occur after we
    // register successfully but before we stored the provisioning config
    // (which completes provisioning from our perspective), we'd loose these
    // values and try to register again on restart with new values and so on,
    // effectively DoS'ing remote and creating a mess of device entries to
    // the provisioned fleet.

    let api_endpoint = &provisioning_config.remote.api_endpoint;
    let recovery_filename = format!(
        "provisioning.{}",
        sha256_hex_digest(api_endpoint.clone().to_string()),
    );

    // Recover a pending config if one exists for this remote.
    // If it does, then it means we failed half-way through provisioning,
    // so we need to ensure that attempting to register with it will
    // return an HTTP 409 Conflict error. If it does, we can assume
    // provisioning is complete, store our config and get rid of the
    // recovered file.
    let config = if let Some(config) = config_store.read("/", &recovery_filename).await? {
        config
    } else {
        // Before attempting to call remote, backup the registration request
        config_store
            .write("/", &recovery_filename, provisioning_config)
            .await?;
        provisioning_config.clone()
    };

    let timeout = &config.remote.request.timeout;
    let request: RegisterRequest = config.clone().into();

    match register(provisioning_key, api_endpoint, timeout, &request).await {
        Ok(_) | Err(ProvisioningError::Status(StatusCode::CONFLICT, _)) => {
            config_store
                .write("/", ProvisioningConfig::default_name(), &config)
                .await
                .map(|_| {
                    // remove recovery config ignoring any errors
                    _ = config_store.delete("/", &recovery_filename);
                })?;
            Ok((
                config.uuid.clone(),
                config.remote.clone(),
                config.device_type.clone(),
            ))
        }
        Err(err) => Err(err),
    }
}

/*
    request {
        user?
        uuid
        application
        device_type
        api_key?
        supervisor_version?
        os_variant?
        os_version?
        mac_address?
    }
*/
#[derive(Clone, Debug, Serialize, Deserialize)]
struct RegisterRequest {
    uuid: Uuid,
    api_key: Option<ApiKey>,
    application: u32, // FIXME: on the backend, this can be inferred from provisioning key
    device_type: String,
}

impl From<ProvisioningConfig> for RegisterRequest {
    fn from(value: ProvisioningConfig) -> Self {
        Self {
            uuid: value.uuid,
            api_key: Some(value.remote.api_key), // the api allows this to be nil but we don't want to
            application: value.fleet,
            device_type: value.device_type,
        }
    }
}

/*
    response {
        id: device.id,
        uuid: device.uuid,
        api_key: apiKey,
    };
*/
#[derive(Clone, Debug, Deserialize)]
struct RegisterResponse {
    // id: u32,
    uuid: Uuid,
    api_key: ApiKey,
}

// TODO: In the future we may want to wrap or extend this function
// to perform the complete `join` action, rather than merely
// registering with the remote.
#[instrument(skip_all, fields(result=field::Empty), err)]
async fn register(
    provisioning_key: &String,
    api_endpoint: &Uri,
    timeout: &Duration,
    request: &RegisterRequest,
) -> Result<(), ProvisioningError> {
    let client = Client::new();
    let endpoint = Uri::from_parts(api_endpoint.clone(), "/device/register", None)?;

    debug!("calling remote");
    let response = client
        .post(endpoint.to_string())
        .bearer_auth(provisioning_key)
        .timeout(*timeout)
        .json(&request)
        .send()
        .await?;

    if !response.status().is_success() {
        let err_code = response.status();
        let err_msg = response.text().await.unwrap();
        return Err(ProvisioningError::Status(err_code, err_msg));
    }

    Span::current().record("result", field::display(response.status()));

    let response: RegisterResponse = response.json().await?;
    assert_eq!(request.uuid, response.uuid);
    assert_eq!(request.api_key, Some(response.api_key));

    Ok(())
}
