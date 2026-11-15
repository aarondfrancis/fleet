# runner-fleet network watchdog v1
# Installed by machine-network.ps1 and run every minute as SYSTEM. When the
# internet has been unreachable for five minutes it reconnects the network:
# the machine can stay associated with its access point while no traffic
# passes, and Windows never reconnects on its own. It changes nothing while
# the internet is reachable.
$ErrorActionPreference = 'Continue'
$dir = Join-Path $env:ProgramData 'runner-fleet'
$statePath = Join-Path $dir 'network-watchdog.json'
$logPath = Join-Path $dir 'network-watchdog.log'
$failAfter = [TimeSpan]::FromMinutes(5)
$cooldown = [TimeSpan]::FromMinutes(10)

function Write-Log([string]$Message) {
    $line = '{0} {1}' -f (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'), $Message
    Add-Content -LiteralPath $logPath -Value $line
    if ((Get-Item -LiteralPath $logPath).Length -gt 256KB) {
        $tail = Get-Content -LiteralPath $logPath -Tail 500
        Set-Content -LiteralPath $logPath -Value $tail
    }
}

function Test-Tcp([string]$Address, [int]$Port) {
    $client = New-Object Net.Sockets.TcpClient
    try {
        return $client.ConnectAsync($Address, $Port).Wait(3000) -and $client.Connected
    } catch {
        return $false
    } finally {
        $client.Dispose()
    }
}

# Online means a public address answers on 443 and a public name resolves.
function Test-Online {
    $reachable = $false
    foreach ($address in @('1.1.1.1', '8.8.8.8', '9.9.9.9')) {
        if (Test-Tcp $address 443) { $reachable = $true; break }
    }
    if (-not $reachable) { return $false }
    try {
        Resolve-DnsName -Name 'github.com' -Type A -DnsOnly -QuickTimeout -ErrorAction Stop | Out-Null
        return $true
    } catch {
        return $false
    }
}

function Read-State {
    if (Test-Path -LiteralPath $statePath) {
        try { return Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json } catch { }
    }
    return [pscustomobject]@{ since = $null; lastRepair = $null; repairs = 0 }
}

function ConvertTo-Utc($Value) {
    if ($Value -is [datetime]) { return $Value.ToUniversalTime() }
    return [datetime]::Parse([string]$Value, [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::RoundtripKind).ToUniversalTime()
}

function Save-State($State) {
    $State | ConvertTo-Json -Compress | Set-Content -LiteralPath $statePath
}

function Get-WlanProfile([string]$Interface) {
    $current = $null
    foreach ($line in (netsh.exe wlan show interfaces)) {
        if ($line -match '^\s*Name\s*:\s*(.+?)\s*$') { $current = $Matches[1] }
        elseif ($current -eq $Interface -and $line -match '^\s*Profile\s*:\s*(.+?)\s*$') { return $Matches[1] }
    }
    return $null
}

# Reconnect Wi-Fi to the same profile; restart the adapter on the third try
# in one outage, or when it is wired or has no profile to reconnect to.
function Repair-Network([int]$Attempt) {
    Clear-DnsClientCache -ErrorAction SilentlyContinue
    $adapters = @(Get-NetAdapter -Physical -ErrorAction SilentlyContinue | Where-Object { $_.Status -ne 'Disabled' })
    if ($adapters.Count -eq 0) {
        Write-Log 'no physical network adapter to repair'
        return
    }
    foreach ($adapter in $adapters) {
        $wifi = $adapter.PhysicalMediaType -match '802\.11' -or $adapter.InterfaceDescription -match 'Wi-?Fi|Wireless'
        $wlanProfile = if ($wifi) { Get-WlanProfile $adapter.Name } else { $null }
        if ($wifi -and $wlanProfile -and $Attempt -lt 3) {
            Write-Log "reconnecting Wi-Fi '$($adapter.Name)' to profile '$wlanProfile' (attempt $Attempt)"
            netsh.exe wlan disconnect "interface=$($adapter.Name)" | Out-Null
            Start-Sleep -Seconds 3
            netsh.exe wlan connect "name=$wlanProfile" "interface=$($adapter.Name)" | Out-Null
        } elseif ($adapter.Status -eq 'Up' -or $wifi) {
            Write-Log "restarting adapter '$($adapter.Name)' (attempt $Attempt)"
            Restart-NetAdapter -Name $adapter.Name -Confirm:$false -ErrorAction SilentlyContinue
        }
    }
}

New-Item -ItemType Directory -Force -Path $dir | Out-Null
$now = (Get-Date).ToUniversalTime()
$state = Read-State
if (Test-Online) {
    if ($state.since) {
        Write-Log "internet reachable again; unreachable since $($state.since), $($state.repairs) repair(s)"
        Save-State ([pscustomobject]@{ since = $null; lastRepair = $null; repairs = 0 })
    }
    exit 0
}
if (-not $state.since) {
    Write-Log 'internet unreachable'
    $state.since = $now.ToString('o')
    Save-State $state
    exit 0
}
if ($now - (ConvertTo-Utc $state.since) -lt $failAfter) { exit 0 }
if ($state.lastRepair -and $now - (ConvertTo-Utc $state.lastRepair) -lt $cooldown) { exit 0 }
$state.repairs = [int]$state.repairs + 1
$state.lastRepair = $now.ToString('o')
Save-State $state
Repair-Network $state.repairs
