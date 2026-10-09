//! Running Runner Fleet's scripts on worker hosts over SSH, ported from
//! `remote.py`. Scripts and their RF_* values travel on stdin, never in the
//! SSH command line. POSIX scripts get the values as plain shell variables, so
//! the programs they start don't inherit them; PowerShell scripts get them as
//! process environment variables. The runner's own `config.sh` and
//! `config.cmd` take the registration token, and on Windows the service
//! password, as arguments, so those do appear in the worker's process list
//! while configuration runs.

mod scripts;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::Value;

use super::github::ReleaseAsset;
use super::model::{CheckStatus, DoctorCheck, Host, LocalRunner, Runner, RunnerRole, SshTransport};
use super::process::{CommandRunner, NonZeroExit};
use super::py::{py_str, repr, splitlines, truthy};
use super::windows_logon::ServiceAction;
pub use scripts::probe_script;
use scripts::{INSTALL, RETIRE, WorkerScript, logs_script, service_script, worker_payload};
pub(crate) use scripts::{posix_doctor, windows_doctor};

/// The OpenSSH client every connection and `ssh -G` lookup uses. GUI apps get
/// a minimal PATH, and the configuration a review binds must come from the
/// same binary that later connects.
pub const SSH_PROGRAM: &str = "/usr/bin/ssh";

/// Options every OpenSSH connection adds to the user's configuration (a
/// hardening over the original): never prompt, never trust a new host key,
/// and never forward anything into a worker.
pub const OPENSSH_OPTIONS: [&str; 8] = [
    "-o",
    "BatchMode=yes",
    "-o",
    "StrictHostKeyChecking=yes",
    "-o",
    "ForwardAgent=no",
    "-o",
    "ClearAllForwardings=yes",
];

/// The longest OpenSSH `ConnectTimeout`, in seconds; shorter operation
/// timeouts lower it.
pub const MAX_CONNECT_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct RemoteError(pub String);

type Result<T> = std::result::Result<T, RemoteError>;

/// Passwords supplied for one operation and never stored. `Debug` hides them.
#[derive(Clone, Default)]
pub struct Transient(Option<String>);

impl Transient {
    /// No password.
    pub const NONE: Self = Self(None);

    pub fn new(value: Option<String>) -> Self {
        Self(value.filter(|v| !v.is_empty()))
    }

    pub fn get(&self) -> Option<&str> {
        self.0.as_deref()
    }

    pub fn or_empty(&self) -> &str {
        self.0.as_deref().unwrap_or("")
    }
}

impl std::fmt::Debug for Transient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Transient(<set>)"
        } else {
            "Transient(None)"
        })
    }
}

pub struct InstallRequest<'a> {
    pub asset: &'a ReleaseAsset,
    pub token: &'a str,
    pub organization_url: &'a str,
    pub runner_group: &'a str,
    /// The Windows `service_account` password the runner service logs on with.
    pub service_password: &'a Transient,
    /// The sudo password, on systemd hosts without non-interactive sudo.
    pub sudo_password: &'a Transient,
}

/// The registration token is never shown.
impl std::fmt::Debug for InstallRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallRequest")
            .field("asset", &self.asset)
            .field("organization_url", &self.organization_url)
            .field("runner_group", &self.runner_group)
            .field("service_password", &self.service_password)
            .field("sudo_password", &self.sudo_password)
            .finish_non_exhaustive()
    }
}

/// What operations need from worker hosts. `RemoteClient` is the real one.
pub trait FleetRemote: Send + Sync {
    fn probe(&self, host: &Host) -> Result<BTreeMap<String, LocalRunner>>;
    fn install(&self, host: &Host, runner: &Runner, request: &InstallRequest) -> Result<String>;
    fn retire(&self, host: &Host, runner: &Runner, token: &str, sudo_password: &Transient) -> Result<String>;
    fn decommission(&self, host: &Host, runner: &Runner, token: &str, sudo_password: &Transient) -> Result<String>;
    fn start(&self, host: &Host, runner: &Runner, sudo_password: &Transient) -> Result<String>;
    fn restart(&self, host: &Host, runner: &Runner, role: RunnerRole, sudo_password: &Transient) -> Result<String>;
    fn logs(&self, host: &Host, runner: &Runner, role: RunnerRole, lines: u32) -> Result<String>;
    fn doctor(&self, host: &Host) -> Vec<DoctorCheck>;
}

