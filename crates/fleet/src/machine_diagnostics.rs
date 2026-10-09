//! Platform-aware interpretation of read-only machine configuration facts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::desired::model::{Host, ServiceMode, is_active};
use crate::machine::{LOW_DISK_FREE_PERCENT, MachineReport, OsFamily, free_percent, gib};
use DiagnosticGroup::{Network, Power, Runners, System};
use DiagnosticStatus::{Info, Pass, Unknown, Warn};

/// A finding distinguishes confirmed problems from information we could not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticStatus {
    Pass,
    Warn,
    Info,
    Unknown,
}

/// Groups keep host configuration separate from runner-specific requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticGroup {
    Network,
    Power,
    System,
    Runners,
}

/// A compact finding with the evidence and limits needed to interpret it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MachineDiagnostic {
    pub id: String,
    pub group: DiagnosticGroup,
    pub title: String,
    pub status: DiagnosticStatus,
    pub value: String,
    pub detail: String,
}

/// Diagnostics share the SSH report's timestamp and freshness, including on failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MachineDiagnostics {
    pub as_of: DateTime<Utc>,
    pub stale: bool,
    pub checks: Vec<MachineDiagnostic>,
}

/// Interpret collected facts without making any network calls or changing a host.
pub(crate) fn evaluate(report: &MachineReport, host: Option<&Host>) -> Vec<MachineDiagnostic> {
    let mut checks = Checks {
        report,
        checks: Vec::new(),
    };
    checks.tailscale();
    match report.os {
        OsFamily::Macos | OsFamily::Windows => checks.power_mac_windows(),
        OsFamily::Linux => checks.power_linux(),
        OsFamily::Other => checks.add(
            "power-sleep",
            Power,
            "System sleep",
            Unknown,
            "Unsupported platform",
            "No native power-policy collector is available for this operating system.".into(),
        ),
    }
    checks.system();
    if let Some(host) = host.filter(|h| h.runners.iter().any(|r| !r.decommission)) {
        checks.runners(host);
    }
    checks.checks
}

/// Findings for one report, in display order.
struct Checks<'a> {
    report: &'a MachineReport,
    checks: Vec<MachineDiagnostic>,
}

impl<'a> Checks<'a> {
    /// A collected fact; empty values count as unread.
    fn get(&self, key: &str) -> Option<&'a str> {
        self.report
            .diagnostics
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    fn add(
        &mut self,
        id: &str,
        group: DiagnosticGroup,
        title: &str,
        status: DiagnosticStatus,
        value: &str,
        detail: String,
    ) {
        self.checks.push(MachineDiagnostic {
            id: id.into(),
            group,
            title: title.into(),
            status,
            value: value.into(),
            detail,
        });
    }

    /// `Info` with the value when the fact was read, otherwise `Unknown`.
    fn add_fact(&mut self, id: &str, group: DiagnosticGroup, title: &str, value: Option<&str>, detail: &str) {
        let status = if value.is_some() { Info } else { Unknown };
        self.add(id, group, title, status, value.unwrap_or("Unknown"), detail.into());
    }

    fn tailscale(&mut self) {
        let installation = self.get("tailscale_install");
        let variant = match installation {
            Some("cli-daemon") => Some("CLI + daemon"),
            Some("cli") => Some("CLI found; daemon unconfirmed"),
            Some("mac-app-store") => Some("GUI · Mac App Store"),
            Some("mac-standalone") => Some("GUI · standalone app"),
            Some("windows") => Some("Windows app + service"),
            _ => None,
        };
        self.add(
            "tailscale-install",
            Network,
            "Tailscale installation",
            if variant.is_some() { Info } else { Unknown },
            variant.unwrap_or("Unknown"),
            self.get("tailscale_cli").map_or_else(
                || "The CLI path could not be read with this SSH account.".into(),
                |p| format!("CLI: {p}. GUI installations can also include a CLI."),
            ),
        );
        self.add_fact(
            "tailscale-version",
            Network,
            "Tailscale version",
            self.get("tailscale_version"),
            "Version reported by the local Tailscale daemon.",
        );
        let backend = self.get("tailscale_backend");
        self.add(
            "tailscale-state",
            Network,
            "Tailscale connection",
            match backend {
                Some("Running") => Pass,
                Some(_) => Warn,
                None => Unknown,
            },
            backend.unwrap_or("Unknown"),
            "Local daemon state from tailscale status.".into(),
        );
        let os = self.report.os;
        let unsupported = os == OsFamily::Windows || matches!(installation, Some("mac-app-store" | "mac-standalone"));
        let (status, value, detail) = match self.get("tailscale_ssh") {
            Some("true") => (
                Pass,
                "Enabled",
                "The local RunSSH preference is enabled. Tailnet access rules still control who can connect.",
            ),
            _ if unsupported => (
                Info,
                "Not supported",
                "This Tailscale variant does not host Tailscale SSH. Use the operating system's SSH server over the tailnet.",
            ),
            Some("false") => (
                Info,
                "Disabled",
                "The local RunSSH preference is disabled. Ordinary SSH over Tailscale can still work.",
            ),
            _ => (
                Unknown,
                "Unknown",
                "The local RunSSH preference could not be read. A configured SSH transport is not proof that Tailscale SSH is enabled.",
            ),
        };
        self.add("tailscale-ssh", Network, "Tailscale SSH", status, value, detail.into());
        if os == OsFamily::Windows {
            let (status, value) = match self.get("tailscale_unattended") {
                Some("true") => (Pass, "Enabled"),
                Some("false") => (Warn, "Disabled"),
                _ => (Unknown, "Unknown"),
            };
            self.add(
                "tailscale-unattended",
                Network,
                "Tailscale unattended mode",
                status,
                value,
                "The ForceDaemon preference controls whether Tailscale stays connected after logout.".into(),
            );
        }
        if matches!(os, OsFamily::Linux | OsFamily::Windows) {
            let startup = self.get("tailscale_startup");
            let (status, value) = match startup {
                Some("enabled" | "Auto") => (Pass, "Starts at boot"),
                Some("disabled" | "masked" | "Manual" | "Disabled") => (Warn, "Not automatic"),
                Some("enabled-runtime") => (Warn, "Enabled until reboot"),
                _ => (Unknown, "Unknown"),
            };
            self.add(
                "tailscale-startup",
                Network,
                "Tailscale startup",
                status,
                value,
                format!("Native service startup: {}.", startup.unwrap_or("not readable")),
            );
        }
    }

