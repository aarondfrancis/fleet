# Runs on a Windows worker over PowerShell stdin. RF_* values are process-only.
$ErrorActionPreference = "Stop"

foreach ($required in @("RF_ROOT", "RF_NAME", "RF_ORG_URL", "RF_RUNNER_GROUP", "RF_LABELS", "RF_TOKEN", "RF_ASSET_URL", "RF_ASSET_NAME")) {
    if (-not [Environment]::GetEnvironmentVariable($required)) { throw "Missing $required" }
}
if ($env:RF_SERVICE_MODE -notin @("windows-service", "windows-logon")) { throw "Unsupported service mode: $env:RF_SERVICE_MODE" }
$logonMode = $env:RF_SERVICE_MODE -eq "windows-logon"
if ($logonMode -and -not $env:RF_SERVICE_ACCOUNT) { throw "windows-logon requires RF_SERVICE_ACCOUNT" }
# windows-logon: a Scheduled Task starts run.cmd when this user logs on, so the
# runner lives in that user's interactive desktop session instead of Session 0.
$taskName = "runner-fleet-$env:RF_NAME"
$logonUser = if ($env:RF_SERVICE_ACCOUNT) { $env:RF_SERVICE_ACCOUNT -replace '^\.\\', "$env:COMPUTERNAME\" } else { $null }

function Register-RunnerLogonTask([string]$RunnerRoot) {
    # conhost --headless leaves no console window to close by accident on the
    # GUI desktop. Task Scheduler's restart-on-failure only covers launch
    # failures, and a repetition on the logon trigger only starts after a
    # logon, so a separate time trigger repeats every 5 minutes indefinitely.
    # MultipleInstances IgnoreNew skips it while the runner is running, and the
    # Interactive principal only launches while the account is logged on.
    $action = New-ScheduledTaskAction -Execute "$env:WINDIR\System32\conhost.exe" `
        -Argument ('--headless "' + (Join-Path $RunnerRoot "run.cmd") + '"') -WorkingDirectory $RunnerRoot
    $logon = New-ScheduledTaskTrigger -AtLogOn -User $logonUser
    $watchdog = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) -RepetitionInterval (New-TimeSpan -Minutes 5)
    $trigger = @($logon, $watchdog)
    $principal = New-ScheduledTaskPrincipal -UserId $logonUser -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 999 `
        -RestartInterval (New-TimeSpan -Minutes 1) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -StartWhenAvailable -MultipleInstances IgnoreNew
    Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Principal $principal `
        -Settings $settings -Description "GitHub Actions runner $env:RF_NAME (runner-fleet)" -Force | Out-Null
}
function Find-RunnerService([string]$RunnerRoot) {
    $needle = $RunnerRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    Get-CimInstance Win32_Service | Where-Object {
        $_.PathName.IndexOf($needle, [StringComparison]::OrdinalIgnoreCase) -ge 0 -and $_.Name -like "actions.runner.*"
    } | Select-Object -First 1
}
$assetUri = [Uri]$env:RF_ASSET_URL
if ($assetUri.Scheme -ne "https" -or $assetUri.Host -ne "github.com" -or -not $assetUri.AbsolutePath.StartsWith("/actions/runner/releases/download/")) {
    throw "Refusing unexpected runner asset URL: $env:RF_ASSET_URL"
}

$allowedRoot = [IO.Path]::GetFullPath("C:\gh-runners")
$root = [IO.Path]::GetFullPath($env:RF_ROOT)
if (-not $root.StartsWith($allowedRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing root outside C:\gh-runners: $root"
}

$runnerFile = Join-Path $root ".runner"
if (Test-Path -LiteralPath $runnerFile) {
    $configured = Get-Content -LiteralPath $runnerFile -Raw | ConvertFrom-Json
    if ($configured.agentName -ne $env:RF_NAME) {
        throw "Configured runner at exact path is '$($configured.agentName)', expected '$env:RF_NAME'"
    }
    if ($logonMode) {
        $existing = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        if (-not $existing) { throw "Runner is configured but logon task $taskName does not exist" }
        # Reconcile the task definition; keep its enabled/disabled gate as it was.
        $wasDisabled = $existing.State -eq "Disabled"
        Register-RunnerLogonTask $root
        if ($wasDisabled) { Disable-ScheduledTask -TaskName $taskName | Out-Null }
        Write-Output "Runner $env:RF_NAME already configured; reconciled logon task $taskName"
        exit 0
    }
    $service = Find-RunnerService $root
    if (-not $service) { throw "Runner is configured but no exact-path Windows service exists" }
    Start-Service -Name $service.Name
    Write-Output "Runner $env:RF_NAME already configured; ensured service is started"
    exit 0
}

if ((Test-Path -LiteralPath $root) -and (Get-ChildItem -LiteralPath $root -Force | Select-Object -First 1)) {
    throw "Refusing non-empty unconfigured directory: $root"
}

New-Item -ItemType Directory -Force -Path $root | Out-Null
$archive = Join-Path ([IO.Path]::GetTempPath()) ("runner-fleet-" + [guid]::NewGuid() + ".zip")
try {
    Write-Output "Downloading $env:RF_ASSET_NAME"
    Invoke-WebRequest -Uri $env:RF_ASSET_URL -OutFile $archive
    if ($env:RF_CHECKSUM) {
        $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $env:RF_CHECKSUM.ToLowerInvariant()) {
            throw "SHA-256 mismatch for $env:RF_ASSET_NAME"
        }
        Write-Output "Verified GitHub SHA-256 digest"
    } else {
        Write-Warning "GitHub release metadata did not publish an asset digest"
    }
    Expand-Archive -LiteralPath $archive -DestinationPath $root
} finally {
    Remove-Item -LiteralPath $archive -Force -ErrorAction SilentlyContinue
}

Set-Location -LiteralPath $root
$arguments = @(
    "--unattended",
    "--url", $env:RF_ORG_URL,
    "--token", $env:RF_TOKEN,
    "--name", $env:RF_NAME,
    "--runnergroup", $env:RF_RUNNER_GROUP,
    "--labels", $env:RF_LABELS
)
if (-not $logonMode) { $arguments += "--runasservice" }
if ($env:RF_SERVICE_ACCOUNT -and -not $logonMode) {
    if (-not $env:RF_SERVICE_PASSWORD) {
        throw "The service account password is required for service_account"
    }
    $arguments += @(
        "--windowslogonaccount", $env:RF_SERVICE_ACCOUNT,
        "--windowslogonpassword", $env:RF_SERVICE_PASSWORD
    )
}
& .\config.cmd @arguments
if ($LASTEXITCODE -ne 0) { throw "config.cmd failed with exit code $LASTEXITCODE" }
$env:RF_TOKEN = $null
$env:RF_SERVICE_PASSWORD = $null
if ($logonMode) {
    & icacls.exe $root /grant "${logonUser}:(OI)(CI)M" /T /C /Q | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "could not grant $logonUser access to $root" }
    Register-RunnerLogonTask $root
    # Held disabled until the controller verifies trusted-group membership and labels.
    Disable-ScheduledTask -TaskName $taskName | Out-Null
    Set-Content -LiteralPath (Join-Path $root ".fleet-task") -Value $taskName -NoNewline
    Write-Output "Installed $env:RF_NAME at $root; logon task $taskName for $logonUser is disabled until trusted-group assignment"
    exit 0
}
$service = Find-RunnerService $root
if (-not $service) { throw "Runner configured but exact-path Windows service was not found" }
if ($service.State -ne "Stopped") { Stop-Service -Name $service.Name }
Write-Output "Installed $env:RF_NAME at $root; service is stopped until trusted-group assignment"
