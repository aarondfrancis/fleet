# Read-only native queries; failures leave the corresponding fact unknown.
$ErrorActionPreference = 'Stop'
function Fact($name, $value) {
  if ($null -ne $value) {
    $text = ([string]$value -replace '[\t\r\n]', ' ')
    if ($text.Length -gt 1000) { $text = $text.Substring(0, 1000) }
    Write-Output "RFM`tdiag:$name`t$text"
  }
}
Fact identity ([Security.Principal.WindowsIdentity]::GetCurrent().Name)
Fact architecture $env:PROCESSOR_ARCHITECTURE
$ts = (Get-Command tailscale.exe -ErrorAction SilentlyContinue).Source
if (!$ts -and (Test-Path "$env:ProgramFiles\Tailscale\tailscale.exe")) { $ts = "$env:ProgramFiles\Tailscale\tailscale.exe" }
Fact tailscale_cli $ts
if ($ts) {
  Fact tailscale_install windows
  try {
    $prefs = (& $ts debug prefs 2>$null | Out-String | ConvertFrom-Json)
    if ($LASTEXITCODE -eq 0 -and $null -ne $prefs.RunSSH) {
      Fact tailscale_ssh ([string]$prefs.RunSSH).ToLowerInvariant()
      # ForceDaemon is omitted when false; require recognizable successful prefs.
      Fact tailscale_unattended ([string]($prefs.ForceDaemon -eq $true)).ToLowerInvariant()
    }
  } catch { }
  try {
    $status = (& $ts status --json 2>$null | Out-String | ConvertFrom-Json)
    if ($LASTEXITCODE -eq 0) {
      Fact tailscale_version $status.Version
      Fact tailscale_backend $status.BackendState
    }
  } catch { }
}
try {
  $service = Get-CimInstance Win32_Service -Filter "Name = 'Tailscale'"
  Fact tailscale_startup $service.StartMode
} catch { }
function PowerIndexes($key, $group, $setting) {
  try {
    $output = (& powercfg.exe /QH SCHEME_CURRENT $group $setting 2>$null | Out-String)
    # The final two hex values are AC then DC. Numeric settings also print
    # minimum/maximum/increment values first. Avoid localized label text.
    $indexes = [regex]::Matches($output, '(?m):\s*0x([0-9a-fA-F]+)\s*$')
    if ($LASTEXITCODE -eq 0 -and $indexes.Count -ge 2) {
      Fact "ac_$key" ([Convert]::ToUInt64($indexes[$indexes.Count - 2].Groups[1].Value, 16))
      Fact "battery_$key" ([Convert]::ToUInt64($indexes[$indexes.Count - 1].Groups[1].Value, 16))
    }
  } catch { }
}
PowerIndexes sleep SUB_SLEEP STANDBYIDLE
PowerIndexes displaysleep SUB_VIDEO VIDEOIDLE
PowerIndexes hibernate SUB_SLEEP HIBERNATEIDLE
PowerIndexes lid 4f971e89-eebd-4455-a8de-9e59040e7347 5ca83367-6e45-459f-a27b-476b1d01c936
PowerIndexes standby_network fea3413e-7e05-4911-9a71-700331f1c294 f15576e8-98b7-4186-b944-eafa664402d9
Fact complete 1
