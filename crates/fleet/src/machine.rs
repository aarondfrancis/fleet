//! Read-only machine health and runner facts collected in one SSH session.
//!
//! Uses the same transport and exact runner paths as reviewed fleet operations.
//! Only inventory hosts are inspected: a tailnet device outside the inventory is never a target.
//! Nothing is installed on a host; unavailable metrics remain unknown.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::desired::model::{Host, LocalRunner, Platform};
use crate::desired::process::SubprocessRunner;
use crate::desired::remote::{
    Connection, Programs, RemoteClient, RemoteError, parse_posix_probe, parse_windows_probe, probe_script,
};

const POSIX_HEALTH: &str = include_str!("../scripts/machine-health.sh");
const WINDOWS_HEALTH: &str = include_str!("../scripts/machine-health.ps1");
const POSIX_DIAGNOSTICS: &str = include_str!("../scripts/machine-diagnostics.sh");
const WINDOWS_DIAGNOSTICS: &str = include_str!("../scripts/machine-diagnostics.ps1");
const RUNNERS_MARKER: &str = "RFM_RUNNERS";
/// Limit for one SSH inspection, connection included.
const SSH_TIMEOUT: Duration = Duration::from_secs(30);

/// Free space below this share of a disk's capacity is low.
pub(crate) const LOW_DISK_FREE_PERCENT: f64 = 10.0;

/// `available` as a percentage of `total`.
pub(crate) fn free_percent(available: u64, total: u64) -> f64 {
    100.0 * available as f64 / total.max(1) as f64
}

/// Bytes in GiB, for display.
pub(crate) fn gib(bytes: u64) -> f64 {
    bytes as f64 / 1_073_741_824.0
}

/// The operating system read from a machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum OsFamily {
    Macos,
    Linux,
    Windows,
    Other,
}

impl OsFamily {
    /// Convert Tailscale's OS names to the dashboard's families.
    pub fn from_tailnet(os: &str) -> Self {
        match os.to_ascii_lowercase().as_str() {
            "macos" => Self::Macos,
            "linux" => Self::Linux,
            "windows" => Self::Windows,
            _ => Self::Other,
        }
    }
}

impl From<Platform> for OsFamily {
    fn from(platform: Platform) -> Self {
        match platform {
            Platform::DarwinArm64 => Self::Macos,
            Platform::WindowsArm64 | Platform::WindowsX64 => Self::Windows,
            Platform::LinuxX64 => Self::Linux,
        }
    }
}

/// Capacity of an OS or home filesystem (all fixed drives on Windows).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DiskStats {
    pub mount_point: String,
    #[ts(type = "number")]
    pub total_bytes: u64,
    #[ts(type = "number")]
    pub available_bytes: u64,
}

/// A successful SSH read, timestamped by the caller when it completes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MachineReport {
    pub os: OsFamily,
    pub os_version: Option<String>,
    pub cpu_percent: Option<f32>,
    pub load_average: Option<[f64; 3]>,
    pub logical_cores: Option<u32>,
    pub memory_total_bytes: Option<u64>,
    pub memory_available_bytes: Option<u64>,
    pub uptime_secs: Option<u64>,
    pub boot_id: Option<String>,
    pub disks: Vec<DiskStats>,
    pub runners: BTreeMap<String, LocalRunner>,
    /// Selected, non-secret facts; absent or unreadable settings remain unknown.
    pub diagnostics: BTreeMap<String, String>,
    pub warnings: Vec<String>,
}

/// Injectable read-only boundary; tests never connect to real hosts.
pub trait MachineSource: Send + Sync {
    /// Blocks until the SSH read completes or times out. Call off the async runtime.
    fn inspect(&self, host: &Host) -> Result<MachineReport, RemoteError>;
}

/// Locate the same SSH programs used by the app's runner operations.
pub fn remote_client() -> RemoteClient {
    RemoteClient::new(Arc::new(SubprocessRunner), SSH_TIMEOUT, Programs::installed())
}

impl MachineSource for RemoteClient {
    fn inspect(&self, host: &Host) -> Result<MachineReport, RemoteError> {
        let output = self.run_script(Connection::from(host), &inspection_script(host), None)?;
        parse_inspection(host, &output)
    }
}

/// The complete read-only script, exposed for diagnostics and fake transport tests.
pub fn inspection_script(host: &Host) -> String {
    let probe = probe_script(host);
    if host.is_windows() {
        format!("{WINDOWS_HEALTH}\n& {{\n{WINDOWS_DIAGNOSTICS}\n}}\nWrite-Output '{RUNNERS_MARKER}'\n{probe}")
    } else {
        format!("{POSIX_HEALTH}\n(\n{POSIX_DIAGNOSTICS}\n)\nprintf '{RUNNERS_MARKER}\\n'\n{probe}")
    }
}

