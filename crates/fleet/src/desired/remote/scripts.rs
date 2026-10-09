//! The scripts sent to worker hosts: guarded worker scripts with their RF_*
//! values, service start/restart, logs, and the read-only probe and doctor.

use super::super::model::{Host, Platform, Runner, RunnerRole, ServiceMode};
use super::super::py::{ps_quote, sh_quote, sh_remote_path};
use super::super::windows_logon::{self, ServiceAction};
use super::Transient;

const WORKER_INSTALL_SH: &str = include_str!("../../../scripts/worker-install.sh");
const WORKER_INSTALL_PS1: &str = include_str!("../../../scripts/worker-install.ps1");
const WORKER_RETIRE_SH: &str = include_str!("../../../scripts/worker-retire.sh");
const WORKER_RETIRE_PS1: &str = include_str!("../../../scripts/worker-retire.ps1");
const WORKER_NIXOS_SERVICE: &str = include_str!("../../../scripts/worker-nixos-service.py");
/// `run_as_root`, defined ahead of every POSIX script that changes services.
const RUN_AS_ROOT_SH: &str = include_str!("../../../scripts/run-as-root.sh");

/// A guarded worker script, in each host shell.
#[derive(Debug, Clone, Copy)]
pub(super) struct WorkerScript {
    posix: &'static str,
    powershell: &'static str,
}

pub(super) const INSTALL: WorkerScript = WorkerScript {
    posix: WORKER_INSTALL_SH,
    powershell: WORKER_INSTALL_PS1,
};

/// Stops and deregisters one exact runner directory, keeping its files.
pub(super) const RETIRE: WorkerScript = WorkerScript {
    posix: WORKER_RETIRE_SH,
    powershell: WORKER_RETIRE_PS1,
};

/// `script` for the host's shell, preceded by its RF_* values. POSIX hosts
/// also get `run_as_root` and the NixOS service helper.
pub(super) fn worker_payload(host: &Host, values: &[(&str, String)], script: WorkerScript) -> String {
    if host.is_windows() {
        powershell_preamble(values) + script.powershell
    } else {
        let helper = [("RF_NIXOS_SERVICE_HELPER", WORKER_NIXOS_SERVICE.to_string())];
        sh_preamble(values) + &sh_preamble(&helper) + RUN_AS_ROOT_SH + script.posix
    }
}

fn sh_preamble(values: &[(&str, String)]) -> String {
    let mut out = String::new();
    for (key, value) in values {
        let quoted = if *key == "RF_ROOT" {
            sh_remote_path(value)
        } else {
            sh_quote(value)
        };
        out.push_str(&format!("{key}={quoted}\n"));
    }
    out
}

fn powershell_preamble(values: &[(&str, String)]) -> String {
    values
        .iter()
        .map(|(k, v)| format!("$env:{k} = {}\n", ps_quote(v)))
        .collect()
}

/// Finds the Actions runner service whose executable lives under `$path`.
const WIN_FIND_SERVICE: &str = "$needle = $path.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar\n\
$service = Get-CimInstance Win32_Service | Where-Object { $_.PathName.IndexOf($needle, [StringComparison]::OrdinalIgnoreCase) -ge 0 -and $_.Name -like 'actions.runner.*' } | Select-Object -First 1\n";

/// The name and exact directory `role` refers to.
fn role_target<'a>(host: &Host, runner: &'a Runner, role: RunnerRole<'a>) -> (&'a str, String) {
    match role {
        RunnerRole::Declared => (&runner.name, host.runner_path(runner)),
        RunnerRole::Legacy(legacy) => (&legacy.name, legacy.directory.clone()),
    }
}

