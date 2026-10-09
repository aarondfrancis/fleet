//! Guarded mutations, ported from `operations.py`: apply (install/reconcile),
//! retire, decommission, restart, plus read-only logs and doctor.
//!
//! Every mutation re-reads GitHub and the host first and refuses when the
//! facts don't match the inventory. Registration and removal tokens are minted
//! only at the moment of use and passed over SSH stdin. Nothing is ever run
//! with `--replace`, no configured runner directory is moved or deleted, and
//! secrets supplied for one operation are never stored.

use std::collections::BTreeMap;
use std::time::Duration;

use super::github::{FleetGithub, GithubError, ReleaseAsset};
use super::model::{
    CheckStatus, DoctorCheck, GithubRunner, Host, Inventory, LegacyRunner, LocalRunner, LookupError, Runner,
    RunnerRole, is_active,
};
use super::py::repr;
use super::remote::{FleetRemote, InstallRequest, RemoteError, Transient};
use super::state::label_drift;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct OperationError(pub String);

type Result<T> = std::result::Result<T, OperationError>;

fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(OperationError(message.into()))
}

impl From<LookupError> for OperationError {
    fn from(e: LookupError) -> Self {
        Self(e.0)
    }
}

impl From<GithubError> for OperationError {
    fn from(e: GithubError) -> Self {
        Self(e.0)
    }
}

impl From<RemoteError> for OperationError {
    fn from(e: RemoteError) -> Self {
        Self(e.0)
    }
}

/// GitHub's runners, by name.
type GithubRunners = BTreeMap<String, GithubRunner>;
/// A host's exact runner directories, by runner name.
type LocalRunners = BTreeMap<String, LocalRunner>;

/// Passwords supplied by the user for one operation (the Python version read
/// these from `RUNNER_FLEET_*` environment variables).
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    /// Windows `service_account` password, needed when installing.
    pub windows_service_password: Transient,
    /// sudo password for systemd hosts without non-interactive sudo.
    pub linux_sudo_password: Transient,
}

static NO_PASSWORD: Transient = Transient::NONE;

impl Secrets {
    /// The password a new runner's Windows service logs on with, on hosts that install one.
    fn service_password_for(&self, host: &Host) -> &Transient {
        if host.needs_service_password() {
            &self.windows_service_password
        } else {
            &NO_PASSWORD
        }
    }

    /// The sudo password, on hosts whose service changes go through sudo.
    fn sudo_password_for(&self, host: &Host) -> &Transient {
        if host.uses_sudo() {
            &self.linux_sudo_password
        } else {
            &NO_PASSWORD
        }
    }
}

pub struct Ops<'a> {
    pub inventory: &'a Inventory,
    pub github: &'a dyn FleetGithub,
    pub remote: &'a dyn FleetRemote,
    pub secrets: Secrets,
    /// Waits between GitHub re-reads; tests pass a no-op.
    pub sleep: &'a (dyn Fn(Duration) + Sync),
}

impl std::fmt::Debug for Ops<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ops")
            .field("organization", &self.inventory.organization.name)
            .field("secrets", &self.secrets)
            .finish_non_exhaustive()
    }
}

