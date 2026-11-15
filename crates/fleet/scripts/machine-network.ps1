# Runs on a Windows worker over PowerShell stdin. RF_* values are process-only.
# Sets the DNS servers on every physical adapter and installs or removes the
# network watchdog (a SYSTEM scheduled task). Idempotent.
$ErrorActionPreference = "Stop"

$taskName = "runner-fleet-network-watchdog"
$dir = Join-Path $env:ProgramData "runner-fleet"
$watchdogPath = Join-Path $dir "network-watchdog.ps1"

$dns = @(($env:RF_DNS -split ",") | ForEach-Object { $_.Trim() } | Where-Object { $_ })
foreach ($server in $dns) {
    $parsed = $null
    if (-not [Net.IPAddress]::TryParse($server, [ref]$parsed)) { throw "Refusing DNS server that is not an IP address: $server" }
}
if ($dns.Count -gt 0) {
    foreach ($adapter in @(Get-NetAdapter -Physical)) {
        if (-not (Get-NetIPInterface -InterfaceIndex $adapter.ifIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue)) { continue }
        $v4 = @($dns | Where-Object { ([Net.IPAddress]$_).AddressFamily -eq [Net.Sockets.AddressFamily]::InterNetwork })
        $v6 = @($dns | Where-Object { ([Net.IPAddress]$_).AddressFamily -eq [Net.Sockets.AddressFamily]::InterNetworkV6 })
        $current4 = @((Get-DnsClientServerAddress -InterfaceIndex $adapter.ifIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue).ServerAddresses)
        $current6 = @((Get-DnsClientServerAddress -InterfaceIndex $adapter.ifIndex -AddressFamily IPv6 -ErrorAction SilentlyContinue).ServerAddresses)
        if (($current4 -join ",") -ne ($v4 -join ",") -or ($v6.Count -gt 0 -and ($current6 -join ",") -ne ($v6 -join ","))) {
            Set-DnsClientServerAddress -InterfaceIndex $adapter.ifIndex -ServerAddresses $dns
            Write-Output "Set DNS on $($adapter.Name) to $($dns -join ', ') (was $(if ($current4) { $current4 -join ', ' } else { 'automatic' }))"
        }
    }
    Clear-DnsClientCache
}

if ($env:RF_WATCHDOG -eq "true") {
    if (-not $env:RF_WATCHDOG_SOURCE) { throw "Missing RF_WATCHDOG_SOURCE" }
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $existing = if (Test-Path -LiteralPath $watchdogPath) { Get-Content -LiteralPath $watchdogPath -Raw } else { $null }
    if ($existing -ne $env:RF_WATCHDOG_SOURCE) {
        Set-Content -LiteralPath $watchdogPath -Value $env:RF_WATCHDOG_SOURCE -NoNewline
        Write-Output "Wrote $watchdogPath"
    }
    $action = New-ScheduledTaskAction -Execute "$env:WINDIR\System32\WindowsPowerShell\v1.0\powershell.exe" `
        -Argument ('-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' + $watchdogPath + '"')
    $startup = New-ScheduledTaskTrigger -AtStartup
    $every = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) -RepetitionInterval (New-TimeSpan -Minutes 1)
    $principal = New-ScheduledTaskPrincipal -UserId "SYSTEM" -LogonType ServiceAccount -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Minutes 5) -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries -StartWhenAvailable -MultipleInstances IgnoreNew
    $task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    Register-ScheduledTask -TaskName $taskName -Action $action -Trigger @($startup, $every) -Principal $principal `
        -Settings $settings -Description "Reconnects the network after five minutes without internet (runner-fleet)" -Force | Out-Null
    Write-Output $(if ($task) { "Updated scheduled task $taskName" } else { "Installed scheduled task $taskName" })
} elseif (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
    Write-Output "Removed scheduled task $taskName"
}