/// Starts or restarts one exact runner's native service. A declared runner on
/// a logon-task host is its logon task instead.
pub(super) fn service_script(
    host: &Host,
    runner: &Runner,
    role: RunnerRole,
    action: ServiceAction,
    sudo_password: &Transient,
) -> String {
    let (name, path) = role_target(host, runner, role);
    if host.is_windows_logon() && role == RunnerRole::Declared {
        return windows_logon::task_script(&path, &runner.name, action);
    }
    if host.is_windows() {
        let (cmdlet, verb) = match action {
            ServiceAction::Start => ("Start-Service", "started"),
            ServiceAction::Restart => ("Restart-Service", "restarted"),
        };
        return format!(
            "$ErrorActionPreference = 'Stop'\n$path = {}\n{WIN_FIND_SERVICE}\
if (-not $service) {{ throw \"No Actions runner service found for exact path $path\" }}\n\
{cmdlet} -Name $service.Name\n\
Write-Output \"{verb} $($service.Name)\"\n",
            ps_quote(&path)
        );
    }
    let unit = sh_quote(&format!("runner-fleet-{}.service", runner.name));
    let privilege = if host.uses_sudo() { "run_as_root " } else { "" };
    let (nixos, svc, verb) = match action {
        ServiceAction::Start => (
            format!(
                "if [ ! -f .service ]; then\n\
run_as_root python3 -c {helper} \"$root\" {name} \"$(id -un)\"\n\
fi\n\
test \"$(cat .service)\" = {unit}\n\
touch .fleet-service-ready\n\
run_as_root systemctl start {unit}\n",
                helper = sh_quote(WORKER_NIXOS_SERVICE),
                name = sh_quote(&runner.name),
            ),
            format!("{privilege}./svc.sh start\n"),
            "started",
        ),
        ServiceAction::Restart => (
            format!(
                "test \"$(cat .service)\" = {unit}\n\
test -f .fleet-service-ready\n\
run_as_root systemctl restart {unit}\n"
            ),
            format!("{privilege}./svc.sh stop\n{privilege}./svc.sh start\n"),
            "restarted",
        ),
    };
    let sudo = if host.uses_sudo() {
        format!(
            "RF_SUDO_PASSWORD={}\n{RUN_AS_ROOT_SH}",
            sh_quote(sudo_password.or_empty())
        )
    } else {
        String::new()
    };
    format!(
        "set -euo pipefail\nroot={root}\n{sudo}\n\
test -x \"$root/svc.sh\" || {{ echo \"missing svc.sh at $root\" >&2; exit 1; }}\n\
cd \"$root\"\n\
if [ -f /etc/NIXOS ]; then\n\
{nixos}\
else\n\
{svc}\
fi\n\
echo \"{verb} {shown}\"\n",
        root = sh_remote_path(&path),
        shown = sh_quote(name),
    )
}

/// Read-only: the tail of the newest runner diagnostic log in one exact directory.
pub(super) fn logs_script(host: &Host, runner: &Runner, role: RunnerRole, lines: u32) -> String {
    let (_, path) = role_target(host, runner, role);
    if host.is_windows() {
        format!(
            "$ErrorActionPreference = 'Stop'\n$path = {}\n\
$log = Get-ChildItem -LiteralPath (Join-Path $path '_diag') -Filter 'Runner_*.log' -File |\n\
  Sort-Object LastWriteTime -Descending | Select-Object -First 1\n\
if (-not $log) {{ throw \"No runner diagnostic log found under $path\" }}\n\
Write-Output \"==> $($log.FullName) <==\"\n\
Get-Content -LiteralPath $log.FullName -Tail {lines}\n",
            ps_quote(&path)
        )
    } else {
        format!(
            "set -euo pipefail\nroot={}\n\
log=\"$(find \"$root/_diag\" -maxdepth 1 -type f -name 'Runner_*.log' -print 2>/dev/null | sort | tail -n 1)\"\n\
test -n \"$log\" || {{ echo \"no runner diagnostic log found under $root\" >&2; exit 1; }}\n\
echo \"==> $log <==\"\n\
tail -n {lines} \"$log\"\n",
            sh_remote_path(&path)
        )
    }
}

