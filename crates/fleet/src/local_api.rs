//! Loopback HTTP observations and bounded, read-only troubleshooting for local agents.
//!
//! Host and Origin checks reject browser access. Only `POST /v1/checks` starts
//! work: observation refreshes and diagnostics, never runner mutations or tokens.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::get;

use crate::engine::Engine;
use crate::model::FleetView;

mod checks;
mod common;
mod queries;
mod reads;
mod schema;

use common::{ApiError, ApiState};

/// Default loopback port; the desktop discovery file advertises the actual listener.
pub const DEFAULT_PORT: u16 = 47622;

const DEFAULT_PAGE_SIZE: usize = 100;
pub(crate) const MAX_PAGE_SIZE: usize = 200;
const CONCURRENT_LOGS: usize = 2;

/// Content fingerprint of a fleet view, ignoring `generatedAt`, so equal content gets an equal revision.
///
/// # Errors
///
/// When the view can't be serialized.
pub fn revision(view: &FleetView) -> Result<String, serde_json::Error> {
    let mut content = serde_json::to_value(view)?;
    if let Some(object) = content.as_object_mut() {
        object.remove("generatedAt");
    }
    Ok(common::fingerprint(&content))
}

/// Construct the API with a shared, bounded registry for asynchronous checks.
pub fn router(engine: Arc<Engine>) -> Router {
    let state = ApiState {
        engine,
        checks: Arc::new(checks::CheckStore::default()),
        logs: Arc::new(tokio::sync::Semaphore::new(CONCURRENT_LOGS)),
    };
    Router::new()
        .route("/", get(reads::index))
        .route("/v1/summary", get(reads::summary))
        .route("/v1/fleet", get(reads::fleet))
        .route("/v1/machines", get(reads::machines))
        .route("/v1/machines/{id}", get(reads::machine))
        .route("/v1/runners", get(reads::runners))
        .route("/v1/runners/{*key}", get(reads::runner))
        .route("/v1/queue", get(reads::queue))
        .route("/v1/discovered", get(reads::discovered))
        .route("/v1/desired", get(reads::desired))
        .route("/v1/monitoring", get(reads::monitoring))
        .route("/v1/events", get(reads::events))
        .route("/v1/issues", get(reads::issues))
        .route("/v1/changes", get(reads::changes))
        .route("/v1/logs", get(checks::logs))
        .route("/v1/checks", axum::routing::post(checks::start))
        .route("/v1/checks/{id}", get(checks::status))
        .route("/v1/openapi.json", get(schema::openapi))
        .fallback(|| async { ApiError::not_found("No such endpoint; GET / lists them.") })
        .method_not_allowed_fallback(|| async { ApiError::method_not_allowed() })
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), common::request))
        .with_state(state)
}

/// Serve until the listener fails; all accepted peers must be loopback addresses.
pub async fn serve(listener: tokio::net::TcpListener, engine: Arc<Engine>) -> std::io::Result<()> {
    axum::serve(
        listener,
        router(engine).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}

/// Bind IPv4 loopback only; port zero requests an available ephemeral port.
pub async fn bind(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)).await
}

#[cfg(test)]
mod tests;