#[derive(Debug, Clone)]
pub struct Programs {
    pub ssh: String,
    pub tailscale: String,
}

impl Programs {
    /// The installed OpenSSH and Tailscale CLIs, located the same way the rest of the app finds them.
    pub fn installed() -> Self {
        Self {
            ssh: SSH_PROGRAM.into(),
            tailscale: crate::tailscale::cli_path().display().to_string(),
        }
    }
}

/// How to reach a machine, and which shell its scripts are written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Connection<'a> {
    /// Names the machine in errors.
    pub id: &'a str,
    /// `None` when the inventory's `ssh = "${VAR}"` is unset.
    pub target: Option<&'a str>,
    /// The `ssh` value as written, for the error when `target` is unset.
    pub setting: &'a str,
    pub transport: SshTransport,
    pub windows: bool,
}

impl<'a> From<&'a Host> for Connection<'a> {
    fn from(host: &'a Host) -> Self {
        Self {
            id: &host.id,
            target: host.ssh_target.as_deref(),
            setting: &host.ssh_setting,
            transport: host.ssh_transport,
            windows: host.is_windows(),
        }
    }
}

impl Default for Programs {
    fn default() -> Self {
        Self {
            ssh: "ssh".into(),
            tailscale: "tailscale".into(),
        }
    }
}

pub struct RemoteClient {
    runner: Arc<dyn CommandRunner>,
    timeout: Duration,
    programs: Programs,
}

impl std::fmt::Debug for RemoteClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteClient")
            .field("timeout", &self.timeout)
            .field("programs", &self.programs)
            .finish_non_exhaustive()
    }
}

impl RemoteClient {
    pub fn new(runner: Arc<dyn CommandRunner>, timeout: Duration, programs: Programs) -> Self {
        Self {
            runner,
            timeout,
            programs,
        }
    }

    fn require_target<'a>(&self, connection: impl Into<Connection<'a>>) -> Result<&'a str> {
        let connection = connection.into();
        connection.target.ok_or_else(|| {
            RemoteError(format!(
                "{}: SSH target is unset; export the variable referenced by {}",
                connection.id, connection.setting
            ))
        })
    }

    /// The command line and stdin used to run `script` on a machine.
    pub(crate) fn script_command<'a>(
        &self,
        connection: impl Into<Connection<'a>>,
        script: &str,
    ) -> Result<(Vec<String>, String)> {
        let connection = connection.into();
        let target = self.require_target(connection)?;
        let (remote_command, input) = if connection.windows {
            (
                "powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command \
                 \"$encoded = [Console]::In.ReadLine(); \
                 $source = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($encoded)); \
                 & ([ScriptBlock]::Create($source))\""
                    .to_string(),
                base64::engine::general_purpose::STANDARD.encode(script.as_bytes()) + "\n",
            )
        } else {
            ("bash -s".to_string(), script.to_string())
        };
        let command = match connection.transport {
            SshTransport::Tailscale => vec![
                self.programs.tailscale.clone(),
                "ssh".into(),
                target.into(),
                remote_command,
            ],
            SshTransport::Openssh => {
                let connect = self.timeout.as_secs().clamp(1, MAX_CONNECT_TIMEOUT_SECS);
                let mut command = vec![self.programs.ssh.clone()];
                command.extend(OPENSSH_OPTIONS.map(String::from));
                command.extend([
                    "-o".into(),
                    format!("ConnectTimeout={connect}"),
                    "--".into(),
                    target.into(),
                    remote_command,
                ]);
                command
            }
        };
        Ok((command, input))
    }

    pub(crate) fn run_script<'a>(
        &self,
        connection: impl Into<Connection<'a>>,
        script: &str,
        timeout: Option<Duration>,
    ) -> Result<String> {
        let (command, input) = self.script_command(connection, script)?;
        self.runner
            .run(
                &command,
                Some(&input),
                timeout.unwrap_or(self.timeout),
                NonZeroExit::Error,
            )
            .map(|r| r.stdout)
            .map_err(|e| RemoteError(e.to_string()))
    }

    fn run_worker_script(
        &self,
        host: &Host,
        values: &[(&str, String)],
        script: WorkerScript,
        timeout: Duration,
    ) -> Result<String> {
        self.run_script(host, &worker_payload(host, values, script), Some(timeout))
    }

    fn deregister(
        &self,
        host: &Host,
        name: &str,
        root: &str,
        token: &str,
        sudo_password: &Transient,
    ) -> Result<String> {
        let values = [
            ("RF_ROOT", root.to_string()),
            ("RF_NAME", name.to_string()),
            ("RF_TOKEN", token.to_string()),
            ("RF_SERVICE_MODE", host.service_mode.to_string()),
            ("RF_SUDO_PASSWORD", sudo_password.or_empty().to_string()),
        ];
        self.run_worker_script(host, &values, RETIRE, Duration::from_secs(120))
    }
}