/// Every inventory runner and the legacy runner it replaces.
fn probe_targets(host: &Host) -> Vec<(String, String)> {
    let mut targets = Vec::new();
    for runner in &host.runners {
        targets.push((runner.name.clone(), host.runner_path(runner)));
        if let Some(old) = &runner.replaces {
            targets.push((old.name.clone(), old.directory.clone()));
        }
    }
    targets
}

/// Read-only: reports whether each exact directory is configured, its service
/// state and runner version.
pub fn probe_script(host: &Host) -> String {
    if host.is_windows() {
        return windows_probe(host);
    }
    let service_check = if host.service_mode == ServiceMode::Launchagent {
        "label=\"$(cat \"$path/.service\" 2>/dev/null || true)\"; \
if [ -z \"$label\" ]; then service=uninstalled; \
elif launchctl print \"gui/$(id -u)/$label\" >/dev/null 2>&1; then service=active; \
else service=inactive; fi"
    } else {
        "label=\"$(cat \"$path/.service\" 2>/dev/null || true)\"; \
if [ -z \"$label\" ]; then service=uninstalled; \
else service=\"$(systemctl is-active \"$label\" 2>/dev/null || true)\"; \
[ -n \"$service\" ] || service=unknown; fi"
    };
    let mut blocks = vec!["set -u".to_string()];
    for (name, path) in probe_targets(host) {
        blocks.push(format!(
            "path={path}\n\
configured=false; service=missing; version=''\n\
if [ -f \"$path/.runner\" ]; then\n\
  configured=true\n\
  {service_check}\n\
  if [ -x \"$path/bin/Runner.Listener\" ]; then\n\
    version=\"$(\"$path/bin/Runner.Listener\" --version 2>/dev/null | tail -n 1 | tr '\\t\\r\\n' '   ')\"\n\
  fi\n\
fi\n\
printf 'RF1\\t%s\\t%s\\t%s\\t%s\\t%s\\n' {name} \"$path\" \"$configured\" \"$service\" \"$version\"\n",
            path = sh_remote_path(&path),
            name = sh_quote(&name),
        ));
    }
    blocks.join("\n")
}

fn windows_probe(host: &Host) -> String {
    let mut rows = vec!["$ErrorActionPreference = 'Stop'".to_string(), "$rows = @()".to_string()];
    for (name, path) in probe_targets(host) {
        rows.push(format!(
            "$path = {path}\n\
$configured = Test-Path -LiteralPath (Join-Path $path '.runner')\n\
$state = if ($configured) {{ 'uninstalled' }} else {{ 'missing' }}\n\
$version = $null\n\
$taskFile = Join-Path $path '.fleet-task'\n\
if ($configured -and (Test-Path -LiteralPath $taskFile)) {{\n\
  $task = Get-ScheduledTask -TaskName (Get-Content -LiteralPath $taskFile -Raw).Trim() -ErrorAction SilentlyContinue\n\
  $bin = (Join-Path $path 'bin') + [IO.Path]::DirectorySeparatorChar\n\
  $listener = Get-Process -Name Runner.Listener -ErrorAction SilentlyContinue | Where-Object {{ $_.Path -and $_.Path.StartsWith($bin, [StringComparison]::OrdinalIgnoreCase) }} | Select-Object -First 1\n\
  $state = if ($listener) {{ 'running' }} elseif (-not $task) {{ 'uninstalled' }} elseif ($task.State -eq 'Disabled') {{ 'disabled' }} else {{ 'stopped' }}\n\
}} elseif ($configured) {{\n\
{WIN_FIND_SERVICE}\
  if ($service) {{ $state = $service.State.ToLowerInvariant() }}\n\
}}\n\
if ($configured) {{\n\
  $listener = Join-Path $path 'bin\\Runner.Listener.exe'\n\
  if (Test-Path -LiteralPath $listener) {{ $version = (& $listener --version 2>$null | Select-Object -Last 1) }}\n\
}}\n\
$rows += [pscustomobject]@{{ name = {name}; path = $path; configured = $configured; service = $state; version = $version }}\n",
            path = ps_quote(&path),
            name = ps_quote(&name),
        ));
    }
    rows.push("ConvertTo-Json -Compress -Depth 3 -InputObject @($rows)".into());
    rows.join("\n")
}

