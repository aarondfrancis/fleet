//! Request snapshots, provenance, conditional reads and consistent API errors.

use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Value, json};

use super::checks::CheckStore;
use crate::engine::Engine;
use crate::model::FleetView;

#[derive(Clone)]
pub(super) struct ApiState {
    pub engine: Arc<Engine>,
    pub checks: Arc<CheckStore>,
    pub logs: Arc<tokio::sync::Semaphore>,
}

#[derive(Clone)]
pub(super) struct Snapshot {
    pub view: Arc<FleetView>,
    pub revision: String,
}

impl Snapshot {
    pub async fn read(engine: Arc<Engine>) -> Result<Self, ApiError> {
        let view = tokio::task::spawn_blocking(move || engine.view())
            .await
            .map_err(|e| ApiError::internal(format!("Couldn't read the fleet: {e}")))?;
        Ok(Self {
            revision: super::revision(&view).map_err(|e| ApiError::internal(e.to_string()))?,
            view: Arc::new(view),
        })
    }
}

pub(super) fn fingerprint(value: &Value) -> String {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    value.to_string().hash(&mut hash);
    format!("{:016x}", hash.finish())
}

#[derive(Debug, Clone, Copy, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum ErrorCode {
    NotFound,
    AmbiguousIdentifier,
    InvalidRequest,
    MethodNotAllowed,
    Forbidden,
    DemoMode,
    UnsupportedTarget,
    Busy,
    CheckFailed,
    Internal,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct ErrorBody {
    pub code: ErrorCode,
    pub error: String,
    pub retryable: bool,
    pub candidates: Vec<String>,
}

#[derive(Debug)]
pub(super) struct ApiError {
    pub status: StatusCode,
    pub body: ErrorBody,
}

impl ErrorCode {
    /// The code for an error response that didn't come from an `ApiError`, such as an extractor rejection.
    fn for_status(status: StatusCode) -> Self {
        match status {
            StatusCode::NOT_FOUND => Self::NotFound,
            StatusCode::METHOD_NOT_ALLOWED => Self::MethodNotAllowed,
            StatusCode::FORBIDDEN => Self::Forbidden,
            StatusCode::TOO_MANY_REQUESTS => Self::Busy,
            status if status.is_server_error() => Self::Internal,
            _ => Self::InvalidRequest,
        }
    }
}

impl ApiError {
    pub fn new(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorBody {
                code,
                error: message.into(),
                retryable: false,
                candidates: Vec::new(),
            },
        }
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, message)
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest, message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, message)
    }
    pub fn method_not_allowed() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            ErrorCode::MethodNotAllowed,
            "Only GET, HEAD and POST /v1/checks are supported.",
        )
    }
    pub fn busy(message: impl Into<String>) -> Self {
        let mut error = Self::new(StatusCode::TOO_MANY_REQUESTS, ErrorCode::Busy, message);
        error.body.retryable = true;
        error
    }
    pub fn ambiguous(kind: &str, candidates: Vec<String>) -> Self {
        let mut error = Self::new(
            StatusCode::CONFLICT,
            ErrorCode::AmbiguousIdentifier,
            format!("More than one {kind} matches. Use an exact ID from candidates."),
        );
        error.body.candidates = candidates;
        error
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(self.body)).into_response();
        if self.status == StatusCode::TOO_MANY_REQUESTS {
            response
                .headers_mut()
                .insert("retry-after", HeaderValue::from_static("3"));
        }
        response
    }
}

pub(super) async fn request(State(state): State<ApiState>, mut req: Request<Body>, next: Next) -> Response {
    if let Some(ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<SocketAddr>>()
        && !peer.ip().is_loopback()
    {
        return ApiError::new(StatusCode::FORBIDDEN, ErrorCode::Forbidden, "Local connections only.").into_response();
    }
    if !loopback_host(req.headers()) || req.headers().contains_key("origin") {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::Forbidden,
            "Use a loopback Host header without an Origin header.",
        )
        .into_response();
    }
    if req.method() != Method::GET
        && req.method() != Method::HEAD
        && !(req.method() == Method::POST && req.uri().path() == "/v1/checks")
    {
        return ApiError::method_not_allowed().into_response();
    }
    let snapshot = match Snapshot::read(state.engine).await {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let conditional = (req.method() == Method::GET)
        .then(|| {
            req.headers()
                .get("if-none-match")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        })
        .flatten();
    let is_head = req.method() == Method::HEAD;
    let is_schema = req.uri().path() == "/v1/openapi.json";
    req.extensions_mut().insert(snapshot.clone());
    let response = next.run(req).await;
    let (mut parts, body) = response.into_parts();
    parts
        .headers
        .insert("cache-control", HeaderValue::from_static("no-cache"));
    parts.headers.insert("x-api-version", HeaderValue::from_static("1"));
    parts.headers.insert(
        "x-data-source",
        HeaderValue::from_static(match snapshot.view.source {
            crate::model::DataSource::Live => "live",
            crate::model::DataSource::Demo => "demo",
        }),
    );
    if let Ok(header) = HeaderValue::from_str(&snapshot.revision) {
        parts.headers.insert("x-fleet-revision", header);
    }
    if is_head {
        return Response::from_parts(parts, body);
    }
    let bytes = match to_bytes(body, 32 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(e) => return ApiError::internal(format!("Couldn't serialize response: {e}")).into_response(),
    };
    let mut value: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        json!({
            "code": ErrorCode::for_status(parts.status), "error": String::from_utf8_lossy(&bytes),
            "retryable": false, "candidates": [],
        })
    });
    if !is_schema && let Some(object) = value.as_object_mut() {
        object.insert("apiVersion".into(), json!(1));
        object.insert("source".into(), json!(snapshot.view.source));
        object.insert("generatedAt".into(), json!(snapshot.view.generated_at));
        object.insert("revision".into(), json!(snapshot.revision));
    }
    if parts.status.is_success() {
        let mut stable = value.clone();
        if let Some(object) = stable.as_object_mut() {
            object.remove("generatedAt");
        }
        let etag = format!("W/\"{}\"", fingerprint(&stable));
        if let Ok(header) = HeaderValue::from_str(&etag) {
            parts.headers.insert("etag", header);
        }
        if conditional.as_ref().is_some_and(|h| {
            h.split(',')
                .any(|tag| tag.trim() == "*" || tag.trim().trim_start_matches("W/") == etag.trim_start_matches("W/"))
        }) {
            parts.status = StatusCode::NOT_MODIFIED;
            parts.headers.remove("content-length");
            return Response::from_parts(parts, Body::empty());
        }
    }
    parts.headers.remove("content-length");
    parts
        .headers
        .insert("content-type", HeaderValue::from_static("application/json"));
    Response::from_parts(parts, Body::from(value.to_string()))
}

fn loopback_host(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get("host").and_then(|h| h.to_str().ok()) else {
        return false;
    };
    let Ok(authority) = host.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    matches!(authority.host(), "127.0.0.1" | "localhost" | "[::1]")
}