/// Decode metrics and runner rows, rejecting truncated or mismatched responses.
pub fn parse_inspection(host: &Host, output: &str) -> Result<MachineReport, RemoteError> {
    let (health, runners) = output
        .split_once(&format!("{RUNNERS_MARKER}\n"))
        .or_else(|| output.split_once(&format!("{RUNNERS_MARKER}\r\n")))
        .ok_or_else(|| RemoteError("The SSH check did not return a complete machine report.".into()))?;
    let mut fields = BTreeMap::new();
    let mut disks = BTreeMap::new();
    let mut complete = false;
    for line in health.lines() {
        if line == "RFM_END" {
            complete = true;
        } else if let Some(row) = line.strip_prefix("RFM\t") {
            let Some((name, value)) = row.split_once('\t') else {
                return Err(RemoteError("Malformed machine health row.".into()));
            };
            if name == "disk" {
                let parts: Vec<_> = value.split('\t').collect();
                let [mount, total, available] = parts[..] else {
                    return Err(RemoteError("Malformed disk health row.".into()));
                };
                let total_bytes = number::<u64>("disk capacity", total)?;
                let available_bytes = number::<u64>("available disk space", available)?;
                if total_bytes == 0 || available_bytes > total_bytes {
                    return Err(RemoteError("Invalid disk capacity in the SSH report.".into()));
                }
                disks.insert(
                    mount.to_string(),
                    DiskStats {
                        mount_point: mount.into(),
                        total_bytes,
                        available_bytes,
                    },
                );
            } else {
                fields.insert(name, value);
            }
        }
    }
    if !complete {
        return Err(RemoteError("The SSH health check was interrupted.".into()));
    }
    let os = match fields.get("os").copied() {
        Some("macos") => OsFamily::Macos,
        Some("linux") => OsFamily::Linux,
        Some("windows") => OsFamily::Windows,
        Some("other") => OsFamily::Other,
        _ => {
            return Err(RemoteError(
                "The SSH check did not identify the operating system.".into(),
            ));
        }
    };
    if os != OsFamily::from(host.platform) {
        return Err(RemoteError(
            "The machine's operating system doesn't match its inventory platform. Fix the host's `platform` or `ssh` target in the inventory.".into(),
        ));
    }
    let get = |name: &str| fields.get(name).copied().filter(|v| !v.is_empty());
    let cpu_percent = get("cpu").map(|v| number::<f32>("CPU usage", v)).transpose()?;
    if cpu_percent.is_some_and(|v| !v.is_finite() || !(0.0..=100.0).contains(&v)) {
        return Err(RemoteError("Invalid CPU usage in the SSH report.".into()));
    }
    let load_average = get("load")
        .map(|v| {
            let values = v
                .split_whitespace()
                .map(|n| number::<f64>("load average", n))
                .collect::<Result<Vec<_>, _>>()?;
            let values: [f64; 3] = values
                .try_into()
                .map_err(|_| RemoteError("Invalid load average in the SSH report.".into()))?;
            if values.iter().any(|v| !v.is_finite() || *v < 0.0) {
                return Err(RemoteError("Invalid load average in the SSH report.".into()));
            }
            Ok(values)
        })
        .transpose()?;
    let memory_total_bytes = get("memory_total")
        .map(|v| number::<u64>("total memory", v))
        .transpose()?;
    let memory_available_bytes = get("memory_available")
        .map(|v| number::<u64>("available memory", v))
        .transpose()?;
    if let (Some(total), Some(available)) = (memory_total_bytes, memory_available_bytes)
        && (total == 0 || available > total)
    {
        return Err(RemoteError("Invalid memory capacity in the SSH report.".into()));
    }
    let expected: Vec<&str> = host.runner_names().collect();
    let runners = if expected.is_empty() && matches!(runners.trim(), "" | "[]") {
        BTreeMap::new()
    } else if host.is_windows() {
        parse_windows_probe(runners)?
    } else {
        parse_posix_probe(runners)?
    };
    if expected.len() != runners.len() || expected.iter().any(|name| !runners.contains_key(*name)) {
        return Err(RemoteError(
            "The SSH check's runners don't match the ones the inventory declares for this machine. Check the runner names in the inventory, then check the machine again.".into(),
        ));
    }
    Ok(MachineReport {
        os,
        os_version: get("os_version").map(str::to_string),
        cpu_percent,
        load_average,
        logical_cores: get("cores").map(|v| number("CPU count", v)).transpose()?,
        memory_total_bytes,
        memory_available_bytes,
        uptime_secs: get("uptime").map(|v| number("uptime", v)).transpose()?,
        boot_id: get("boot_id").map(str::to_string),
        disks: disks.into_values().collect(),
        runners,
        diagnostics: if get("diag:complete") == Some("1") {
            fields
                .iter()
                .filter_map(|(key, value)| {
                    key.strip_prefix("diag:")
                        .filter(|k| *k != "complete")
                        .map(|key| (key.to_string(), value.to_string()))
                })
                .collect()
        } else {
            BTreeMap::new()
        },
        warnings: if cpu_percent.is_none() || memory_total_bytes.is_none() || memory_available_bytes.is_none() {
            vec!["Some health metrics could not be read over SSH.".into()]
        } else {
            Vec::new()
        },
    })
}

fn number<T: std::str::FromStr>(name: &str, value: &str) -> Result<T, RemoteError> {
    value
        .parse()
        .map_err(|_| RemoteError(format!("Invalid {name} in the SSH report.")))
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