/// Read-only host checks: SSH identity, prerequisites, disk and power policy.
pub(crate) fn posix_doctor(host: &Host) -> String {
    let darwin = host.platform == Platform::DarwinArm64;
    let required = if darwin {
        "curl tar git shasum"
    } else {
        "curl tar git sha256sum systemctl sudo"
    };
    let power = if darwin {
        "pmset -g custom 2>/dev/null | tr '\\n' ';' | cut -c 1-1000"
    } else {
        "systemctl is-enabled sleep.target suspend.target hibernate.target 2>/dev/null | tr '\\n' ',' | cut -c 1-500"
    };
    let head = format!(
        "set -u\n\
printf 'RFD\\tssh\\tpass\\tconnected as %s on %s\\n' \"$(id -un)\" \"$(hostname -s)\"\n\
for command in {required}; do\n\
  if command -v \"$command\" >/dev/null 2>&1; then\n\
    printf 'RFD\\tprerequisite:%s\\tpass\\tfound\\n' \"$command\"\n\
  else\n\
    printf 'RFD\\tprerequisite:%s\\tfail\\tnot found\\n' \"$command\"\n\
  fi\n\
done\n\
available=\"$(df -Pk \"$HOME\" | awk 'NR==2 {{print $4}}')\"\n\
printf 'RFD\\tdisk\\tpass\\t%s KiB available on home volume\\n' \"$available\"\n"
    );
    if darwin {
        format!(
            "{head}power=\"$({power})\"\n\
printf 'RFD\\tpower\\twarn\\t%s\\n' \"$power\"\n\
printf 'RFD\\tservice-mode\\twarn\\tLaunchAgents require user login after reboot; verify the LaunchAgent owner is logged in\\n'\n"
        )
    } else {
        format!(
            "{head}if sudo -n true >/dev/null 2>&1; then\n\
  printf 'RFD\\tsudo\\tpass\\tnon-interactive sudo works\\n'\n\
else\n\
  printf 'RFD\\tsudo\\twarn\\tinstall/apply needs non-interactive sudo for dependencies and systemd\\n'\n\
fi\n\
power=\"$({power})\"\n\
printf 'RFD\\tpower\\twarn\\t%s\\n' \"$power\"\n\
printf 'RFD\\tservice-mode\\tpass\\tsystemd starts before login and survives reboot\\n'\n"
        )
    }
}