impl FleetRemote for RemoteClient {
    fn probe(&self, host: &Host) -> Result<BTreeMap<String, LocalRunner>> {
        let output = self.run_script(host, &probe_script(host), None)?;
        if host.is_windows() {
            parse_windows_probe(&output)
        } else {
            parse_posix_probe(&output)
        }
    }

    fn install(&self, host: &Host, runner: &Runner, request: &InstallRequest) -> Result<String> {
        let values = [
            ("RF_ROOT", host.runner_path(runner)),
            ("RF_NAME", runner.name.clone()),
            ("RF_ORG_URL", request.organization_url.to_string()),
            ("RF_RUNNER_GROUP", request.runner_group.to_string()),
            ("RF_LABELS", host.labels_for(runner).join(",")),
            ("RF_TOKEN", request.token.to_string()),
            ("RF_ASSET_URL", request.asset.url.clone()),
            ("RF_ASSET_NAME", request.asset.name.clone()),
            ("RF_CHECKSUM", request.asset.sha256.clone().unwrap_or_default()),
            ("RF_SERVICE_MODE", host.service_mode.to_string()),
            ("RF_SERVICE_ACCOUNT", host.service_account.clone().unwrap_or_default()),
            ("RF_SERVICE_PASSWORD", request.service_password.or_empty().to_string()),
            ("RF_SUDO_PASSWORD", request.sudo_password.or_empty().to_string()),
        ];
        self.run_worker_script(host, &values, INSTALL, Duration::from_secs(300))
    }

    fn retire(&self, host: &Host, runner: &Runner, token: &str, sudo_password: &Transient) -> Result<String> {
        let legacy = runner
            .replaces
            .as_ref()
            .ok_or_else(|| RemoteError(format!("{} does not replace a legacy runner", runner.name)))?;
        self.deregister(host, &legacy.name, &legacy.directory, token, sudo_password)
    }

    fn decommission(&self, host: &Host, runner: &Runner, token: &str, sudo_password: &Transient) -> Result<String> {
        if !runner.decommission {
            return Err(RemoteError(format!(
                "{} is not declared decommission = true",
                runner.name
            )));
        }
        self.deregister(host, &runner.name, &host.runner_path(runner), token, sudo_password)
    }

    fn start(&self, host: &Host, runner: &Runner, sudo_password: &Transient) -> Result<String> {
        let script = service_script(host, runner, RunnerRole::Declared, ServiceAction::Start, sudo_password);
        self.run_script(host, &script, Some(Duration::from_secs(300)))
    }

