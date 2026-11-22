//! Engine: owns config, secrets and observations; runs polls; applies user actions.
//!
//! Poll methods never hold a lock across an await and never discard the last
//! good observation on failure — errors are recorded beside it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use chrono::Utc;

use crate::derive::{derive, derive_without_issues};
use crate::github::{GithubClient, GithubError};
use crate::model::*;
use crate::sync::{MutexExt, RwLockExt};
use crate::{demo, tailscale};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Github(#[from] GithubError),
    #[error("Couldn't save: {0}")]
    Storage(String),
}

pub struct Engine {
    config_path: PathBuf,
    config: RwLock<AppConfig>,
    obs: RwLock<Observations>,
    secrets: Arc<dyn SecretStore>,
    github: Mutex<HashMap<String, Arc<GithubClient>>>,
    tailscale_status:
        Box<dyn Fn() -> futures::future::BoxFuture<'static, Result<TailnetSnapshot, String>> + Send + Sync>,
    desired: Mutex<desired::DesiredStore>,
    desired_changed: tokio::sync::watch::Sender<u64>,
    desired_io: desired::DesiredIo,
    machine_source: Arc<dyn crate::machine::MachineSource>,
    machine_poll: tokio::sync::Mutex<()>,
    desired_refresh_gate: tokio::sync::Mutex<()>,
    doctor_gate: tokio::sync::Mutex<()>,
    monitoring: Mutex<crate::monitoring::MonitorStore>,
    job_history: Arc<Mutex<crate::job_history::JobHistory>>,
    job_history_round: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("config_path", &self.config_path)
            .finish_non_exhaustive()
    }
}

#[path = "engine_desired.rs"]
pub mod desired;

#[path = "engine_job_history.rs"]
mod job_history;
#[path = "engine_machines.rs"]
mod machines;
#[path = "engine_monitoring.rs"]
mod monitoring;
#[path = "engine_orgs.rs"]
mod orgs;
#[path = "engine_secrets.rs"]
mod secrets;
pub use secrets::{MemorySecrets, SecretStore};

impl Engine {
    pub fn load(config_path: impl Into<PathBuf>, secrets: Arc<dyn SecretStore>) -> Result<Self, EngineError> {
        let config_path = config_path.into();
        let config = match std::fs::read_to_string(&config_path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| EngineError::Storage(format!("{}: {e}", config_path.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => AppConfig::default(),
            Err(e) => return Err(EngineError::Storage(e.to_string())),
        };
        let monitoring = crate::monitoring::MonitorStore::load(config_path.with_file_name("monitoring.json"));
        let job_history = crate::job_history::JobHistory::load(config_path.with_file_name("job-history.json"));
        Ok(Self {
            config_path,
            config: RwLock::new(config),
            obs: RwLock::new(Observations::default()),
            secrets,
            github: Mutex::new(HashMap::new()),
            tailscale_status: Box::new(|| Box::pin(tailscale::status())),
            desired: Mutex::new(desired::DesiredStore::default()),
            desired_changed: tokio::sync::watch::channel(0).0,
            desired_io: desired::DesiredIo::default(),
            machine_source: Arc::new(crate::machine::remote_client()),
            machine_poll: tokio::sync::Mutex::new(()),
            desired_refresh_gate: tokio::sync::Mutex::new(()),
            doctor_gate: tokio::sync::Mutex::new(()),
            monitoring: Mutex::new(monitoring),
            job_history: Arc::new(Mutex::new(job_history)),
            job_history_round: tokio::sync::Mutex::new(()),
        })
    }

    /// Replace the Tailscale source (tests).
    pub fn with_tailscale_source<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<TailnetSnapshot, String>> + Send + 'static,
    {
        self.tailscale_status = Box::new(move || Box::pin(f()));
        self
    }

    /// Notified whenever machine data or desired-state data changes.
    pub fn desired_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.desired_changed.subscribe()
    }

    /// Tells `desired_changes` subscribers the fleet view changed.
    pub(crate) fn view_changed(&self) {
        self.desired_changed.send_modify(|v| *v = v.wrapping_add(1));
    }

    pub fn config(&self) -> AppConfig {
        self.config.read_locked().clone()
    }

    pub fn view(&self) -> FleetView {
        let now = Utc::now();
        let config = self.config();
        if config.demo_mode {
            let (demo_config, demo_obs) = demo::dataset(now);
            return derive(
                &demo_config,
                &demo_obs,
                &crate::demo_desired::plan_inputs(now),
                now,
                DataSource::Demo,
            );
        }
        let plan = self.plan_inputs();
        let mut view = derive_without_issues(&config, &self.obs.read_locked(), &plan, now, DataSource::Live);
        self.monitoring
            .locked()
            .attach(&mut view, now, config.desktop_notifications);
        self.job_history.locked().attach(&mut view, now);
        crate::issues::attach(&mut view);
        view
    }

    fn update_config(&self, f: impl FnOnce(&mut AppConfig)) -> Result<(), EngineError> {
        let mut config = self.config.write_locked();
        let mut next = config.clone();
        f(&mut next);
        save_json(&self.config_path, &next)?;
        *config = next;
        Ok(())
    }

    pub async fn poll_tailscale(&self) {
        let result = (self.tailscale_status)().await;
        let mut obs = self.obs.write_locked();
        match result {
            Ok(snap) => {
                obs.tailscale = Some(Timed::new(Utc::now(), snap));
                obs.tailscale_error = None;
            }
            Err(e) => {
                tracing::warn!("tailscale status failed: {e}");
                obs.tailscale_error = Some(Timed::new(Utc::now(), e));
            }
        }
    }

    /// Whether a Tailscale status has been read since launch.
    pub fn tailnet_available(&self) -> bool {
        self.obs.read_locked().tailscale.is_some()
    }

    pub fn ignore_peer(&self, node_id: &str) -> Result<(), EngineError> {
        let name = self
            .obs
            .read_locked()
            .tailscale
            .as_ref()
            .and_then(|t| t.value.peers.iter().find(|p| p.node_id == node_id))
            .map(|p| p.hostname.clone())
            .unwrap_or_else(|| node_id.to_string());
        self.update_config(|c| {
            if !c.ignored_peers.iter().any(|i| i.tailscale_node_id == node_id) {
                c.ignored_peers.push(IgnoredPeer {
                    tailscale_node_id: node_id.to_string(),
                    name,
                    ignored_at: Utc::now(),
                });
            }
        })
    }

    pub fn unignore_peer(&self, node_id: &str) -> Result<(), EngineError> {
        self.update_config(|c| c.ignored_peers.retain(|i| i.tailscale_node_id != node_id))
    }

    pub fn set_local_api(&self, on: bool) -> Result<(), EngineError> {
        self.update_config(|c| c.local_api = on)
    }

    pub fn set_demo_mode(&self, on: bool) -> Result<(), EngineError> {
        self.update_config(|c| c.demo_mode = on)
    }
}

fn save_json(path: &Path, value: &impl serde::Serialize) -> Result<(), EngineError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| EngineError::Storage(e.to_string()))?;
    crate::atomic_write::atomic_write(path, &bytes).map_err(|e| EngineError::Storage(e.to_string()))
}
