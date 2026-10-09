$ErrorActionPreference = "Stop"
foreach ($required in @("RF_ROOT", "RF_NAME", "RF_TOKEN")) {
    if (-not [Environment]::GetEnvironmentVariable($required)) { throw "Missing $required" }
}

$root = [IO.Path]::GetFullPath($env:RF_ROOT)
$runnerFile = Join-Path $root ".runner"
if (-not (Test-Path -LiteralPath $runnerFile)) { throw "No configured runner at exact path $root" }
$configured = Get-Content -LiteralPath $runnerFile -Raw | ConvertFrom-Json
if ($configured.agentName -ne $env:RF_NAME) {
    throw "Runner name at exact path is '$($configured.agentName)', expected '$env:RF_NAME'"
}

Set-Location -LiteralPath $root
$taskFile = Join-Path $root ".fleet-task"
if (Test-Path -LiteralPath $taskFile) {
    # windows-logon runner: stop and remove its logon task and listener first.
    $taskName = (Get-Content -LiteralPath $taskFile -Raw).Trim()
    if ($taskName -ne "runner-fleet-$env:RF_NAME") { throw "Unexpected logon task '$taskName' for $env:RF_NAME" }
    if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
        Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
    }
    $bin = (Join-Path $root "bin") + [IO.Path]::DirectorySeparatorChar
    Get-Process -Name Runner.Listener, Runner.Worker -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($bin, [StringComparison]::OrdinalIgnoreCase) } |
        Stop-Process -Force
    Remove-Item -LiteralPath $taskFile -Force
}
& .\config.cmd remove --token $env:RF_TOKEN
if ($LASTEXITCODE -ne 0) { throw "config.cmd remove failed with exit code $LASTEXITCODE" }
$env:RF_TOKEN = $null
Write-Output "Retired $env:RF_NAME; preserved exact directory $root"