impl Ops<'_> {
    fn group(&self) -> &str {
        &self.inventory.organization.runner_group
    }

    /// The organization's runners, plus those registered to `repository`.
    fn github_runners(&self, repository: Option<&str>) -> std::result::Result<GithubRunners, GithubError> {
        let mut runners = self.github.list_runners(self.group())?;
        if let Some(repository) = repository {
            runners.extend(self.github.list_repository_runners(repository)?);
        }
        Ok(runners)
    }

    /// GitHub's runners, then the host's exact runner directories.
    fn read_host(&self, host: &Host, repository: Option<&str>) -> Result<(GithubRunners, LocalRunners)> {
        let runners = self.github_runners(repository)?;
        let local = self.remote.probe(host)?;
        Ok((runners, local))
    }

    /// Install or reconcile one host's desired runners. A host with a
    /// capacity-preserving migration must be applied one runner at a time.
    pub fn apply_host(&self, host_id: &str, runner_name: Option<&str>) -> Result<Vec<String>> {
        let host = self.inventory.find_host(host_id)?;
        check_enabled(host)?;
        let selected = select_runners(host, runner_name)?;
        check_one_runner_at_a_time(host, runner_name, &selected)?;
        if host.ssh_target.is_none() {
            return fail(format!(
                "{}: set the environment variable referenced by {}",
                host.id, host.ssh_setting
            ));
        }
        let (group_id, github_runners, local_runners) = self
            .apply_preflight(host)
            .map_err(|e| OperationError(format!("preflight failed without mutation: {e}")))?;
        check_apply_preconditions(&selected, &github_runners, &local_runners)?;
        let installing = selected.iter().any(|r| !github_runners.contains_key(&r.name));
        if host.needs_service_password() && installing && self.secrets.windows_service_password.get().is_none() {
            return fail(format!(
                "the password for {} is required to install a runner on {}",
                host.service_account.as_deref().unwrap_or_default(),
                host.id
            ));
        }

        let mut messages = Vec::new();
        let mut asset: Option<ReleaseAsset> = None;
        for runner in selected {
            let message_count = messages.len();
            let (current, installed) = match github_runners.get(&runner.name) {
                Some(current) => (current.clone(), false),
                None => {
                    if asset.is_none() {
                        asset = Some(self.github.latest_asset(host.platform.runner_asset())?);
                    }
                    let asset = asset.as_ref().expect("set above");
                    messages.push(self.install_runner(host, runner, asset)?);
                    (self.wait_for_runner(&runner.name)?, true)
                }
            };
            let start = installed || local_runners.get(&runner.name).is_some_and(|l| !l.service_active());
            self.reconcile_runner(host, runner, group_id, &current, start, &mut messages)
                .map_err(|e| {
                    OperationError(format!(
                        "reconciliation stopped for {}: {e}; inspect status before retrying",
                        runner.name
                    ))
                })?;
            if messages.len() == message_count {
                messages.push(format!("{} already satisfies desired state", runner.name));
            }
        }
        Ok(messages)
    }

    /// The trusted group's id, GitHub's runners and the host's runner directories.
    fn apply_preflight(&self, host: &Host) -> Result<(i64, GithubRunners, LocalRunners)> {
        let group_id = self.github.validate_runner_group_policy(self.group())?;
        let (github_runners, local_runners) = self.read_host(host, None)?;
        Ok((group_id, github_runners, local_runners))
    }

    /// Registers `runner` directly into the trusted group, its service stopped.
    fn install_runner(&self, host: &Host, runner: &Runner, asset: &ReleaseAsset) -> Result<String> {
        let token = self.github.registration_token()?;
        let request = InstallRequest {
            asset,
            token: &token,
            organization_url: &self.inventory.organization.url,
            runner_group: self.group(),
            service_password: self.secrets.service_password_for(host),
            sudo_password: self.secrets.sudo_password_for(host),
        };
        self.remote
            .install(host, runner, &request)
            .map_err(|e| OperationError(format!("install failed for {}: {e}", runner.name)))?;
        Ok(format!(
            "installed {} v{} at {}",
            runner.name,
            asset.version,
            host.runner_path(runner)
        ))
    }

    /// Labels and group first, so the runner is trusted before its service
    /// starts taking jobs.
    fn reconcile_runner(
        &self,
        host: &Host,
        runner: &Runner,
        group_id: i64,
        current: &GithubRunner,
        start: bool,
        messages: &mut Vec<String>,
    ) -> Result<()> {
        let desired = host.labels_for(runner);
        let (missing, unexpected) = label_drift(&desired, current);
        if !missing.is_empty() || !unexpected.is_empty() {
            self.github.set_labels(current.id, &desired)?;
            messages.push(format!("reconciled labels for {}", runner.name));
        }
        if current.in_desired_group != Some(true) {
            self.github.assign_group(group_id, current.id)?;
            messages.push(format!("assigned {} to {}", runner.name, self.group()));
        }
        if start {
            self.remote.start(host, runner, self.secrets.sudo_password_for(host))?;
            messages.push(format!("started {} after trusted-group reconciliation", runner.name));
        }
        Ok(())
    }

    /// Stop and deregister one declared legacy runner once its replacement is
    /// online, idle, in the trusted group with exact labels, and active. The
    /// legacy directory is preserved.
    pub fn retire_legacy(&self, host_id: &str, runner_name: &str) -> Result<String> {
        let host = self.inventory.find_host(host_id)?;
        let (runner, legacy) = retirement_target(host, runner_name)?;
        let (github_runners, local) = self
            .read_host(host, legacy.repository.as_deref())
            .map_err(|e| OperationError(format!("retirement preflight failed without mutation: {e}")))?;

        let replacement_github = github_runners.get(&runner.name);
        let replacement_local = local.get(&runner.name);
        let Some(replacement) = replacement_github.filter(|g| g.status == "online" && !g.busy) else {
            return fail(format!("replacement {} must be online and idle", runner.name));
        };
        if replacement.in_desired_group != Some(true) {
            return fail(format!("replacement {} must belong to {}", runner.name, self.group()));
        }
        let (missing, unexpected) = label_drift(&host.labels_for(runner), replacement);
        if !missing.is_empty() || !unexpected.is_empty() {
            let mut detail = Vec::new();
            if !missing.is_empty() {
                detail.push(format!("missing {}", missing.join(", ")));
            }
            if !unexpected.is_empty() {
                detail.push(format!("unexpected {}", unexpected.join(", ")));
            }
            return fail(format!(
                "replacement {} has label drift: {}",
                runner.name,
                detail.join("; ")
            ));
        }
        if !replacement_local.is_some_and(|l| l.configured && l.service_active()) {
            return fail(format!(
                "replacement {} must be configured with an active service",
                runner.name
            ));
        }
        let Some(legacy_github) = github_runners.get(&legacy.name) else {
            return fail(format!("legacy runner {} does not exist on GitHub", legacy.name));
        };
        if legacy_github.busy {
            return fail(format!("legacy runner {} is busy", legacy.name));
        }
        if !local.get(&legacy.name).is_some_and(|l| l.configured) {
            return fail(format!("legacy exact directory {} is not configured", legacy.directory));
        }
        self.deregister_legacy(host, runner, legacy)
            .map_err(|e| OperationError(format!("retirement failed for {}: {e}", legacy.name)))?;
        Ok(format!(
            "retired {}; preserved {} for recovery",
            legacy.name, legacy.directory
        ))
    }

    fn deregister_legacy(&self, host: &Host, runner: &Runner, legacy: &LegacyRunner) -> Result<String> {
        let token = self.github.removal_token(legacy.repository.as_deref())?;
        Ok(self
            .remote
            .retire(host, runner, &token, self.secrets.sudo_password_for(host))?)
    }

    /// Remove one idle runner declared `decommission = true`. Its directory is
    /// preserved; an offline registration with no local directory is deleted
    /// through the API.
    pub fn decommission_runner(&self, host_id: &str, runner_name: &str) -> Result<String> {
        let host = self.inventory.find_host(host_id)?;
        let runner = inventory_runner(host, runner_name)?;
        check_declared_for_decommission(runner)?;
        let path = host.runner_path(runner);
        let (github_runners, local) = self
            .read_host(host, None)
            .map_err(|e| OperationError(format!("decommission preflight failed without mutation: {e}")))?;

        let configured = local.get(&runner.name).is_some_and(|l| l.configured);
        let Some(current) = github_runners.get(&runner.name) else {
            if configured {
                return fail(format!(
                    "{}: exact local directory is configured but GitHub runner is missing; inspect manually",
                    runner.name
                ));
            }
            return Ok(format!(
                "{} is already decommissioned; {path} is preserved",
                runner.name
            ));
        };
        if current.busy {
            return fail(format!("{} is busy; refusing to decommission", runner.name));
        }
        if !configured && current.status == "online" {
            return fail(format!(
                "{}: GitHub reports it online but {path} is not configured; refusing to delete",
                runner.name
            ));
        }
        let removed = if configured {
            self.deregister_decommissioned(host, runner).map(drop)
        } else {
            self.github.delete_runner(current.id).map_err(OperationError::from)
        };
        removed.map_err(|e| {
            OperationError(format!(
                "decommission failed for {}: {e}; inspect status before retrying",
                runner.name
            ))
        })?;
        self.wait_for_removal(&runner.name)?;
        Ok(if configured {
            format!("decommissioned {}; preserved {path}", runner.name)
        } else {
            format!("deleted offline GitHub registration for {}", runner.name)
        })
    }

    fn deregister_decommissioned(&self, host: &Host, runner: &Runner) -> Result<String> {
        let token = self.github.removal_token(None)?;
        Ok(self
            .remote
            .decommission(host, runner, &token, self.secrets.sudo_password_for(host))?)
    }

    /// Restart one exact runner service (or its legacy runner's), only when
    /// GitHub confirms it is idle.
    pub fn restart_runner(&self, name: &str) -> Result<String> {
        let (host, runner, role) = self.inventory.find_runner(name)?;
        check_restartable(name, runner, role)?;
        let repository = match role {
            RunnerRole::Declared => None,
            RunnerRole::Legacy(legacy) => legacy.repository.as_deref(),
        };
        let runners = self
            .github_runners(repository)
            .map_err(|e| OperationError(format!("cannot verify runner is idle: {e}")))?;
        check_idle(name, &runners)?;
        self.remote
            .restart(host, runner, role, self.secrets.sudo_password_for(host))?;
        Ok(format!("restarted {name} on {}", host.id))
    }

    /// Read-only: tail one exact runner's diagnostic log.
    pub fn runner_logs(&self, name: &str, lines: u32) -> Result<String> {
        if !(1..=10_000).contains(&lines) {
            return fail("--lines must be between 1 and 10000");
        }
        let (host, runner, role) = self.inventory.find_runner(name)?;
        Ok(self.remote.logs(host, runner, role, lines)?)
    }

    fn wait_for_runner(&self, name: &str) -> Result<GithubRunner> {
        for attempt in 0..10 {
            // A failed re-read here propagates as-is, as in the original.
            let runners = self.github.list_runners(self.group())?;
            if let Some(found) = runners.get(name) {
                return Ok(found.clone());
            }
            if attempt < 9 {
                (self.sleep)(Duration::from_secs(2));
            }
        }
        fail(format!(
            "{name} was installed but did not appear in GitHub within 20s; inspect status before retrying"
        ))
    }

    fn wait_for_removal(&self, name: &str) -> Result<()> {
        for attempt in 0..10 {
            let runners = self.github.list_runners(self.group()).map_err(|e| {
                OperationError(format!(
                    "{name} was deregistered but GitHub could not be re-read: {e}; inspect status"
                ))
            })?;
            if !runners.contains_key(name) {
                return Ok(());
            }
            if attempt < 9 {
                (self.sleep)(Duration::from_secs(2));
            }
        }
        fail(format!(
            "{name} still appears in GitHub 20s after deregistration; inspect status before retrying"
        ))
    }
}

