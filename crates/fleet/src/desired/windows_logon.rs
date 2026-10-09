//! `service_mode = "windows-logon"`: the runner runs inside the service
//! account's auto-logged-on desktop session (a Scheduled Task named
//! `runner-fleet-NAME`) instead of a Session 0 service, so GUI/WebView tests
//! see a real desktop. Ported from Runner Fleet's `remote.py`.

use super::py::ps_quote;

/// What a service or logon-task script does to the runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServiceAction {
    Start,
    /// Stop it first (a logon task also stops its listener).
    Restart,
}

const TASK_SCRIPT: &str = r#"$ErrorActionPreference = 'Stop'
$path = @PATH@
$task = @TASK@
if ((Get-Content -LiteralPath (Join-Path $path '.fleet-task') -Raw).Trim() -ne $task) { throw "logon task marker at $path does not name $task" }
$definition = Get-ScheduledTask -TaskName $task
$bin = (Join-Path $path 'bin') + [IO.Path]::DirectorySeparatorChar
function Get-Listener { Get-Process -Name Runner.Listener -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($bin, [StringComparison]::OrdinalIgnoreCase) } }
@STOP@Enable-ScheduledTask -TaskName $task | Out-Null
$user = $definition.Principal.UserId.Split('\')[-1]
$session = quser.exe 2>$null | Select-String -Pattern ('^\s*>?' + [regex]::Escape($user) + '\s')
if (-not $session) { Write-Output "enabled $task; it starts when $user logs on (no session now)"; exit 0 }
Start-ScheduledTask -TaskName $task
for ($i = 0; $i -lt 30; $i++) {
  if (Get-Listener) { Write-Output "@VERB@ $task in $user's session"; exit 0 }
  Start-Sleep -Seconds 1
}
throw "$task started but no Runner.Listener appeared under $bin within 30s"
"#;

const TASK_STOP: &str = r#"Stop-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue
Get-Listener | Stop-Process -Force
Start-Sleep -Seconds 2
"#;

/// Enables and starts (or restarts) one exact runner's logon task in the
/// user's session.
pub(crate) fn task_script(path: &str, runner: &str, action: ServiceAction) -> String {
    TASK_SCRIPT
        .replace("@PATH@", &ps_quote(path))
        .replace("@TASK@", &ps_quote(&format!("runner-fleet-{runner}")))
        .replace(
            "@STOP@",
            match action {
                ServiceAction::Start => "",
                ServiceAction::Restart => TASK_STOP,
            },
        )
        .replace(
            "@VERB@",
            match action {
                ServiceAction::Start => "started",
                ServiceAction::Restart => "restarted",
            },
        )
}

const DOCTOR: &str = r#"$user = @ACCOUNT@.Split('\')[-1]
$winlogon = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon' -ErrorAction SilentlyContinue
if ($winlogon.AutoAdminLogon -eq '1' -and $winlogon.DefaultUserName -eq $user) {
  if ($winlogon.DefaultPassword) {
    Write-Output "RFD`tautologon`twarn`t$user logs on automatically, but a plaintext DefaultPassword is stored; use Sysinternals Autologon"
  } else {
    Write-Output "RFD`tautologon`tpass`t$user logs on automatically after reboot"
  }
} else {
  Write-Output "RFD`tautologon`tfail`tauto-logon is not set for $user (AutoAdminLogon=$($winlogon.AutoAdminLogon), DefaultUserName=$($winlogon.DefaultUserName)); logon-task runners do not start after reboot"
}
$session = quser.exe 2>$null | Select-String -Pattern ('^\s*>?' + [regex]::Escape($user) + '\s.*\sActive\s')
if ($session) {
  Write-Output "RFD`tsession:$user`tpass`t$user has an active interactive session"
} else {
  Write-Output "RFD`tsession:$user`twarn`t$user has no active session; logon-task runners are not running"
}
Write-Output "RFD`tservice-mode`tpass`tlogon tasks run in $user's interactive desktop session"
"#;

/// Read-only: auto-logon and an active session for `account`.
pub(crate) fn doctor_script(account: &str) -> String {
    DOCTOR.replace("@ACCOUNT@", &ps_quote(account))
}
