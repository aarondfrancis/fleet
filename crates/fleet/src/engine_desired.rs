//! Engine side of desired state (Runner Fleet, merged in): checking the fleet
//! against `inventory/fleet.toml`, and running reviewed changes.
//!
//! Checking is read-only. A change is always two steps: `desired_review`
//! re-reads the host and shows exactly what would happen, then
//! `desired_execute` re-reads again and runs only if the plan is still the one
//! the user reviewed. Passwords supplied for a change are used once and never
//! stored.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use super::{Engine, EngineError};

#[path = "engine_desired_review.rs"]
mod review;
#[path = "engine_desired_ssh.rs"]
mod ssh;
#[path = "engine_desired_store.rs"]
mod store;
pub use review::{ChangeSecrets, ReviewView};
pub use ssh::SshTransportIdentity;
pub use store::DesiredStore;

use crate::desired::github::FleetGithub;
use crate::desired::inventory::{default_inventory_path, load_inventory_from_env};
use crate::desired::model::{FleetSnapshot, Inventory, PlanItem, SshTransport};
use crate::desired::operations::{self, Ops, Secrets};
use crate::desired::remote::{FleetRemote, Transient};
use crate::desired::state::collect_snapshot;
use crate::fleet_plan::{ChangeRequest, DoctorView, OperationStatus, OperationView, host_identity_conflicts};
use crate::sync::{MutexExt, RwLockExt};
use review::{build_review, request_host};
use store::{Review, runner_identities};

pub type DesiredBoundaries = Arc<dyn Fn(&Inventory) -> (Arc<dyn FleetGithub>, Arc<dyn FleetRemote>) + Send + Sync>;

/// Effective SSH configuration for a target (`ssh -G`), if resolution succeeds.
pub type SshTransportResolver = Arc<dyn Fn(&str) -> Option<SshTransportIdentity> + Send + Sync>;

pub struct DesiredIo {
    pub boundaries: DesiredBoundaries,
    pub sleep: fn(Duration),
    pub resolve_ssh_transport: SshTransportResolver,
}

impl std::fmt::Debug for DesiredIo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesiredIo").finish_non_exhaustive()
    }
}

impl Default for DesiredIo {
    fn default() -> Self {
        Self {
            boundaries: Arc::new(|inventory: &Inventory| {
                let (github, remote) =
                    crate::desired::real_boundaries(&inventory.organization.name, Duration::from_secs(20));
                (
                    Arc::new(github) as Arc<dyn FleetGithub>,
                    Arc::new(remote) as Arc<dyn FleetRemote>,
                )
            }),
            sleep: std::thread::sleep,
            resolve_ssh_transport: Arc::new(ssh::resolve),
        }
    }
}

const CHANGE_RUNNING: &str = "Another change is running; wait for it to finish.";

/// Releases the change slot `take_review` reserved, however the execute ends.
struct Starting<'a>(&'a Engine);

impl Drop for Starting<'_> {
    fn drop(&mut self) {
        self.0.desired.locked().starting = false;
    }
}

/// Runs blocking work off the async runtime. If the work panics, the error
/// says what couldn't be done.
async fn run_blocking<T: Send + 'static>(
    what: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, EngineError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| EngineError::Invalid(format!("Couldn't {what}: {e}")))
}

impl Engine {
    pub fn with_desired_io(mut self, io: DesiredIo) -> Self {
        self.desired_io = io;
        self
    }

    pub fn inventory_path(&self) -> PathBuf {
        self.config()
            .inventory_path
            .map(PathBuf::from)
            .unwrap_or_else(default_inventory_path)
    }

    pub fn set_inventory_path(&self, path: Option<String>) -> Result<(), EngineError> {
        let path = path.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
        self.update_config(|c| c.inventory_path = path)?;
        self.desired.locked().forget_inventory();
        self.view_changed();
        Ok(())
    }

    /// The inventory as it is on disk now, read off the async runtime: it's
    /// edited by hand, so it's re-read for every check and change.
    pub(super) async fn read_inventory(&self) -> Result<Inventory, EngineError> {
        let path = self.inventory_path();
        run_blocking("read the inventory", move || load_inventory_from_env(&path))
            .await?
            .map_err(|e| EngineError::Invalid(e.0))
    }

    fn host_identity_conflict(&self, inventory: &Inventory, host_id: &str) -> Option<String> {
        let ssh_hosts = self.desired.locked().ssh_hosts.clone();
        let peers = self
            .obs
            .read_locked()
            .tailscale
            .as_ref()
            .map(|snapshot| snapshot.value.peers.clone())
            .unwrap_or_default();
        host_identity_conflicts(inventory, &ssh_hosts, &peers).remove(host_id)
    }