/// Apply refuses a disabled host.
pub(crate) fn check_enabled(host: &Host) -> Result<()> {
    if host.enabled {
        Ok(())
    } else {
        fail(format!("host {} is disabled", host.id))
    }
}

/// The desired runners an apply covers: every one, or the one named.
pub(crate) fn select_runners<'h>(host: &'h Host, name: Option<&str>) -> Result<Vec<&'h Runner>> {
    let Some(name) = name else {
        return Ok(host.desired_runners().collect());
    };
    if host.runners.iter().any(|r| r.name == name && r.decommission) {
        return fail(format!(
            "{name} is declared for decommission; use bin/fleet decommission {} --runner {name}",
            host.id
        ));
    }
    let selected: Vec<&Runner> = host.desired_runners().filter(|r| r.name == name).collect();
    if selected.is_empty() {
        return fail(format!("{} is not a desired runner on {}", repr(name), host.id));
    }
    Ok(selected)
}

/// A host migrating from legacy runners is applied one runner at a time, so
/// each legacy runner keeps its capacity until its replacement is healthy.
pub(crate) fn check_one_runner_at_a_time(host: &Host, runner_name: Option<&str>, selected: &[&Runner]) -> Result<()> {
    if runner_name.is_none() && selected.len() > 1 && selected.iter().any(|r| r.replaces.is_some()) {
        return fail(format!(
            "{} has a capacity-preserving migration; select exactly one --runner",
            host.id
        ));
    }
    Ok(())
}