const WINDOWS_DOCTOR_COMMON: &str = r#"$ErrorActionPreference = 'Continue'
Write-Output "RFD`tssh`tpass`tconnected as $env:USERNAME on $env:COMPUTERNAME"
$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  Write-Output "RFD`tadministrator`tpass`televated service management is available"
} else {
  Write-Output "RFD`tadministrator`tfail`tinstall/apply and service operations require an elevated SSH identity"
}
foreach ($command in @('curl.exe', 'tar.exe', 'git.exe')) {
  if (Get-Command $command -ErrorAction SilentlyContinue) {
    Write-Output "RFD`tprerequisite:$command`tpass`tfound"
  } else {
    Write-Output "RFD`tprerequisite:$command`tfail`tnot found"
  }
}
$services = Get-CimInstance Win32_Service | Where-Object { $_.Name -like 'actions.runner.*' }
foreach ($service in $services) {
  Write-Output "RFD`tservice-identity:$($service.Name)`twarn`truns as $($service.StartName); verify profile and certificate requirements"
}
$drive = Get-PSDrive -Name C
Write-Output "RFD`tdisk`tpass`t$([math]::Round($drive.Free / 1GB, 1)) GiB available on C:"
$scheme = (powercfg.exe /GETACTIVESCHEME 2>&1 | Out-String).Trim() -replace "`r?`n", '; '
Write-Output "RFD`tpower`twarn`t$scheme; review unattended reboot policy"
function Get-AcPowerIndex([string]$Subgroup, [string]$Setting) {
  $text = (powercfg.exe /QH SCHEME_CURRENT $Subgroup $Setting 2>&1 | Out-String)
  if ($text -match 'Current AC Power Setting Index:\s*0x([0-9a-fA-F]+)') { return [Convert]::ToInt64($Matches[1], 16) }
  return $null
}
function Write-AcTimeoutCheck([string]$Check, $Seconds, [string]$Description) {
  if ($null -eq $Seconds) {
    Write-Output "RFD`t$Check`twarn`tcould not read the AC $Description timeout"
  } elseif ($Seconds -eq 0) {
    Write-Output "RFD`t$Check`tpass`tAC $Description never happens"
  } else {
    Write-Output "RFD`t$Check`twarn`tAC $Description after $Seconds seconds; runner hosts should never sleep or turn the display off"
  }
}
Write-AcTimeoutCheck 'power:ac-sleep' (Get-AcPowerIndex 'SUB_SLEEP' 'STANDBYIDLE') 'sleep'
Write-AcTimeoutCheck 'power:ac-display' (Get-AcPowerIndex 'SUB_VIDEO' 'VIDEOIDLE') 'display off'
$lid = Get-AcPowerIndex '4f971e89-eebd-4455-a8de-9e59040e7347' '5ca83367-6e45-459f-a27b-476b1d01c936'
if ($null -eq $lid) {
  Write-Output "RFD`tpower:ac-lid`tpass`tno lid action on this host"
} elseif ($lid -eq 0) {
  Write-Output "RFD`tpower:ac-lid`tpass`tclosing the lid on AC does nothing"
} else {
  Write-Output "RFD`tpower:ac-lid`twarn`tclosing the lid on AC sleeps or shuts down (action $lid)"
}
# On Modern Standby (S0) hosts, turning the display off enters standby even
# when sleep is never, so networking must stay connected in standby or the
# runner, Tailscale, and SSH all drop until someone wakes the machine.
$sleepStates = (powercfg.exe /A 2>&1 | Out-String)
if ((($sleepStates -split 'not available')[0]) -match 'S0 Low Power Idle') {
  $connectivity = Get-AcPowerIndex 'fea3413e-7e05-4911-9a71-700331f1c294' 'f15576e8-98b7-4186-b944-eafa664402d9'
  $policy = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Policies\Microsoft\Power\PowerSettings\f15576e8-98b7-4186-b944-eafa664402d9' -Name ACSettingIndex -ErrorAction SilentlyContinue
  $source = if ($null -ne $policy) { "policy ACSettingIndex=$($policy.ACSettingIndex)" } else { 'power plan' }
  if ($connectivity -eq 1) {
    Write-Output "RFD`tpower:connected-standby`tpass`tModern Standby keeps networking connected on AC ($source)"
  } else {
    Write-Output "RFD`tpower:connected-standby`twarn`tModern Standby disconnects networking on AC ($source, effective=$connectivity); the runner goes offline when the display turns off. Enable AC networking connectivity in standby; a policy value takes effect after reboot"
  }
}
"#;

/// Read-only Windows checks; logon-task hosts also check auto-logon and the session.
pub(crate) fn windows_doctor(host: &Host) -> String {
    let mode = if host.is_windows_logon() {
        windows_logon::doctor_script(host.service_account.as_deref().unwrap_or_default())
    } else {
        "Write-Output \"RFD`tservice-mode`tpass`tWindows service starts without interactive login\"\n".to_string()
    };
    format!("{WINDOWS_DOCTOR_COMMON}{mode}")
}