    fn ensure_live_desired(&self) -> Result<(), EngineError> {
        if self.config().demo_mode {
            return Err(EngineError::Invalid(
                "Turn off demo data to check or change real machines.".into(),
            ));
        }
        Ok(())
    }

    /// Read-only: lists GitHub runners and probes every enabled host (or one).
    pub async fn desired_refresh(self: &Arc<Self>, host: Option<String>) -> Result<(), EngineError> {
        let Ok(_guard) = self.desired_refresh_gate.try_lock() else {
            return Ok(());
        };
        self.desired_refresh_inner(host).await
    }

    /// Wait for an existing check, then perform and await the requested scope.
    pub(crate) async fn desired_refresh_wait(self: &Arc<Self>, host: Option<String>) -> Result<(), EngineError> {
        let _guard = self.desired_refresh_gate.lock().await;
        self.desired_refresh_inner(host).await
    }

    /// Callers hold `desired_refresh_gate`, so checks never overlap.
    async fn desired_refresh_inner(self: &Arc<Self>, host: Option<String>) -> Result<(), EngineError> {
        self.ensure_live_desired()?;
        let inventory = self.read_inventory().await?;
        self.desired.locked().refreshing = true;
        self.view_changed();
        self.resolve_ssh_hosts(&inventory).await;
        let result = self.fresh_snapshot(&inventory, host.as_deref()).await;
        {
            let mut store = self.desired.locked();
            store.refreshing = false;
            if let Ok(snapshot) = &result {
                store.merge_snapshot(&inventory, snapshot.clone(), host.as_deref());
            }
        }
        self.view_changed();
        result.map(drop)
    }