/// Apply never replaces a registration or a configured directory it can't
/// match, and never restarts a runner that is mid-job.
fn check_apply_preconditions(selected: &[&Runner], github: &GithubRunners, local: &LocalRunners) -> Result<()> {
    for runner in selected {
        let current_github = github.get(&runner.name);
        let current_local = local.get(&runner.name);
        let configured = current_local.is_some_and(|l| l.configured);
        if current_github.is_none() && configured {
            return fail(format!(
                "{}: exact local directory is configured but GitHub runner is missing; refusing replacement",
                runner.name
            ));
        }
        if current_github.is_some() && !configured {
            return fail(format!(
                "{}: GitHub name exists but exact local directory is missing; refusing name collision",
                runner.name
            ));
        }
        if let (Some(g), Some(l)) = (current_github, current_local) {
            if g.status != "online" && l.service_active() {
                return fail(format!(
                    "{}: GitHub reports offline while the service is active; inspect logs before mutation",
                    runner.name
                ));
            }
            if g.busy && !l.service_active() {
                return fail(format!("{} is busy; refusing to restart its service", runner.name));
            }
        }
    }
    Ok(())
}

/// The inventory runner named `runner_name` on `host`, and the legacy runner it retires.
pub(crate) fn retirement_target<'h>(host: &'h Host, runner_name: &str) -> Result<(&'h Runner, &'h LegacyRunner)> {
    let Some(runner) = host.runners.iter().find(|r| r.name == runner_name) else {
        return fail(format!("{} is not a desired runner on {}", repr(runner_name), host.id));
    };
    let Some(legacy) = &runner.replaces else {
        return fail(format!("{runner_name} has no declared legacy runner"));
    };
    Ok((runner, legacy))
}

