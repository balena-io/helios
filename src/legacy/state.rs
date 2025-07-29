use mahler::workflow::Interrupt;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, field, instrument, trace, warn};

use crate::types::ApiKey;
use crate::util::http::{Client, ClientError, Uri};

use super::error::UpstreamError;

#[derive(Debug, Error)]
pub enum StateUpdateError {
    #[error(transparent)]
    Upstream(#[from] UpstreamError),

    #[error("Operation was interrupted")]
    Interrupted,
}

impl StateUpdateError {
    fn from_upstream<T>(err: T) -> Self
    where
        T: Into<UpstreamError>,
    {
        Self::Upstream(err.into())
    }
}

#[derive(Debug, Deserialize)]
struct StateStatusResponse {
    #[serde(rename = "appState")]
    app_state: String,
}

pub async fn wait_for_state_settle(
    legacy_api_endpoint: Uri,
    legacy_api_key: ApiKey,
    interrupt: Interrupt,
) -> Result<(), StateUpdateError> {
    let client = Client::default();

    // Build the status check URI
    let status_url = Uri::from_parts(
        legacy_api_endpoint,
        "/v2/state/status",
        Some(&format!("apikey={legacy_api_key}")),
    );
    let status_url = status_url.map_err(StateUpdateError::from_upstream)?;

    // Poll the status endpoint until appState is 'applied'
    loop {
        trace!("waiting for legacy Supervisor state to settle");
        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {}
            _ = interrupt.wait() => return Err(StateUpdateError::Interrupted)
        }
        let status = client
            .get::<StateStatusResponse>(&status_url, None)
            .await
            .map_err(StateUpdateError::from_upstream)?;
        if status.app_state == "applied" {
            break;
        }
    }
    Ok(())
}

/// Trigger an update on the legacy supervisor
#[instrument(skip_all, err)]
pub async fn trigger_update(
    legacy_api_endpoint: Uri,
    legacy_api_key: ApiKey,
    force: bool,
    cancel: bool,
    interrupt: Interrupt,
) -> Result<(), StateUpdateError> {
    let client = Client::default();

    // Build the URI from the address parts
    let update_url = Uri::from_parts(
        legacy_api_endpoint.clone(),
        "/v1/update",
        Some(format!("apikey={legacy_api_key}").as_str()),
    );
    let update_url = update_url.map_err(StateUpdateError::from_upstream)?;

    let payload = json!({
        "force": force,
        "cancel": cancel
    });

    debug!("calling legacy Supervisor");
    loop {
        match client.post(&update_url, None, &payload).await {
            Ok(()) => {
                debug!("success");
                break;
            }
            Err(ClientError::Server(status)) => {
                warn!(response = field::display(status), "received error response")
            }
            Err(e) => warn!("failed: {e}, retrying in 5s"),
        };

        // Back-off for a bit in case the supervisor is restarting
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    // Wait for the state to settle
    wait_for_state_settle(
        legacy_api_endpoint.clone(),
        legacy_api_key.clone(),
        interrupt,
    )
    .await?;

    Ok(())
}
