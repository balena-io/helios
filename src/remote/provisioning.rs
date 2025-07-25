use std::fs;
use std::time::Duration;

use axum::http::Uri;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tracing::{debug, field, warn};

use crate::config;
use crate::remote::{ProvisioningConfig, RemoteConfig};
use crate::types::{ApiKey, Uuid};
use crate::util::crypto::sha256_hex_digest;
use crate::util::fs::safe_write_all;
use crate::util::uri::{make_uri, UriError};

#[derive(Debug, Error)]
pub enum ProvisioningError {
    #[error("Invalid remote endpoint URI: {0}")]
    InvalidRemoteEndpointURI(#[from] UriError),

    #[error("Request encoding failed: {0}")]
    RequestEncoding(#[from] serde_json::Error),

    #[error("Response decoding failed: {0}")]
    ResponseDecoding(#[from] reqwest::Error),

    #[error("Recovery file error: {0}")]
    RecoveryFile(#[from] std::io::Error),

    #[error("Failed to save provisioning config: {0}")]
    SaveConfig(#[from] config::SaveConfigError),

    #[error("Remote returned error: ({0}) {1}")]
    Status(StatusCode, String),
}

pub async fn provision(
    provisioning_key: &String,
    provisioning_config: &ProvisioningConfig,
) -> Result<(Uuid, RemoteConfig), ProvisioningError> {
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

    let recovery_path = config::config_dir().join(format!(
        ".provisioning.{}.json",
        sha256_hex_digest(provisioning_config.remote.api_endpoint.clone().to_string())
    ));

    // Recover a pending config if one exists for this remote.
    // If it does, then it means we failed half-way through provisioning,
    // so we need to ensure that attempting to register with it will
    // return an HTTP 409 Conflict error. If it does, we can assume
    // provisioning is complete, store our config and get rid of the
    // recovered file.
    let config: ProvisioningConfig = match fs::read_to_string(&recovery_path) {
        Ok(contents) => serde_json::from_str(&contents)?,
        Err(_) => {
            // Before attempting to call remote, backup the registration request
            let buf = serde_json::to_vec(&provisioning_config)?;
            safe_write_all(&recovery_path, &buf)?;
            provisioning_config.clone()
        }
    };

    let api_endpoint = &config.remote.api_endpoint;
    let timeout = &config.remote.request.timeout;
    let request: RegisterRequest = config.clone().into();

    match register(provisioning_key, api_endpoint, timeout, &request).await {
        Ok(_) | Err(ProvisioningError::Status(StatusCode::CONFLICT, _)) => {
            config::save(&config).await.map(|_| {
                // remove recovery config and ignore any errors
                _ = fs::remove_file(recovery_path);
            })?;
            Ok((config.uuid.clone(), config.remote.clone()))
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
    // user: Option<String>, // long deprecated in api; unecessary; ignore
    uuid: Uuid,
    application: u32,
    device_type: String,
    api_key: Option<ApiKey>,
    supervisor_version: Option<String>,
    os_version: Option<String>,
    os_variant: Option<String>,
    mac_address: Option<String>,
}

impl From<ProvisioningConfig> for RegisterRequest {
    fn from(value: ProvisioningConfig) -> Self {
        Self {
            uuid: value.uuid,
            application: value.fleet,
            device_type: value.device_type,
            api_key: Some(value.remote.api_key), // the api allows this to be nil but we don't want to
            supervisor_version: value.supervisor_version,
            os_version: value.os_version,
            os_variant: value.os_variant,
            mac_address: value.mac_address,
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

/*
    ProvisioningOptions {
        uuid:               conf.uuid,
        applicationId:      conf.applicationId,
        deviceArch:         conf.deviceArch,
        deviceType:         conf.deviceType,
        provisioningApiKey: conf.apiKey,
        deviceApiKey:       conf.deviceApiKey,
        apiEndpoint:        conf.apiEndpoint,
        apiRequestTimeout:  conf.apiRequestTimeout,
        registered_at:      conf.registered_at,
        deviceId:           conf.deviceId,
        supervisorVersion:  conf.version,
        osVersion:          conf.osVersion,
        osVariant:          conf.osVariant,
        macAddress:         conf.macAddress,
    };
*/
// TODO: In the future we may want to wrap or extend this function
// to perform the complete `join` action, rather than merely
// registering with the remote.
async fn register(
    provisioning_key: &String,
    api_endpoint: &Uri,
    timeout: &Duration,
    request: &RegisterRequest,
) -> Result<(), ProvisioningError> {
    let client = Client::new();
    let endpoint = make_uri(api_endpoint.clone(), "/device/register", None)?;
    let payload = json!(request);

    debug!("calling remote");
    let response = client
        .post(endpoint.to_string())
        .bearer_auth(provisioning_key)
        .timeout(*timeout)
        .json(&payload)
        .send()
        .await?;

    if !response.status().is_success() {
        warn!(
            response = field::display(response.status()),
            "received error response"
        );
        let err_code = response.status();
        let err_msg = response.text().await.unwrap();
        return Err(ProvisioningError::Status(err_code, err_msg));
    }

    debug!(response = field::display(response.status()), "success");

    let response: RegisterResponse = response.json().await?;
    assert_eq!(request.uuid, response.uuid);
    assert_eq!(request.api_key, Some(response.api_key));

    Ok(())
}