/// The inventory runner named `runner_name` on `host`.
pub(crate) fn inventory_runner<'h>(host: &'h Host, runner_name: &str) -> Result<&'h Runner> {
    host.runners.iter().find(|r| r.name == runner_name).ok_or_else(|| {
        OperationError(format!(
            "{} is not an inventory runner on {}",
            repr(runner_name),
            host.id
        ))
    })
}

/// Only a runner the inventory declares `decommission = true` is decommissioned.
pub(crate) fn check_declared_for_decommission(runner: &Runner) -> Result<()> {
    if runner.decommission {
        Ok(())
    } else {
        fail(format!(
            "{} is still desired; set decommission = true in inventory first",
            runner.name
        ))
    }
}

/// A runner declared for decommission is never restarted; its legacy runner may be.
pub(crate) fn check_restartable(name: &str, runner: &Runner, role: RunnerRole) -> Result<()> {
    if runner.decommission && role == RunnerRole::Declared {
        return fail(format!("{name} is declared for decommission; refusing restart"));
    }
    Ok(())
}

/// Restart needs GitHub to confirm the runner is idle.
pub(crate) fn check_idle(name: &str, github: &GithubRunners) -> Result<()> {
    match github.get(name) {
        None => fail(format!("{name} is missing on GitHub; cannot verify it is idle")),
        Some(runner) if runner.busy => fail(format!("{name} is busy; refusing restart")),
        Some(_) => Ok(()),
    }
}

