//! Desired state (the inventory) and the observed facts it is compared with.
//! Ported from Runner Fleet's `model.py`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::py::{PathStyle, PurePath};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export, rename = "FleetLegacyRunner")]
#[serde(rename_all = "camelCase")]
pub struct LegacyRunner {
    pub name: String,
    pub directory: String,
    /// Set when the legacy runner was registered to one repository rather
    /// than the organization.
    pub repository: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export, rename = "FleetRunner")]
#[serde(rename_all = "camelCase")]
pub struct Runner {
    pub name: String,
    pub directory: String,
    pub labels: Vec<String>,
    /// Host labels this runner must not carry (e.g. keep a GUI slot off builds).
    pub exclude_labels: Vec<String>,
    pub replaces: Option<LegacyRunner>,
    /// Declared for removal: never installed or started, only deregistered.
    pub decommission: bool,
}

/// The OS and CPU architecture a host's runners are installed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export, rename = "FleetPlatform")]
pub enum Platform {
    #[serde(rename = "darwin-arm64")]
    DarwinArm64,
    #[serde(rename = "windows-arm64")]
    WindowsArm64,
    #[serde(rename = "windows-x64")]
    WindowsX64,
    #[serde(rename = "linux-x64")]
    LinuxX64,
}

impl Platform {
    const ALL: [Self; 4] = [Self::DarwinArm64, Self::WindowsArm64, Self::WindowsX64, Self::LinuxX64];

    /// The inventory spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DarwinArm64 => "darwin-arm64",
            Self::WindowsArm64 => "windows-arm64",
            Self::WindowsX64 => "windows-x64",
            Self::LinuxX64 => "linux-x64",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == value)
    }

    pub const fn is_windows(self) -> bool {
        match self {
            Self::WindowsArm64 | Self::WindowsX64 => true,
            Self::DarwinArm64 | Self::LinuxX64 => false,
        }
    }

    /// Service modes this platform allows.
    pub(crate) const fn service_modes(self) -> &'static [ServiceMode] {
        match self {
            Self::DarwinArm64 => &[ServiceMode::Launchagent],
            Self::WindowsArm64 | Self::WindowsX64 => &[ServiceMode::WindowsService, ServiceMode::WindowsLogon],
            Self::LinuxX64 => &[ServiceMode::Systemd],
        }
    }

    /// A host carries exactly one of these labels.
    pub(crate) const fn capability_labels(self) -> &'static [&'static str] {
        match self {
            Self::DarwinArm64 => &["macos-build"],
            Self::WindowsArm64 | Self::WindowsX64 => &["windows-build"],
            Self::LinuxX64 => &["linux-build", "linux-experimental"],
        }
    }

    /// The platform as GitHub's runner release assets name it.
    pub const fn runner_asset(self) -> &'static str {
        match self {
            Self::DarwinArm64 => "osx-arm64",
            Self::WindowsArm64 => "win-arm64",
            Self::WindowsX64 => "win-x64",
            Self::LinuxX64 => "linux-x64",
        }
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a host runs its runners as native services.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export, rename = "FleetServiceMode")]
#[serde(rename_all = "kebab-case")]
pub enum ServiceMode {
    /// A per-user LaunchAgent (macOS).
    Launchagent,
    Systemd,
    /// A Windows service in Session 0.
    WindowsService,
    /// A logon task in an auto-logged-on user's desktop session (for GUI tests).
    WindowsLogon,
}

impl ServiceMode {
    const ALL: [Self; 4] = [
        Self::Launchagent,
        Self::Systemd,
        Self::WindowsService,
        Self::WindowsLogon,
    ];

    /// The inventory spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Launchagent => "launchagent",
            Self::Systemd => "systemd",
            Self::WindowsService => "windows-service",
            Self::WindowsLogon => "windows-logon",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == value)
    }
}

