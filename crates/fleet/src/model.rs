//! Data model: persisted config, raw observations, and the derived view the UI renders.
//!
//! Observations are facts with timestamps. The view is a pure function of
//! (config, observations, now) — see `derive.rs`.

use std::collections::BTreeMap;

use crate::machine::{DiskStats, MachineReport, OsFamily};
use crate::runner_version::{RunnerRelease, RunnerUpdate};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

// Persisted settings. No secrets: tokens live in the macOS Keychain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default)]
    pub orgs: Vec<OrgConfig>,
    #[serde(default)]
    pub ignored_peers: Vec<IgnoredPeer>,
    /// Personal labels, keyed by inventory host or Tailscale node identity.
    #[serde(default)]
    pub machine_preferences: BTreeMap<String, MachinePreferences>,
    /// Send new alert and recovery notifications through the desktop OS.
    #[serde(default)]
    pub desktop_notifications: bool,
    #[serde(default)]
    pub demo_mode: bool,
    /// Serve the read-only local API on 127.0.0.1 (applies at next launch).
    #[serde(default = "default_true")]
    pub local_api: bool,
    /// The desired-state inventory (`fleet.toml`). Defaults to this
    /// repository's `inventory/fleet.toml`.
    #[serde(default)]
    pub inventory_path: Option<String>,
}

/// Matches the serde defaults, so a missing settings file and an empty one mean the same.
impl Default for AppConfig {
    fn default() -> Self {
        Self {
            orgs: Vec::new(),
            ignored_peers: Vec::new(),
            machine_preferences: BTreeMap::new(),
            desktop_notifications: false,
            demo_mode: false,
            local_api: true,
            inventory_path: None,
        }
    }
}

/// Names and notes kept only in this dashboard's local settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MachinePreferences {
    pub display_name: Option<String>,
    pub description: Option<String>,
}

/// A count for a view field. Saturates rather than wrapping (it never gets close).
pub(crate) fn count_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn default_true() -> bool {
    true
}

/// A machine's key for an inventory host: `inv:<host id>`.
pub(crate) fn inventory_machine_key(host_id: &str) -> String {
    format!("inv:{host_id}")
}

/// A machine's key for a tailnet peer outside the inventory: `ts:<node id>`.
pub(crate) fn tailnet_machine_key(node_id: &str) -> String {
    format!("ts:{node_id}")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OrgConfig {
    /// GitHub organization login, e.g. `acme`.
    pub login: String,
    /// API base, `https://api.github.com` unless GitHub Enterprise Server.
    #[serde(default = "default_api_base")]
    pub api_base: String,
    pub repo_scope: RepoScope,
    pub added_at: DateTime<Utc>,
    /// Where the API token comes from.
    #[serde(default)]
    pub auth: OrgAuth,
    #[serde(default)]
    pub kind: AccountKind,
    /// Repositories (names without the owner) whose repository-level runners are shown too.
    /// Monitoring only: inventory changes cover organization runners.
    #[serde(default)]
    pub runner_repos: Vec<String>,
}

impl OrgConfig {
    /// The repositories to scan for jobs. A personal account's runners only take jobs from
    /// their own repositories, so it scans its runner repositories.
    pub fn job_scope(&self) -> RepoScope {
        match self.kind {
            AccountKind::Organization => self.repo_scope.clone(),
            AccountKind::User => RepoScope::Selected {
                repos: self.runner_repos.clone(),
            },
        }
    }
}

/// What an added GitHub account is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum AccountKind {
    #[default]
    Organization,
    /// A personal account, which can only have repository-level runners.
    User,
}

/// How to watch an account being added.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AccountSetup {
    pub kind: AccountKind,
    pub repo_scope: RepoScope,
    pub runner_repos: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum OrgAuth {
    /// The GitHub CLI's login (`gh auth token`), held in memory only.
    GhCli,
    /// A token pasted in Settings, stored in this Mac's Keychain.
    #[default]
    Token,
}

