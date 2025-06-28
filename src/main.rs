use anyhow::Result;
use axum::http::uri::PathAndQuery;
use clap::Parser;
use hyper::Uri;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, field, info, instrument, warn, Span};
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
};

mod api;
mod cli;
mod config;
pub mod control;
pub mod link;
mod overrides;

use api::Api;
use cli::{Cli, Commands};
use config::Config;
use link::{Directive, UplinkService};
use overrides::Overrides;

/// Notify the fallback service of a state update via POST to /v1/update
#[instrument(skip_all, fields(force = directive.force, cancel = directive.cancel, response = field::Empty), err)]
async fn notify_fallback(
    client: &reqwest::Client,
    fallback_address: Uri,
    fallback_api_key: &Option<String>,
    directive: &Directive,
) -> Result<()> {
    // Build the URI from the address parts
    let mut addr_parts = fallback_address.into_parts();
    addr_parts.path_and_query = if let Some(apikey) = fallback_api_key {
        Some(PathAndQuery::from_maybe_shared(format!(
            "/v1/update?apikey={apikey}",
        ))?)
    } else {
        Some(PathAndQuery::from_maybe_shared("/v1/update")?)
    };
    let url = Uri::from_parts(addr_parts)?.to_string();

    let payload = json!({
        "force": directive.force,
        "cancel": directive.cancel
    });

    let response = client.post(&url).json(&payload).send().await?;

    Span::current().record("response", response.status().as_u16());

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing subscriber for human-readable logs
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env().unwrap_or(
                EnvFilter::default()
                    .add_directive("debug".parse()?)
                    .add_directive("hyper=error".parse()?)
                    .add_directive("bollard=error".parse()?),
            ),
        )
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_span_events(FmtSpan::CLOSE)
                .event_format(fmt::format().compact().with_target(false).without_time()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Register {
            remote,
            provisioning_key,
        }) => {
            info!(
                "Starting device registration with key: {}",
                provisioning_key
            );
            debug!("Remote config: {:#?}", remote);
            todo!("Implement register command");
        }
        None => {
            // Main service mode
            let config = Config::load(&cli)?;
            info!("Configuration loaded successfully");
            debug!("{:#?}", config);

            start_supervising(config).await?;
        }
    }

    Ok(())
}

async fn start_supervising(config: Config) -> Result<()> {
    info!("Service started");
    // Only start poll if there is an API endpoint
    let api = if let Some(api_endpoint) = config.remote.api_endpoint.clone() {
        let overrides = Overrides::new(config.uuid.clone());

        info!("Starting target state poll to {api_endpoint}");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let uplink = UplinkService::new(api_endpoint, config.clone(), tx).await?;

        let api = Arc::new(Api::new(config.clone()).with_uplink(uplink));

        let api_ref = Arc::clone(&api);
        let fallback_address = config.fallback.address.clone();
        let fallback_api_key = config.fallback.api_key.clone();
        tokio::spawn(async move {
            // Create HTTP client for fallback notifications
            let client = reqwest::Client::new();

            while let Some(directive) = rx.recv().await {
                // Override the target state from `/mnt/temp/apps`
                let target_with_overrides = overrides.apply(directive.target.clone()).await;

                // Update the API target state, this will be returned by the API in
                // polling requests from the legacy supervisor
                api_ref.set_target_state(target_with_overrides).await;

                // Notify fallback supervisor if configured
                if let Some(ref fallback_addr) = fallback_address {
                    let _ = notify_fallback(
                        &client,
                        fallback_addr.clone(),
                        &fallback_api_key,
                        &directive,
                    )
                    .await;
                }
            }
        });
        api
    } else {
        warn!("No remote API endpoint provided, running in unmanaged mode");
        Arc::new(Api::new(config.clone()))
    };

    api.start().await?;

    Ok(())
}