impl std::fmt::Display for ServiceMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How scripts reach a host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export, rename = "FleetSshTransport")]
#[serde(rename_all = "lowercase")]
pub enum SshTransport {
    /// OpenSSH with the user's configuration.
    #[default]
    Openssh,
    /// `tailscale ssh`.
    Tailscale,
}

impl SshTransport {
    /// The inventory spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Openssh => "openssh",
            Self::Tailscale => "tailscale",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [Self::Openssh, Self::Tailscale]
            .into_iter()
            .find(|t| t.as_str() == value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export, rename = "FleetHost")]
#[serde(rename_all = "camelCase")]
pub struct Host {
    pub id: String,
    pub platform: Platform,
    /// The resolved SSH target; `None` when `ssh = "${VAR}"` is unset.
    pub ssh_target: Option<String>,
    /// The `ssh` value exactly as written in the inventory.
    pub ssh_setting: String,
    pub root: String,
    pub service_mode: ServiceMode,
    pub labels: Vec<String>,
    pub runners: Vec<Runner>,
    pub service_account: Option<String>,
    pub enabled: bool,
    pub ssh_transport: SshTransport,
}

impl Host {
    pub fn is_windows(&self) -> bool {
        self.platform.is_windows()
    }

    /// The runner runs in the service account's interactive desktop session
    /// (a logon task) instead of as a Session 0 service.
    pub fn is_windows_logon(&self) -> bool {
        self.service_mode == ServiceMode::WindowsLogon
    }

    /// Service changes go through sudo, which may need the user's password.
    pub fn uses_sudo(&self) -> bool {
        self.service_mode == ServiceMode::Systemd
    }

    /// Installing registers a Windows service that logs on as
    /// `service_account`, which needs its password. Logon-task runners use
    /// the account's existing auto-logon session instead.
    pub fn needs_service_password(&self) -> bool {
        self.is_windows() && self.service_account.is_some() && !self.is_windows_logon()
    }

    pub fn path_style(&self) -> PathStyle {
        if self.is_windows() {
            PathStyle::Windows
        } else {
            PathStyle::Posix
        }
    }

    pub fn runner_path(&self, runner: &Runner) -> String {
        PurePath::new(&self.root, self.path_style()).join(&runner.directory)
    }

    pub fn desired_runners(&self) -> impl Iterator<Item = &Runner> {
        self.runners.iter().filter(|r| !r.decommission)
    }

    /// Every inventory runner's name, each followed by the legacy runner it replaces.
    pub fn runner_names(&self) -> impl Iterator<Item = &str> {
        self.runners
            .iter()
            .flat_map(|r| std::iter::once(r.name.as_str()).chain(r.replaces.as_ref().map(|l| l.name.as_str())))
    }