    fn power_mac_windows(&mut self) {
        let mac = self.report.os == OsFamily::Macos;
        let unit = if mac { "minutes" } else { "seconds" };
        for (key, title, risk) in [
            ("sleep", "Sleep on AC power", true),
            ("displaysleep", "Display off on AC power", false),
        ] {
            let number = self.get(&format!("ac_{key}")).and_then(|v| v.parse::<u64>().ok());
            let (status, value) = match number {
                Some(0) => (if risk { Pass } else { Info }, "Never".into()),
                Some(n) => (if risk { Warn } else { Info }, format!("After {n} {unit}")),
                None => (Unknown, "Unknown".into()),
            };
            let battery = self
                .get(&format!("battery_{key}"))
                .and_then(|v| v.parse::<u64>().ok())
                .map(|n| {
                    if n == 0 {
                        " Battery: never.".into()
                    } else {
                        format!(" Battery: after {n} {unit}.")
                    }
                })
                .unwrap_or_default();
            let detail = if risk {
                "Automatic sleep interrupts remote access and jobs. This timeout does not cover lid actions or every standby policy."
            } else {
                "Display sleep is separate from system sleep; Modern Standby devices may also change connectivity when the display turns off."
            };
            self.add(
                &format!("power-{key}"),
                Power,
                title,
                status,
                &value,
                format!("{detail}{battery}"),
            );
        }
        if mac {
            self.power_mac();
        } else {
            self.power_windows();
        }
    }

    fn power_mac(&mut self) {
        let (status, value) = match self.get("ac_autorestart") {
            Some("1") => (Pass, "Enabled"),
            Some("0") => (Warn, "Disabled"),
            _ => (Unknown, "Unknown"),
        };
        self.add(
            "power-recovery",
            Power,
            "Restart after power failure",
            status,
            value,
            "AC autorestart setting from pmset. Availability depends on the Mac model.".into(),
        );
        let (status, value) = match self.get("ac_womp") {
            Some("1") => (Info, "Enabled"),
            Some("0") => (Info, "Disabled"),
            _ => (Unknown, "Unknown"),
        };
        self.add(
            "power-wake",
            Power,
            "Wake for network access",
            status,
            value,
            "Wake-on-network setting from pmset. This does not guarantee wake through Tailscale.".into(),
        );
    }

    fn power_windows(&mut self) {
        let seconds = self.get("ac_hibernate").and_then(|v| v.parse::<u64>().ok());
        let value = seconds.map(|n| {
            if n == 0 {
                "Never".into()
            } else {
                format!("After {n} seconds")
            }
        });
        self.add(
            "power-hibernate",
            Power,
            "Hibernate on AC power",
            match seconds {
                Some(0) => Pass,
                Some(_) => Warn,
                None => Unknown,
            },
            value.as_deref().unwrap_or("Unknown"),
            "Active power plan hibernation timeout.".into(),
        );
        let lid = match self.get("ac_lid") {
            Some("0") => Some("Do nothing"),
            Some("1") => Some("Sleep"),
            Some("2") => Some("Hibernate"),
            Some("3") => Some("Shut down"),
            _ => None,
        };
        self.add(
            "power-lid",
            Power,
            "Lid close on AC power",
            if lid.is_some() { Info } else { Unknown },
            lid.unwrap_or("Unknown / unavailable"),
            "Active power plan policy, applicable only to devices with a lid.".into(),
        );
        let network = match self.get("ac_standby_network") {
            Some("0") => Some("Disabled"),
            Some("1") => Some("Enabled"),
            Some("2") => Some("Managed by Windows"),
            _ => None,
        };
        self.add(
            "power-standby-network",
            Power,
            "Network during standby",
            if network.is_some() { Info } else { Unknown },
            network.unwrap_or("Unknown / unavailable"),
            "AC power plan preference. Hardware support and group policy may override it; this does not prove connectivity during Modern Standby.".into(),
        );
    }