    fn restart(&self, host: &Host, runner: &Runner, role: RunnerRole, sudo_password: &Transient) -> Result<String> {
        let timeout = if host.is_windows_logon() && role == RunnerRole::Declared {
            90
        } else {
            60
        };
        let script = service_script(host, runner, role, ServiceAction::Restart, sudo_password);
        self.run_script(host, &script, Some(Duration::from_secs(timeout)))
    }

    fn logs(&self, host: &Host, runner: &Runner, role: RunnerRole, lines: u32) -> Result<String> {
        self.run_script(
            host,
            &logs_script(host, runner, role, lines),
            Some(Duration::from_secs(30)),
        )
    }

    fn doctor(&self, host: &Host) -> Vec<DoctorCheck> {
        if host.ssh_target.is_none() {
            return vec![DoctorCheck::new(
                &host.id,
                "ssh",
                CheckStatus::Fail,
                format!("set the environment variable referenced by {}", host.ssh_setting),
            )];
        }
        let script = if host.is_windows() {
            windows_doctor(host)
        } else {
            posix_doctor(host)
        };
        match self
            .run_script(host, &script, Some(Duration::from_secs(30)))
            .and_then(|out| parse_doctor(&host.id, &out))
        {
            Ok(checks) => checks,
            Err(e) => vec![DoctorCheck::new(&host.id, "ssh", CheckStatus::Fail, e.0)],
        }
    }
}

pub fn parse_posix_probe(output: &str) -> Result<BTreeMap<String, LocalRunner>> {
    let mut runners = BTreeMap::new();
    for raw in splitlines(output) {
        if !raw.starts_with("RF1\t") {
            continue;
        }
        let fields: Vec<&str> = raw.splitn(6, '\t').collect();
        let [_, name, path, configured, service, version] = fields[..] else {
            return Err(RemoteError(format!("malformed runner probe row: {}", repr(raw))));
        };
        runners.insert(
            name.to_string(),
            LocalRunner {
                name: name.into(),
                path: path.into(),
                configured: configured == "true",
                service_state: if service.is_empty() {
                    "unknown".into()
                } else {
                    service.into()
                },
                version: (!version.is_empty()).then(|| version.to_string()),
            },
        );
    }
    if runners.is_empty() {
        return Err(RemoteError("host returned no recognizable runner probe data".into()));
    }
    Ok(runners)
}

pub fn parse_windows_probe(output: &str) -> Result<BTreeMap<String, LocalRunner>> {
    let malformed = |detail: String| RemoteError(format!("malformed Windows runner probe data: {detail}"));
    let value: Value = serde_json::from_str(output.trim()).map_err(|e| malformed(e.to_string()))?;
    let rows = match value {
        Value::Array(rows) => rows,
        other => vec![other],
    };
    let mut runners = BTreeMap::new();
    for row in rows {
        let Value::Object(row) = row else {
            return Err(malformed("row is not an object".into()));
        };
        let field = |key: &str| row.get(key).ok_or_else(|| malformed(format!("'{key}'")));
        let name = py_str(field("name")?);
        let configured = truthy(field("configured")?);
        let version = row.get("version").filter(|v| truthy(v)).map(py_str);
        runners.insert(
            name.clone(),
            LocalRunner {
                name,
                path: py_str(field("path")?),
                configured,
                service_state: py_str(field("service")?).to_lowercase(),
                version,
            },
        );
    }
    if runners.is_empty() {
        return Err(RemoteError("host returned no runner probe data".into()));
    }
    Ok(runners)
}

pub(crate) fn parse_doctor(host: &str, output: &str) -> Result<Vec<DoctorCheck>> {
    let checks: Vec<DoctorCheck> = splitlines(output)
        .into_iter()
        .filter(|raw| raw.starts_with("RFD\t"))
        .filter_map(|raw| {
            let fields: Vec<&str> = raw.splitn(4, '\t').collect();
            let [_, check, status, detail] = fields[..] else {
                return None;
            };
            Some(DoctorCheck::new(host, check, CheckStatus::parse(status)?, detail))
        })
        .collect();
    if checks.is_empty() {
        return Err(RemoteError("host returned no recognizable doctor data".into()));
    }
    Ok(checks)
}
