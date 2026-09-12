//! Binary entrypoint. Everything of substance is in the library so the
//! conformance suite under `tests/` can reach it — an integration test cannot
//! see inside a bin-only crate, and the suite is the whole point of #656.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::SystemTime;

use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use slack_event_gateway::http::{self, Gateway};
use slack_event_gateway::publish::{self, MetadataTokens, PubSub};
use slack_event_gateway::registry::Registry;

/// Default Pub/Sub endpoint.
const DEFAULT_PUBSUB_URL: &str = "https://pubsub.googleapis.com";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // No body ever reaches a log, so the formatter needs nothing special —
    // but the level does have to be readable, because the operational signals
    // here (a refused signature, a failed publish) are all log lines.
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();

    let registry = Registry::parse(&load_registrations()?)?;
    tracing::info!(
        operators = registry.users.len(),
        "loaded the registration table"
    );

    let client = reqwest::Client::builder()
        .timeout(publish::PUBLISH_BUDGET)
        .build()?;
    let pubsub_url = std::env::var("PUBSUB_URL").unwrap_or_else(|_| DEFAULT_PUBSUB_URL.to_string());
    let gateway = Arc::new(Gateway {
        registry,
        publisher: PubSub::new(client.clone(), &pubsub_url, MetadataTokens::new(client)),
    });

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "listening");

    loop {
        // A shutdown signal ends the accept loop; in-flight requests finish on
        // their own tasks. Cloud Run sends SIGTERM before it stops an
        // instance, and a delivery dropped mid-flight is one Slack counts as
        // failed.
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) => {
                    tracing::warn!(error = %e, "could not accept a connection");
                    continue;
                }
            },
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutting down");
                return Ok(());
            }
        };
        let gateway = Arc::clone(&gateway);
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let gateway = Arc::clone(&gateway);
                async move {
                    Ok::<_, std::convert::Infallible>(
                        http::handle(gateway, request, SystemTime::now()).await,
                    )
                }
            });
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                // Connection-level, not request-level: a client that hung up
                // mid-request lands here, and it is not worth a warning.
                tracing::debug!(error = %e, "connection ended");
            }
        });
    }
}

/// The registration table, from a mounted file or the environment.
///
/// A mounted secret is preferred: an environment variable is visible in the
/// revision's configuration to anyone who can read it in the console, and it
/// holds every operator's signing secret.
fn load_registrations() -> Result<String, Box<dyn std::error::Error>> {
    if let Ok(path) = std::env::var("REGISTRATIONS_PATH") {
        return Ok(std::fs::read_to_string(&path)
            .map_err(|e| format!("could not read the registration table at `{path}`: {e}"))?);
    }
    if let Ok(inline) = std::env::var("REGISTRATIONS") {
        return Ok(inline);
    }
    Err(
        "neither `REGISTRATIONS_PATH` nor `REGISTRATIONS` is set — there is no registration \
         table, so every delivery would be refused"
            .into(),
    )
}