    fn power_linux(&mut self) {
        let targets = [
            "sleep",
            "suspend",
            "hibernate",
            "hybrid-sleep",
            "suspend-then-hibernate",
        ];
        let target = |t: &str| self.get(&format!("target_{t}"));
        let blocked = target("sleep") == Some("masked") || targets[1..].iter().all(|t| target(t) == Some("masked"));
        let detail = targets
            .map(|t| format!("{t}: {}", target(t).unwrap_or("unknown")))
            .join("; ");
        self.add(
            "power-sleep",
            Power,
            "System sleep protection",
            if blocked { Pass } else { Unknown },
            if blocked {
                "Blocked by systemd"
            } else {
                "Not confirmed disabled"
            },
            format!(
                "{detail}. Unmasked targets do not prove automatic sleep is enabled; desktop and logind policies can differ."
            ),
        );
        let idle = self.get("idle_action");
        let status = match idle {
            Some("ignore") => Pass,
            Some("suspend" | "hibernate" | "hybrid-sleep" | "suspend-then-hibernate" | "poweroff") if !blocked => Warn,
            Some(_) => Info,
            None => Unknown,
        };
        self.add(
            "power-idle",
            Power,
            "Logind idle action",
            status,
            idle.unwrap_or("Unknown"),
            "Configuration on disk, including drop-ins. A desktop environment can have a separate policy; changes may need a service reload.".into(),
        );
    }

    fn system(&mut self) {
        self.add_fact(
            "ssh-identity",
            System,
            "SSH account",
            self.get("identity"),
            "Account used for this read-only inspection.",
        );
        self.add_fact(
            "architecture",
            System,
            "Architecture",
            self.get("architecture"),
            "Architecture reported by the operating system.",
        );
        let disks = &self.report.disks;
        if disks.is_empty() {
            self.add(
                "disk-space",
                System,
                "Disk space",
                Unknown,
                "Unknown",
                "Filesystem capacity could not be read.".into(),
            );
            return;
        }
        let low = disks
            .iter()
            .any(|d| free_percent(d.available_bytes, d.total_bytes) < LOW_DISK_FREE_PERCENT);
        let detail = disks
            .iter()
            .map(|d| {
                format!(
                    "{}: {:.1} GiB free of {:.1} GiB",
                    d.mount_point,
                    gib(d.available_bytes),
                    gib(d.total_bytes)
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let value = if low {
            format!("Less than {LOW_DISK_FREE_PERCENT}% free")
        } else {
            format!("At least {LOW_DISK_FREE_PERCENT}% free")
        };
        self.add(
            "disk-space",
            System,
            "Disk space",
            if low { Warn } else { Pass },
            &value,
            detail,
        );
    }

    fn runners(&mut self, host: &Host) {
        let (value, detail) = match host.service_mode {
            ServiceMode::Launchagent => (
                "User login required",
                format!(
                    "Configured as per-user LaunchAgents. Console user: {}. A reboot may require login before runners start.",
                    self.get("console_user").unwrap_or("unknown")
                ),
            ),
            ServiceMode::WindowsLogon => (
                "User login required",
                "Configured as logon tasks. Use Runner checks below to inspect the interactive session and auto-logon configuration.".into(),
            ),
            ServiceMode::WindowsService | ServiceMode::Systemd => (
                "System services",
                "The inventory configures runners as native system services. Service state is checked separately below.".into(),
            ),
        };
        self.add("runner-startup", Runners, "Runner startup", Info, value, detail);
        for runner in host.runners.iter().filter(|r| !r.decommission) {
            let local = self.report.runners.get(&runner.name);
            let (status, value) = match local {
                Some(r) if !r.configured => (Warn, "Not configured"),
                Some(r) if is_active(&r.service_state) => (Pass, "Running"),
                Some(r) if r.service_state == "unknown" => (Unknown, "Unknown"),
                Some(_) => (Warn, "Not running"),
                None => (Unknown, "Unknown"),
            };
            self.add(
                &format!("runner:{}", runner.name),
                Runners,
                &runner.name,
                status,
                value,
                local.map_or_else(
                    || "No local runner facts were returned.".into(),
                    |r| {
                        format!(
                            "Service: {} · version: {} · {}",
                            r.service_state,
                            r.version.as_deref().unwrap_or("unknown"),
                            r.path
                        )
                    },
                ),
            );
        }
    }
}

#[cfg(test)]
#[path = "machine_diagnostics_tests.rs"]
mod tests;
