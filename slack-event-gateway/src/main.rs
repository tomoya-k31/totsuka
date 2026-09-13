//! Binary entrypoint. Everything of substance is in the library so the
//! conformance suite under `tests/` can reach it — an integration test cannot
//! see inside a bin-only crate, and the suite is the whole point of #656.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::SystemTime;

use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use hyper_util::server::graceful::GracefulShutdown;

use slack_event_gateway::http::{self, Gateway};
use slack_event_gateway::publish::{self, MetadataTokens, PubSub};
use slack_event_gateway::registry::Registry;

/// Default Pub/Sub endpoint.
const DEFAULT_PUBSUB_URL: &str = "https://pubsub.googleapis.com";

/// How long a shutdown waits for in-flight requests.
///
/// Comfortably over one publish budget (2.5s) so a delivery mid-publish
/// finishes and gets its 200, and comfortably under Cloud Run's own grace
/// period so the process exits on its own terms rather than being killed.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

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

    // **Cloud Run stops an instance with SIGTERM, not SIGINT.** Waiting only on
    // `ctrl_c` meant a deploy or a scale-down killed the process outright — and
    // a publish that Pub/Sub already accepted but which never got to answer 200
    // becomes a delivery Slack counts as *failed*. Enough of those is exactly
    // the condition that disables the subscription, which is the disease this
    // whole design exists to cure. So both signals are handled, and in-flight
    // connections are drained rather than dropped with the runtime.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let graceful = GracefulShutdown::new();

    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) => {
                    tracing::warn!(error = %e, "could not accept a connection");
                    continue;
                }
            },
            _ = sigterm.recv() => {
                tracing::info!("SIGTERM; draining in-flight requests");
                break;
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("SIGINT; draining in-flight requests");
                break;
            }
        };
        let gateway = Arc::clone(&gateway);
        let service = service_fn(move |request| {
            let gateway = Arc::clone(&gateway);
            async move {
                Ok::<_, std::convert::Infallible>(
                    http::handle(gateway, request, SystemTime::now()).await,
                )
            }
        });
        let connection = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(stream), service);
        // Watched, so the drain below actually waits for it. A bare
        // `tokio::spawn` would be dropped along with the runtime.
        let watched = graceful.watch(connection);
        tokio::spawn(async move {
            if let Err(e) = watched.await {
                // Connection-level, not request-level: a client that hung up
                // mid-request lands here, and it is not worth a warning.
                tracing::debug!(error = %e, "connection ended");
            }
        });
    }

    // Bounded: Cloud Run's own grace period is finite, and a keep-alive
    // connection with no request in flight would otherwise hold the process
    // open until the client chose to close it.
    tokio::select! {
        () = graceful.shutdown() => tracing::info!("all connections drained"),
        () = tokio::time::sleep(SHUTDOWN_GRACE) => tracing::warn!(
            seconds = SHUTDOWN_GRACE.as_secs(),
            "shutdown grace elapsed with requests still in flight"
        ),
    }
    Ok(())
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