pub fn default_api_base() -> String {
    "https://api.github.com".into()
}

/// Which repositories to scan for queued / in-progress jobs. GitHub has no
/// org-wide jobs endpoint, so this bounds API usage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RepoScope {
    /// The N most recently pushed repos in the org.
    Recent { max_repos: u32 },
    /// Exactly these repos (names without the org prefix).
    Selected { repos: Vec<String> },
    /// Runner status only; never scan jobs.
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredPeer {
    pub tailscale_node_id: String,
    pub name: String,
    pub ignored_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Timed<T> {
    pub at: DateTime<Utc>,
    pub value: T,
}

impl<T> Timed<T> {
    pub fn new(at: DateTime<Utc>, value: T) -> Self {
        Self { at, value }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Observations {
    pub tailscale: Option<Timed<TailnetSnapshot>>,
    pub tailscale_error: Option<Timed<String>>,
    /// Keyed by org login (lowercase).
    pub github: BTreeMap<String, OrgObservation>,
    /// Keyed by [`inventory_machine_key`] or [`tailnet_machine_key`].
    pub machines: BTreeMap<String, MachineObservation>,
    /// Recent `actions/runner` releases.
    pub runner_releases: Option<Timed<Vec<RunnerRelease>>>,
    /// The last attempt to read them, successful or not.
    pub runner_releases_checked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TailnetSnapshot {
    pub self_node_id: Option<String>,
    pub local_online: Option<bool>,
    pub tailnet_name: Option<String>,
    pub peers: Vec<TailnetPeer>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TailnetPeer {
    pub node_id: String,
    pub hostname: String,
    /// MagicDNS name without the trailing dot.
    pub dns_name: Option<String>,
    /// As reported by Tailscale: `macOS`, `linux`, `windows`, `iOS`, ...
    pub os: String,
    pub ips: Vec<String>,
    pub online: bool,
    pub last_seen: Option<DateTime<Utc>>,
    pub tags: Vec<String>,
    /// SSH host keys the node publishes to the tailnet (Tailscale SSH). Used to
    /// identify peers with Tailscale SSH enabled.
    pub ssh_host_keys: Vec<String>,
}

impl TailnetPeer {
    /// Desktop/server OSes that can host GitHub runners.
    pub fn can_host_runners(&self) -> bool {
        matches!(
            self.os.to_ascii_lowercase().as_str(),
            "macos" | "linux" | "windows" | "freebsd" | "openbsd"
        )
    }

    pub fn address(&self) -> String {
        self.dns_name
            .clone()
            .or_else(|| self.ips.first().cloned())
            .unwrap_or_else(|| self.hostname.clone())
    }
}

#[derive(Debug, Clone, Default)]
pub struct OrgObservation {
    pub runners: Option<Timed<Vec<GhRunner>>>,
    pub jobs: Option<Timed<JobScan>>,
    /// Latest runner-poll or runner authentication error.
    pub error: Option<Timed<String>>,
    /// Latest job-poll or job authentication error, independent of runner polls.
    pub jobs_error: Option<Timed<String>>,
    pub rate_limit: Option<RateLimit>,
    pub missing_token: bool,
    /// Repository-level runners by `owner/name`, each with its own read time.
    pub repo_runners: BTreeMap<String, Timed<Vec<GhRunner>>>,
    /// Why a runner repository couldn't be listed in the latest poll, by `owner/name`.
    pub repo_errors: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GhRunner {
    pub id: u64,
    pub name: String,
    pub os: String,
    /// `online` / `offline`
    pub status: String,
    pub busy: bool,
    pub labels: Vec<String>,
    pub ephemeral: Option<bool>,
    pub version: Option<String>,
    pub runner_group_id: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct JobScan {
    pub repos_scanned: Vec<String>,
    /// True when repositories, runs, or jobs were skipped by a cap or access failure.
    pub truncated: bool,
    pub jobs: Vec<GhJob>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GhJob {
    pub id: u64,
    pub run_id: u64,
    /// `owner/name`
    pub repo: String,
    pub name: String,
    pub workflow_name: Option<String>,
    /// `queued`, `in_progress`, `waiting`, `pending`, `requested`, `completed`
    pub status: String,
    pub labels: Vec<String>,
    pub runner_id: Option<u64>,
    pub runner_name: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub html_url: Option<String>,
    pub head_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RateLimit {
    pub limit: u32,
    pub remaining: u32,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct MachineObservation {
    /// Exact collection configuration, so edited hosts never inherit old facts.
    pub target: crate::machine::MachineTarget,
    pub report: Option<Timed<MachineReport>>,
    /// Outcome of the most recent attempt; None when it succeeded.
    pub last_error: Option<Timed<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum DataSource {
    Live,
    Demo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FleetView {
    pub generated_at: DateTime<Utc>,
    pub source: DataSource,
    pub summary: FleetSummary,
    /// Shared attention findings; summary.attention is this collection’s length.
    pub issues: Vec<crate::issues::FleetIssue>,
    /// Runner machines: in the inventory or with runners.
    pub machines: Vec<MachineView>,
    /// Other tailnet machines that could host runners (none of the above).
    pub other_machines: Vec<MachineView>,
    /// GitHub runners not tied to any machine (e.g. hosted runner pools).
    pub unmatched_runners: Vec<RunnerView>,
    pub ignored: Vec<DiscoveredPeerView>,
    pub queue: QueueView,
    /// Desired state from `inventory/fleet.toml`: errors, last check, the current change.
    pub plan: crate::fleet_plan::FleetPlanStatus,
    pub orgs: Vec<OrgStatusView>,
    pub tailnet: TailnetStatusView,
    pub monitoring: crate::monitoring::FleetMonitoring,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FleetSummary {
    pub machines: u32,
    pub machines_connected: u32,
    pub runners: u32,
    pub runners_online: u32,
    pub runners_busy: u32,
    pub runners_offline: u32,
    pub runners_unknown: u32,
    pub queued_jobs: u32,
    pub attention: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MachineView {
    /// Stable selection key: `ts:<tailscale node id>`, `inv:<inventory host>`
    pub id: String,
    pub display_name: String,
    pub description: Option<String>,
    pub host: String,
    pub os: Option<OsFamily>,
    pub os_version: Option<String>,
    pub tailnet: Option<TailnetPeer>,
    pub connection: MachineConnection,
    pub health: Option<HealthView>,
    pub diagnostics: Option<crate::machine_diagnostics::MachineDiagnostics>,
    pub monitoring: crate::monitoring::FleetMachineMonitoring,
    pub runners: Vec<RunnerView>,
    pub warnings: Vec<String>,
    /// What the inventory declares for this machine, if it's a fleet host.
    pub desired: Option<crate::fleet_plan::MachineDesired>,
}

impl MachineView {
    /// Inventory identity survives periods when the tailnet peer is unavailable.
    pub(crate) fn preferences_key(&self) -> String {
        self.desired
            .as_ref()
            .map_or_else(|| self.id.clone(), |d| inventory_machine_key(&d.host.id))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum MachineConnection {
    /// A recent successful read over SSH.
    Connected {
        checked_at: DateTime<Utc>,
    },
    /// An older read retained after a failure or missed poll.
    Stale {
        checked_at: DateTime<Utc>,
        reason: String,
    },
    Offline {
        last_seen: Option<DateTime<Utc>>,
    },
    Error {
        detail: String,
    },
    /// Disabled in the inventory; no automatic SSH checks.
    Disabled,
    /// No attempt has completed yet.
    Pending,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct HealthView {
    pub as_of: DateTime<Utc>,
    /// True when `as_of` is old: render greyed with "as of", never as zero.
    pub stale: bool,
    pub cpu_percent: Option<f32>,
    pub load_average: Option<[f64; 3]>,
    pub logical_cores: Option<u32>,
    #[ts(type = "number | null")]
    pub memory_total_bytes: Option<u64>,
    #[ts(type = "number | null")]
    pub memory_available_bytes: Option<u64>,
    pub disks: Vec<DiskStats>,
    #[ts(type = "number | null")]
    pub uptime_secs: Option<u64>,
    /// Native boot-session identity; changing uptime alone does not prove a reboot.
    pub boot_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Tri {
    Yes,
    No,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RunnerActivity {
    /// GitHub reports it running a job.
    Busy {
        since: Option<DateTime<Utc>>,
    },
    /// GitHub reports it online and idle.
    Idle,
    Offline,
    /// No fresh source: show last known facts as stale.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RunnerView {
    /// Stable selection key: `gh:<org>:<id>`, or `inv:<host>:<runner>` for an
    /// inventory runner GitHub doesn't list.
    pub key: String,
    pub name: String,
    pub org: Option<String>,
    pub machine_id: Option<String>,
    pub installed: Tri,
    pub connected: Tri,
    pub activity: RunnerActivity,
    pub github: Option<GithubRunnerFacts>,
    pub local: Option<LocalRunnerFacts>,
    /// Only set when the GitHub jobs API actually links a job to this runner.
    pub current_job: Option<JobView>,
    pub warnings: Vec<String>,
    /// What the inventory says about this runner, if its machine is a fleet host.
    pub desired: Option<crate::fleet_plan::RunnerDesired>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct GithubRunnerFacts {
    #[ts(type = "number")]
    pub id: u64,
    pub status: String,
    pub busy: bool,
    pub labels: Vec<String>,
    pub os: String,
    pub version: Option<String>,
    pub ephemeral: Option<bool>,
    pub as_of: DateTime<Utc>,
    pub stale: bool,
    /// `owner/name` for a repository-level runner; None for an organization runner.
    pub repo: Option<String>,
    /// Set when `version` is behind the latest `actions/runner` release.
    pub update: Option<RunnerUpdate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LocalRunnerFacts {
    pub install_dir: String,
    pub configured: bool,
    pub service_state: String,
    pub version: Option<String>,
    pub as_of: DateTime<Utc>,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    #[ts(type = "number")]
    pub id: u64,
    pub name: String,
    pub workflow_name: Option<String>,
    pub repo: String,
    pub branch: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub html_url: Option<String>,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredPeerView {
    pub peer: TailnetPeer,
    pub ignored_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct QueueView {
    pub jobs: Vec<QueuedJobView>,
    /// Queued jobs whose labels no self-hosted runner in the org could ever match
    /// (typically GitHub-hosted `ubuntu-latest`). Counted, not listed.
    pub other_queued: u32,
    /// False when job data is missing or stale for some org.
    pub complete: bool,
    pub as_of: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct QueuedJobView {
    pub org: String,
    pub job: JobView,
    pub created_at: Option<DateTime<Utc>>,
    /// Runner keys whose labels are a superset of the job's labels.
    pub compatible_runners: Vec<String>,
    pub compatible_online: u32,
    pub compatible_idle: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OrgStatusView {
    pub login: String,
    pub has_token: bool,
    pub runners_as_of: Option<DateTime<Utc>>,
    pub jobs_as_of: Option<DateTime<Utc>>,
    pub repos_scanned: u32,
    pub jobs_truncated: bool,
    pub error: Option<String>,
    /// `owner/name: reason` for each runner repository that couldn't be listed.
    pub runner_repo_errors: Vec<String>,
    pub rate_limit: Option<RateLimit>,
    pub repo_scope: RepoScope,
    pub auth: OrgAuth,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TailnetStatusView {
    pub local_online: Option<bool>,
    pub available: bool,
    pub tailnet_name: Option<String>,
    pub as_of: Option<DateTime<Utc>>,
    pub error: Option<String>,
}
