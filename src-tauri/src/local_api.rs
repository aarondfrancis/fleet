//! Starts the read-only local API (see `fleet::local_api`) and writes a small
//! discovery file so agents can find it: `<app config dir>/local-api.json`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fleet::engine::Engine;
use fleet::sync::MutexExt;
use serde::Serialize;

pub const PORT_ENV: &str = "RUNNER_DASHBOARD_API_PORT";

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalApiStatus {
    pub enabled: bool,
    pub url: Option<String>,
    pub error: Option<String>,
    pub discovery_file: Option<String>,
}

pub type SharedStatus = Arc<Mutex<LocalApiStatus>>;

pub fn start(engine: Arc<Engine>, config_dir: PathBuf) -> SharedStatus {
    let status: SharedStatus = Arc::new(Mutex::new(LocalApiStatus {
        enabled: engine.config().local_api,
        ..Default::default()
    }));
    let discovery = config_dir.join("local-api.json");
    if !engine.config().local_api {
        // Usually absent; a stale file just points at a closed port.
        let _ = std::fs::remove_file(&discovery);
        return status;
    }
    let port = std::env::var(PORT_ENV)
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(fleet::local_api::DEFAULT_PORT);
    let shared = status.clone();
    tauri::async_runtime::spawn(async move {
        match fleet::local_api::bind(port).await {
            Ok(listener) => {
                let address = match listener.local_addr() {
                    Ok(address) => address,
                    Err(error) => {
                        shared.locked().error = Some(error.to_string());
                        return;
                    }
                };
                let url = format!("http://{address}");
                let body = serde_json::json!({ "url": url, "pid": std::process::id(), "readOnly": true });
                // The discovery file is a convenience; the API works without it.
                let written = async {
                    tokio::fs::create_dir_all(&config_dir).await?;
                    tokio::fs::write(&discovery, body.to_string()).await
                }
                .await;
                if let Err(e) = written {
                    tracing::warn!("local API: can't write {}: {e}", discovery.display());
                }
                {
                    let mut s = shared.locked();
                    s.url = Some(url.clone());
                    s.discovery_file = Some(discovery.display().to_string());
                }
                tracing::info!(%url, "local API listening");
                if let Err(e) = fleet::local_api::serve(listener, engine).await {
                    shared.locked().error = Some(e.to_string());
                }
            }
            Err(e) => {
                tracing::warn!("local API: can't bind 127.0.0.1:{port}: {e}");
                // Usually absent; a stale file just points at a closed port.
                let _ = std::fs::remove_file(&discovery);
                shared.locked().error = Some(format!(
                    "Couldn't listen on port {port} ({e}). Set {PORT_ENV} to use another port."
                ));
            }
        }
    });
    status
}