    /// Re-resolves every host's SSH target, off the async runtime, so the view
    /// can match aliases to tailnet machines without blocking.
    pub(super) async fn resolve_ssh_hosts(&self, inventory: &Inventory) -> BTreeMap<String, SshTransportIdentity> {
        let resolve = self.desired_io.resolve_ssh_transport.clone();
        let targets: Vec<String> = inventory.hosts.iter().filter_map(|h| h.ssh_target.clone()).collect();
        let resolved = run_blocking("resolve SSH hosts", move || {
            targets
                .into_iter()
                .filter_map(|t| resolve(&t).map(|identity| (t, identity)))
                .collect::<BTreeMap<_, _>>()
        })
        .await;
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(e) => {
                // No resolved transport blocks every review and change on an
                // OpenSSH host, so this fails closed.
                tracing::warn!("{e}");
                return BTreeMap::new();
            }
        };
        self.desired.locked().ssh_hosts = resolved
            .iter()
            .map(|(target, identity)| (target.clone(), identity.hostname().to_string()))
            .collect();
        resolved
    }

    async fn fresh_snapshot(&self, inventory: &Inventory, host: Option<&str>) -> Result<FleetSnapshot, EngineError> {
        let (github, remote) = (self.desired_io.boundaries)(inventory);
        let inventory = inventory.clone();
        let host = host.map(str::to_string);
        run_blocking("check the fleet", move || {
            collect_snapshot(&inventory, github.as_ref(), remote.as_ref(), host.as_deref())
        })
        .await?
        .map_err(|e| EngineError::NotFound(e.0))
    }

    /// Read-only host checks: SSH identity, prerequisites, disk and power.
    pub async fn desired_doctor(self: &Arc<Self>, host: Option<String>) -> Result<(), EngineError> {
        let Ok(_guard) = self.doctor_gate.try_lock() else {
            return Ok(());
        };
        self.desired_doctor_inner(host).await
    }

    /// Wait for an existing check, then perform and await the requested scope.
    pub(crate) async fn desired_doctor_wait(self: &Arc<Self>, host: Option<String>) -> Result<(), EngineError> {
        let _guard = self.doctor_gate.lock().await;
        self.desired_doctor_inner(host).await
    }

    /// Callers hold `doctor_gate`, so host checks never overlap.
    async fn desired_doctor_inner(self: &Arc<Self>, host: Option<String>) -> Result<(), EngineError> {
        self.ensure_live_desired()?;
        let inventory = self.read_inventory().await?;
        self.desired.locked().doctor_running = true;
        self.view_changed();
        let (github, remote) = (self.desired_io.boundaries)(&inventory);
        let scope = host.clone();
        let result = run_blocking("run the host checks", move || {
            operations::doctor(&inventory, github.as_ref(), remote.as_ref(), scope.as_deref())
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| e.0));
        let (checks, error) = match result {
            Ok(checks) => (checks, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        {
            let mut store = self.desired.locked();
            store.doctor_running = false;
            store.doctor = Some(DoctorView {
                host,
                checks,
                error,
                checked_at: Utc::now(),
            });
        }
        self.view_changed();
        Ok(())
    }

    /// Read-only: the tail of one runner's diagnostic log.
    pub async fn desired_logs(&self, runner: String, lines: u32) -> Result<String, EngineError> {
        self.ensure_live_desired()?;
        let inventory = self.read_inventory().await?;
        let (github, remote) = (self.desired_io.boundaries)(&inventory);
        let sleep = self.desired_io.sleep;
        run_blocking("read the runner log", move || {
            let ops = Ops {
                inventory: &inventory,
                github: github.as_ref(),
                remote: remote.as_ref(),
                secrets: Secrets::default(),
                sleep: &sleep,
            };
            ops.runner_logs(&runner, lines)
        })
        .await?
        .map_err(|e| EngineError::Invalid(e.0))
    }

    /// Read-only: re-reads the host and builds the review for one change.
    pub async fn desired_review(self: &Arc<Self>, request: ChangeRequest) -> Result<ReviewView, EngineError> {
        self.ensure_live_desired()?;
        let inventory = self.read_inventory().await?;
        let host_id = request_host(&request, &inventory)?;
        let host = inventory.find_host(&host_id).map_err(|e| EngineError::NotFound(e.0))?;
        let transports = self.resolve_ssh_hosts(&inventory).await;
        let snapshot = self.fresh_snapshot(&inventory, Some(&host_id)).await?;
        self.desired
            .locked()
            .merge_snapshot(&inventory, snapshot.clone(), Some(&host_id));
        self.view_changed();

        let mut review = build_review(&inventory, host, &request, &snapshot)?;
        let ssh_transport = host
            .ssh_target
            .as_ref()
            .and_then(|target| transports.get(target))
            .cloned();
        if host.ssh_transport == SshTransport::Openssh && ssh_transport.is_none() {
            review.blockers.push(
                "Couldn't resolve the effective SSH configuration. Check SSH settings and review the change again."
                    .into(),
            );
        }
        if let Some(conflict) = self.host_identity_conflict(&inventory, &host_id) {
            review.blockers.push(conflict);
        }
        if matches!(request, ChangeRequest::Apply { .. })
            && review.blockers.is_empty()
            && !review.items.iter().any(PlanItem::is_runnable)
        {
            review
                .blockers
                .push("Nothing to change: this already matches the inventory.".into());
        }
        review.ready = review.blockers.is_empty();
        let identities = runner_identities(host, &snapshot);
        let inventory_path = self.inventory_path();
        let mut store = self.desired.locked();
        let ssh_hosts = store.ssh_hosts.clone();
        store.add_review(Review {
            view: review.clone(),
            created_at: Utc::now(),
            inventory_path,
            inventory,
            ssh_hosts,
            ssh_transport,
            identities,
        });
        Ok(review)
    }

    /// Runs a reviewed change in the background, if the host still looks the
    /// way it did at review. Progress arrives via `desired_changes`.
    pub async fn desired_execute(
        self: &Arc<Self>,
        review_id: &str,
        secrets: ChangeSecrets,
        confirmation: Option<String>,
    ) -> Result<(), EngineError> {
        self.ensure_live_desired()?;
        let review = self.take_review(review_id, confirmation.as_deref())?;
        let starting = Starting(self);
        let (inventory, host_id) = self.recheck(&review).await?;
        self.start_operation(&review.view);
        drop(starting);
        self.view_changed();
        let secrets = Secrets {
            windows_service_password: Transient::new(secrets.service_password),
            linux_sudo_password: Transient::new(secrets.sudo_password),
        };
        self.spawn_change(inventory, review.view.request, host_id, secrets);
        Ok(())
    }

    /// The review, used up, if it is ready and confirmed and no change is running.
    /// Reserves the change slot until `start_operation`, so a second execute
    /// can't start while this one re-checks the host.
    fn take_review(&self, review_id: &str, confirmation: Option<&str>) -> Result<Review, EngineError> {
        let mut store = self.desired.locked();
        if store.change_running() {
            return Err(EngineError::Invalid(CHANGE_RUNNING.into()));
        }
        let review = store.take_review(review_id).ok_or_else(|| {
            EngineError::NotFound("That review expired or was already used; review the change again.".into())
        })?;
        if !review.view.ready {
            return Err(EngineError::Invalid("This change has blockers and can't run.".into()));
        }
        if let Some(phrase) = &review.view.confirm_phrase
            && confirmation.map(str::trim) != Some(phrase.as_str())
        {
            return Err(EngineError::Invalid(format!("Type \"{phrase}\" exactly to confirm.")));
        }
        store.starting = true;
        Ok(review)
    }

    /// Re-reads the inventory, SSH, GitHub and the host, and returns the
    /// inventory and host to change only if all of them match the review.
    async fn recheck(&self, review: &Review) -> Result<(Inventory, String), EngineError> {
        let reviewed = &review.view;
        let inventory = self.read_inventory().await?;
        if inventory != review.inventory || self.inventory_path() != review.inventory_path {
            return Err(EngineError::Invalid(
                "The inventory changed since you reviewed it. Review the change again.".into(),
            ));
        }
        let host_id = request_host(&reviewed.request, &inventory)?;
        let host = inventory.find_host(&host_id).map_err(|e| EngineError::NotFound(e.0))?;
        let transports = self.resolve_ssh_hosts(&inventory).await;
        if host.ssh_transport == SshTransport::Openssh {
            let transport = host.ssh_target.as_ref().and_then(|target| transports.get(target));
            if transport.is_none() || transport != review.ssh_transport.as_ref() {
                self.view_changed();
                return Err(EngineError::Invalid(format!(
                    "{} SSH transport changed since you reviewed it. Review the change again.",
                    host.id
                )));
            }
        }
        let snapshot = self.fresh_snapshot(&inventory, Some(&host_id)).await?;
        self.desired
            .locked()
            .merge_snapshot(&inventory, snapshot.clone(), Some(&host_id));
        let mut now = build_review(&inventory, host, &reviewed.request, &snapshot)?;
        if let Some(conflict) = self.host_identity_conflict(&inventory, &host_id) {
            now.blockers.push(conflict);
            now.ready = false;
        }
        let ssh_hosts = self.desired.locked().ssh_hosts.clone();
        if !now.ready
            || now.items != reviewed.items
            || now.confirm_phrase != reviewed.confirm_phrase
            || now.needs_service_password != reviewed.needs_service_password
            || now.accepts_sudo_password != reviewed.accepts_sudo_password
            || runner_identities(host, &snapshot) != review.identities
            || ssh_hosts != review.ssh_hosts
        {
            self.view_changed();
            return Err(EngineError::Invalid(format!(
                "{} changed since you reviewed it. Review the change again.",
                host.id
            )));
        }
        Ok((inventory, host_id))
    }

    fn start_operation(&self, review: &ReviewView) {
        let mut store = self.desired.locked();
        store.operation = Some(OperationView {
            title: review.title.clone(),
            request: review.request.clone(),
            status: OperationStatus::Running,
            messages: Vec::new(),
            error: None,
            started_at: Utc::now(),
            finished_at: None,
        });
    }

    /// Carries out the change, records how it ended, then checks the host again.
    fn spawn_change(self: &Arc<Self>, inventory: Inventory, request: ChangeRequest, host_id: String, secrets: Secrets) {
        let engine = self.clone();
        tokio::spawn(async move {
            let (github, remote) = (engine.desired_io.boundaries)(&inventory);
            let sleep = engine.desired_io.sleep;
            let result = run_blocking("finish the change", move || {
                let ops = Ops {
                    inventory: &inventory,
                    github: github.as_ref(),
                    remote: remote.as_ref(),
                    secrets,
                    sleep: &sleep,
                };
                match &request {
                    ChangeRequest::Apply { host, runner } => ops.apply_host(host, runner.as_deref()),
                    ChangeRequest::Retire { host, runner } => ops.retire_legacy(host, runner).map(|m| vec![m]),
                    ChangeRequest::Decommission { host, runner } => {
                        ops.decommission_runner(host, runner).map(|m| vec![m])
                    }
                    ChangeRequest::Restart { runner } => ops.restart_runner(runner).map(|m| vec![m]),
                }
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r.map_err(|e| e.0));

            if let Some(op) = engine.desired.locked().operation.as_mut() {
                op.finished_at = Some(Utc::now());
                match result {
                    Ok(messages) => {
                        op.messages = messages;
                        op.status = OperationStatus::Succeeded;
                    }
                    Err(e) => {
                        op.status = OperationStatus::Failed;
                        op.error = Some(e);
                    }
                }
            }
            engine.view_changed();
            // Show the result against fresh facts, after any check already running.
            if let Err(e) = engine.desired_refresh_wait(Some(host_id)).await {
                tracing::warn!("check after a change failed: {e}");
            }
        });
    }

    pub fn desired_dismiss_operation(&self) {
        {
            let mut store = self.desired.locked();
            if store
                .operation
                .as_ref()
                .is_some_and(|o| o.status != OperationStatus::Running)
            {
                store.operation = None;
            }
        }
        self.view_changed();
    }
}