    /// Host labels then runner labels, de-duplicated in order, minus the
    /// runner's `exclude_labels`.
    pub fn labels_for(&self, runner: &Runner) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for label in self.labels.iter().chain(&runner.labels) {
            if !out.contains(label) && !runner.exclude_labels.contains(label) {
                out.push(label.clone());
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export, rename = "FleetOrganization")]
#[serde(rename_all = "camelCase")]
pub struct Organization {
    pub name: String,
    pub url: String,
    pub runner_group: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inventory {
    pub organization: Organization,
    pub hosts: Vec<Host>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LookupError(pub String);

/// Which runner a name refers to: an inventory runner, or the legacy runner
/// one of them replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerRole<'a> {
    Declared,
    Legacy(&'a LegacyRunner),
}

impl Inventory {
    /// Enabled hosts, or exactly one enabled host.
    pub fn selected_hosts(&self, host_id: Option<&str>) -> Result<Vec<&Host>, LookupError> {
        let hosts = self.hosts.iter().filter(|h| h.enabled);
        match host_id {
            None => Ok(hosts.collect()),
            Some(id) => {
                let selected: Vec<&Host> = hosts.filter(|h| h.id == id).collect();
                if selected.is_empty() {
                    return Err(LookupError(format!("unknown or disabled host: {id}")));
                }
                Ok(selected)
            }
        }
    }

    pub fn find_host(&self, host_id: &str) -> Result<&Host, LookupError> {
        self.hosts
            .iter()
            .find(|h| h.id == host_id)
            .ok_or_else(|| LookupError(format!("unknown host: {host_id}")))
    }

    /// The host and inventory runner for `name`, and whether `name` is that
    /// runner or the legacy runner it replaces.
    pub fn find_runner(&self, name: &str) -> Result<(&Host, &Runner, RunnerRole<'_>), LookupError> {
        for host in &self.hosts {
            for runner in &host.runners {
                if runner.name == name {
                    return Ok((host, runner, RunnerRole::Declared));
                }
                if let Some(legacy) = runner.replaces.as_ref().filter(|r| r.name == name) {
                    return Ok((host, runner, RunnerRole::Legacy(legacy)));
                }
            }
        }
        Err(LookupError(format!("runner is not in inventory: {name}")))
    }
}

/// Every runner on `hosts`, with its host.
pub fn iter_runners<'a>(hosts: &'a [&'a Host]) -> impl Iterator<Item = (&'a Host, &'a Runner)> + 'a {
    hosts.iter().flat_map(|h| h.runners.iter().map(move |r| (*h, r)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubRunner {
    pub id: i64,
    pub name: String,
    pub status: String,
    pub busy: bool,
    pub labels: BTreeSet<String>,
    /// `None` when the desired runner group could not be found.
    pub in_desired_group: Option<bool>,
    pub custom_labels: BTreeSet<String>,
}

impl GithubRunner {
    pub fn new(id: i64, name: &str, status: &str, busy: bool, labels: &[&str], in_desired_group: Option<bool>) -> Self {
        Self {
            id,
            name: name.into(),
            status: status.into(),
            busy,
            labels: labels.iter().map(ToString::to_string).collect(),
            in_desired_group,
            custom_labels: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalRunner {
    pub name: String,
    pub path: String,
    pub configured: bool,
    pub service_state: String,
    pub version: Option<String>,
}

impl LocalRunner {
    pub fn new(name: &str, path: &str, configured: bool, service_state: &str, version: Option<&str>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            configured,
            service_state: service_state.into(),
            version: version.map(str::to_string),
        }
    }

    pub fn service_active(&self) -> bool {
        is_active(&self.service_state)
    }
}

/// launchd/systemd say `active`, Windows says `running`.
pub fn is_active(state: &str) -> bool {
    state == "active" || state == "running"
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FleetSnapshot {
    pub github: BTreeMap<String, GithubRunner>,
    pub local: BTreeMap<String, LocalRunner>,
    pub github_error: Option<String>,
    pub host_errors: BTreeMap<String, String>,
    pub policy_errors: Vec<String>,
}

/// One runner compared with GitHub and its host. A runner outside the
/// inventory has no host or service; busy and version stay `None` until
/// GitHub and the host report them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export, rename = "FleetStatusRow")]
#[serde(rename_all = "camelCase")]
pub struct StatusRow {
    pub runner: String,
    pub host: Option<String>,
    pub github_state: String,
    pub service_state: Option<String>,
    pub busy: Option<bool>,
    pub version: Option<String>,
    pub drift: String,
    pub desired: bool,
}

/// Runner Fleet's text for a status column that has no value.
pub const NO_VALUE: &str = "—";

impl StatusRow {
    /// Runner Fleet's `busy` column: `yes`, `no` or `—`.
    pub fn busy_text(&self) -> &'static str {
        match self.busy {
            Some(true) => "yes",
            Some(false) => "no",
            None => NO_VALUE,
        }
    }
}

/// A desired-state plan step, preserving Runner Fleet's serialized action names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "kebab-case")]
pub enum FleetPlanAction {
    Install,
    ReconcileLabels,
    AssignGroup,
    StartService,
    RetireLegacy,
    Decommission,
    NoChange,
    Restart,
    Inspect,
    InspectOffline,
    InspectGroup,
    PreserveLegacy,
    PreserveService,
    PreserveDecommission,
    RepairRegistration,
    RecoverLocal,
    HardenGroup,
}

impl FleetPlanAction {
    /// The existing Runner Fleet action name used in JSON and CLI output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::ReconcileLabels => "reconcile-labels",
            Self::AssignGroup => "assign-group",
            Self::StartService => "start-service",
            Self::RetireLegacy => "retire-legacy",
            Self::Decommission => "decommission",
            Self::NoChange => "no-change",
            Self::Restart => "restart",
            Self::Inspect => "inspect",
            Self::InspectOffline => "inspect-offline",
            Self::InspectGroup => "inspect-group",
            Self::PreserveLegacy => "preserve-legacy",
            Self::PreserveService => "preserve-service",
            Self::PreserveDecommission => "preserve-decommission",
            Self::RepairRegistration => "repair-registration",
            Self::RecoverLocal => "recover-local",
            Self::HardenGroup => "harden-group",
        }
    }

    /// Whether applying desired state carries out this step.
    pub const fn is_apply(self) -> bool {
        match self {
            Self::Install | Self::ReconcileLabels | Self::AssignGroup | Self::StartService => true,
            Self::RetireLegacy
            | Self::Decommission
            | Self::NoChange
            | Self::Restart
            | Self::Inspect
            | Self::InspectOffline
            | Self::InspectGroup
            | Self::PreserveLegacy
            | Self::PreserveService
            | Self::PreserveDecommission
            | Self::RepairRegistration
            | Self::RecoverLocal
            | Self::HardenGroup => false,
        }
    }
}

impl std::fmt::Display for FleetPlanAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS, schemars::JsonSchema)]
#[ts(export, rename = "FleetPlanItem")]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    pub action: FleetPlanAction,
    /// Whether apply carries out this action; blocked steps still cannot run.
    pub apply: bool,
    pub host: String,
    pub runner: String,
    pub reason: String,
    /// The Runner Fleet command line that carries this out, kept for parity
    /// and for the UI to map an item to the app action that does the same.
    pub command: Option<String>,
    pub blocked: bool,
}

impl PlanItem {
    pub fn new(action: FleetPlanAction, host: &str, runner: &str, reason: impl Into<String>) -> Self {
        Self {
            action,
            apply: action.is_apply(),
            host: host.into(),
            runner: runner.into(),
            reason: reason.into(),
            command: None,
            blocked: false,
        }
    }

    pub fn command(mut self, command: String) -> Self {
        self.command = Some(command);
        self
    }

    pub fn blocked(mut self) -> Self {
        self.blocked = true;
        self
    }

    /// Applying desired state carries out this step now.
    pub fn is_runnable(&self) -> bool {
        self.action.is_apply() && !self.blocked
    }
}

// Reclassify the typed action instead of trusting a classification supplied in JSON.
impl<'de> Deserialize<'de> for PlanItem {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Fields {
            action: FleetPlanAction,
            host: String,
            runner: String,
            reason: String,
            command: Option<String>,
            blocked: bool,
        }

        let fields = Fields::deserialize(deserializer)?;
        let mut item = Self::new(fields.action, &fields.host, &fields.runner, fields.reason);
        item.command = fields.command;
        item.blocked = fields.blocked;
        Ok(item)
    }
}

/// The outcome of one host check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export, rename = "FleetCheckStatus")]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckStatus {
    /// The spelling in doctor output and host check scripts.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [Self::Pass, Self::Warn, Self::Fail]
            .into_iter()
            .find(|s| s.as_str() == value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export, rename = "FleetDoctorCheck")]
#[serde(rename_all = "camelCase")]
pub struct DoctorCheck {
    pub host: String,
    pub check: String,
    pub status: CheckStatus,
    pub detail: String,
}

impl DoctorCheck {
    pub fn new(host: &str, check: &str, status: CheckStatus, detail: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            check: check.into(),
            status,
            detail: detail.into(),
        }
    }
}
