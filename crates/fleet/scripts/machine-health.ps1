# Read-only CIM queries. Independent failures leave individual metrics unknown.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
function Metric($name, $value) {
  $text = if ($null -eq $value) { '' } else { [Convert]::ToString($value, [Globalization.CultureInfo]::InvariantCulture) }
  Write-Output "RFM`t$name`t$text"
}
Metric os windows
try {
  $os = Get-CimInstance Win32_OperatingSystem
  Metric boot_id ($os.LastBootUpTime.ToUniversalTime().ToString('o'))
  Metric os_version "$($os.Caption) $($os.Version)"
  Metric memory_total ([uint64]$os.TotalVisibleMemorySize * 1024)
  Metric memory_available ([uint64]$os.FreePhysicalMemory * 1024)
  Metric uptime ([uint64][math]::Floor(((Get-Date) - $os.LastBootUpTime).TotalSeconds))
} catch { }
try {
  $cpus = @(Get-CimInstance Win32_Processor)
  Metric cores (($cpus | Measure-Object NumberOfLogicalProcessors -Sum).Sum)
  $loads = @($cpus | Where-Object { $null -ne $_.LoadPercentage })
  if ($loads.Count) { Metric cpu (($loads | Measure-Object LoadPercentage -Average).Average) }
} catch { }
try {
  foreach ($disk in @(Get-CimInstance Win32_LogicalDisk -Filter 'DriveType = 3')) {
    if ($disk.Size -gt 0 -and $null -ne $disk.FreeSpace) {
      Write-Output "RFM`tdisk`t$($disk.DeviceID)\`t$($disk.Size)`t$($disk.FreeSpace)"
    }
  }
} catch { }
Write-Output 'RFM_END'