/// Read-only health checks for the control machine and each selected host.
pub fn doctor(
    inventory: &Inventory,
    github: &dyn FleetGithub,
    remote: &dyn FleetRemote,
    host_id: Option<&str>,
) -> Result<Vec<DoctorCheck>> {
    let hosts = inventory.selected_hosts(host_id)?;
    let group = &inventory.organization.runner_group;
    let mut checks = vec![DoctorCheck::new(
        "control",
        "inventory",
        CheckStatus::Pass,
        "schema valid; runner names and exact paths are unique",
    )];
    let auth = github
        .auth_status()
        .and_then(|_| github.validate_runner_group_policy(group))
        .and_then(|_| github.list_runners(group));
    checks.push(match auth {
        Ok(_) => DoctorCheck::new(
            "control",
            "github-auth",
            CheckStatus::Pass,
            "authenticated with organization self-hosted runners access",
        ),
        Err(e) => DoctorCheck::new(
            "control",
            "github-auth",
            CheckStatus::Fail,
            format!("{e}; run: gh auth refresh -h github.com -s admin:org"),
        ),
    });
    for host in hosts {
        let host_checks = remote.doctor(host);
        let ssh_failed = host_checks
            .iter()
            .any(|c| c.check == "ssh" && c.status == CheckStatus::Fail);
        checks.extend(host_checks);
        if ssh_failed {
            continue;
        }
        match remote.probe(host) {
            Ok(local) => {
                for runner in &host.runners {
                    checks.extend(runner_checks(host, runner, &local, &inventory.organization.name));
                }
            }
            Err(e) => checks.push(DoctorCheck::new(&host.id, "runner-probe", CheckStatus::Fail, e.0)),
        }
    }
    Ok(checks)
}

/// One inventory runner's directory and service, and its legacy runner's if
/// that is still configured.
fn runner_checks(host: &Host, runner: &Runner, local: &LocalRunners, organization: &str) -> Vec<DoctorCheck> {
    let state = local.get(&runner.name);
    let path = host.runner_path(runner);
    if runner.decommission {
        let check = format!("decommission:{}", runner.name);
        return vec![if state.is_some_and(|s| s.configured) {
            DoctorCheck::new(
                &host.id,
                &check,
                CheckStatus::Warn,
                format!(
                    "still configured at {path}; run: bin/fleet decommission {} --runner {}",
                    host.id, runner.name
                ),
            )
        } else {
            DoctorCheck::new(
                &host.id,
                &check,
                CheckStatus::Pass,
                format!("not configured; {path} preserved"),
            )
        }];
    }
    let check = format!("runner:{}", runner.name);
    let mut checks = vec![match state.filter(|s| s.configured) {
        None => DoctorCheck::new(&host.id, &check, CheckStatus::Warn, format!("not configured at {path}")),
        Some(s) => DoctorCheck::new(
            &host.id,
            &check,
            if is_active(&s.service_state) {
                CheckStatus::Pass
            } else {
                CheckStatus::Fail
            },
            service_detail(s),
        ),
    }];
    if let Some(legacy) = &runner.replaces
        && let Some(s) = local.get(&legacy.name).filter(|s| s.configured)
    {
        let scope = legacy.repository.as_deref().unwrap_or(organization);
        checks.push(DoctorCheck::new(
            &host.id,
            &format!("legacy:{}", legacy.name),
            if is_active(&s.service_state) {
                CheckStatus::Pass
            } else {
                CheckStatus::Warn
            },
            format!("scope {scope}; {}", service_detail(s)),
        ));
    }
    checks
}

fn service_detail(local: &LocalRunner) -> String {
    format!(
        "service {}; runner {}; {}",
        local.service_state,
        local.version.as_deref().unwrap_or("version unknown"),
        local.path
    )
}
