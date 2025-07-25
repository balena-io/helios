use axum::http::Uri;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, field, warn};

use crate::state::models::Uuid;
use crate::util::uri::{make_uri, UriError};

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

    request {
        user?
        application
        uuid
        device_type
        api_key?
        supervisor_version?
        os_variant?
        os_version?
        mac_address?
    },

    response {
        id: device.id,
        uuid: device.uuid,
        api_key: apiKey,
    };
*/

#[derive(Debug, Error)]
pub enum RegisterError {
    #[error("Invalid remote endpoint URI: {0}")]
    InvalidRemoteEndpointURI(#[from] UriError),

    #[error("JSON serialization failed: {0}")]
    ResponseDerialization(#[from] reqwest::Error),

    #[error("Remote returned error: ({0}) {1}")]
    Status(StatusCode, String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub uuid: Uuid,
    pub application: u32,
    pub device_type: String,
    pub api_key: String, // the api allows this to be nil but we don't want to
    pub supervisor_version: Option<String>,
    pub os_version: Option<String>,
    pub os_variant: Option<String>,
    pub mac_address: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RegisterResponse {
    // pub id: u32,
    pub uuid: Uuid,
    pub api_key: String,
}

// TODO: In the future we may want to wrap or extend this function
// to perform the complete `join` action, rather than merely
// registering with the remote.
pub async fn register(
    provisioning_key: String,
    api_endpoint: Uri,
    request_timeout: Duration,
    register_request: RegisterRequest,
) -> Result<RegisterResponse, RegisterError> {
    let client = Client::new();
    let endpoint = make_uri(api_endpoint.clone(), "/device/register", None)?.to_string();
    let payload = json!(register_request);

    debug!("calling remote");
    let response = client
        .post(&endpoint)
        .bearer_auth(&provisioning_key)
        .timeout(request_timeout)
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
        return Err(RegisterError::Status(err_code, err_msg));
    }

    debug!(response = field::display(response.status()), "success");

    let response: RegisterResponse = response.json().await?;
    assert!(response.uuid == register_request.uuid);
    assert!(response.api_key == register_request.api_key);

    Ok(response)
}
