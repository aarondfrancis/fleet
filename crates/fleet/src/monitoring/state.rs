//! Per-machine transitions, evaluated only from fresh observations.

use super::{FleetAlertKind, FleetEventKind as Kind, FleetMachineAlert};
use crate::desired::model::is_active;
use crate::machine::{LOW_DISK_FREE_PERCENT, free_percent, gib};
use crate::model::{HealthView, MachineConnection, MachineView};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const OFFLINE_DELAY: Duration = Duration::minutes(3);
const DISK_DELAY: Duration = Duration::seconds(60);
/// A low-disk alert clears only this far above the threshold, so readings near it don't flap.
const DISK_RECOVERY_MARGIN_PERCENT: f64 = 2.0;

#[derive(Debug)]
pub(super) struct Change {
    pub kind: Kind,
    pub detail: String,
    pub notify: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct MachineState {
    pub target: String,
    pub name: String,
    pub last_seen: Option<DateTime<Utc>>,
    pub network_at: Option<DateTime<Utc>>,
    pub online: Option<bool>,
    pub offline_since: Option<DateTime<Utc>>,
    pub offline_alert: Option<DateTime<Utc>>,
    pub offline_announced: bool,
    pub health_at: Option<DateTime<Utc>>,
    pub boot_id: Option<String>,
    pub ssh_available: Option<bool>,
    pub services: BTreeMap<String, String>,
    pub disks: BTreeMap<String, DiskState>,
    pub maintenance_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct DiskState {
    pub pending_since: Option<DateTime<Utc>>,
    pub active_since: Option<DateTime<Utc>>,
    pub announced: bool,
    pub at: Option<DateTime<Utc>>,
    pub free: u64,
    pub total: u64,
}

pub(super) fn target(m: &MachineView) -> String {
    m.desired
        .as_ref()
        .and_then(|d| d.host.ssh_target.clone())
        .unwrap_or_else(|| m.host.clone())
}

fn change(kind: Kind, detail: impl Into<String>, notify: bool) -> Change {
    Change {
        kind,
        detail: detail.into(),
        notify,
    }
}

impl MachineState {
    pub fn observe(&mut self, m: &MachineView, network_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Vec<Change> {
        let mut changes = Vec::new();
        self.name.clone_from(&m.display_name);
        self.last_seen = Some(now);
        if self.maintenance_until.is_some_and(|t| t <= now) {
            self.maintenance_until = None;
            changes.push(change(
                Kind::MaintenanceEnded,
                "The one-hour maintenance pause ended. Alerts are active again.",
                false,
            ));
        }
        let muted = self.maintenance_until.is_some_and(|t| t > now);
        self.observe_network(m, network_at, muted, &mut changes);
        self.observe_ssh(&m.connection, &mut changes);
        if let Some(health) = m
            .health
            .as_ref()
            .filter(|h| !h.stale && self.health_at.is_none_or(|at| h.as_of > at))
        {
            self.health_at = Some(health.as_of);
            self.observe_boot(health, &mut changes);
            self.observe_services(m, &mut changes);
            self.observe_disks(health, muted, &mut changes);
        }
        changes
    }

    fn observe_network(
        &mut self,
        m: &MachineView,
        network_at: Option<DateTime<Utc>>,
        muted: bool,
        changes: &mut Vec<Change>,
    ) {
        if let Some(at) = network_at.filter(|at| self.network_at.is_none_or(|prev| *at > prev))
            && let Some(peer) = &m.tailnet
        {
            let gap = self.network_at.is_none_or(|prev| at - prev > Duration::seconds(60));
            self.network_at = Some(at);
            if let Some(was_online) = self.online
                && was_online != peer.online
            {
                changes.push(change(
                    if peer.online { Kind::Online } else { Kind::Offline },
                    if peer.online {
                        "The machine is online on the tailnet again."
                    } else {
                        "The machine is offline on the tailnet. An alert follows if it stays offline for three minutes."
                    },
                    peer.online && self.offline_alert.is_some() && self.offline_announced && !muted,
                ));
            }
            self.online = Some(peer.online);
            if peer.online {
                self.offline_since = None;
                self.offline_alert = None;
                self.offline_announced = false;
            } else {
                if gap && self.offline_alert.is_none() {
                    self.offline_since = None;
                }
                let since = *self.offline_since.get_or_insert(at);
                if at - since >= OFFLINE_DELAY && self.offline_alert.is_none() {
                    self.offline_alert = Some(since);
                    self.offline_announced = !muted;
                    changes.push(change(
                        Kind::OfflineAlert,
                        "Offline for at least three minutes across fresh tailnet checks.",
                        !muted,
                    ));
                } else if self.offline_alert.is_some() && !self.offline_announced && !muted {
                    self.offline_announced = true;
                    changes.push(change(
                        Kind::AlertsResumed,
                        "The machine is still offline after maintenance.",
                        true,
                    ));
                }
            }
        } else if network_at.is_none() {
            // A control-plane outage, app sleep or stale data cannot accrue outage time.
            self.offline_since = None;
        }
    }

    fn observe_ssh(&mut self, connection: &MachineConnection, changes: &mut Vec<Change>) {
        let available = match connection {
            MachineConnection::Connected { .. } => true,
            MachineConnection::Error { .. } | MachineConnection::Stale { .. } => false,
            MachineConnection::Offline { .. } | MachineConnection::Disabled | MachineConnection::Pending => return,
        };
        if self.ssh_available.is_some_and(|old| old != available) {
            changes.push(if available {
                change(Kind::SshRecovered, "SSH health checks are succeeding again.", false)
            } else {
                change(
                    Kind::SshUnavailable,
                    "A current SSH health read is unavailable. Last successful values are retained.",
                    false,
                )
            });
        }
        self.ssh_available = Some(available);
    }

    fn observe_boot(&mut self, health: &HealthView, changes: &mut Vec<Change>) {
        let Some(id) = &health.boot_id else {
            return;
        };
        if self.boot_id.as_ref().is_some_and(|old| old != id) {
            changes.push(change(
                Kind::Rebooted,
                "A different operating-system boot session was observed.",
                false,
            ));
        }
        self.boot_id = Some(id.clone());
    }

    fn observe_services(&mut self, m: &MachineView, changes: &mut Vec<Change>) {
        for runner in &m.runners {
            let Some(local) = runner.local.as_ref().filter(|r| !r.stale) else {
                continue;
            };
            let status = match local.service_state.as_str() {
                _ if !local.configured => "not configured",
                state if is_active(state) => "running",
                "unknown" => continue,
                other => other,
            };
            if let Some(old) = self.services.insert(runner.name.clone(), status.into())
                && old != status
            {
                changes.push(change(
                    Kind::RunnerServiceChanged,
                    format!("{}: {old} → {status}.", runner.name),
                    false,
                ));
            }
        }
    }

    fn observe_disks(&mut self, health: &HealthView, muted: bool, changes: &mut Vec<Change>) {
        for disk in &health.disks {
            if disk.total_bytes == 0 || disk.available_bytes > disk.total_bytes {
                continue;
            }
            let state = self.disks.entry(disk.mount_point.clone()).or_default();
            if state.at.is_none_or(|at| health.as_of - at > Duration::minutes(3)) {
                state.pending_since = None;
            }
            state.at = Some(health.as_of);
            state.free = disk.available_bytes;
            state.total = disk.total_bytes;
            let percent = free_percent(disk.available_bytes, disk.total_bytes);
            if percent < LOW_DISK_FREE_PERCENT {
                let since = *state.pending_since.get_or_insert(health.as_of);
                if health.as_of - since >= DISK_DELAY && state.active_since.is_none() {
                    state.active_since = Some(since);
                    state.announced = !muted;
                    changes.push(change(Kind::LowDisk, disk_detail(&disk.mount_point, state), !muted));
                }
            } else {
                state.pending_since = None;
                if percent >= LOW_DISK_FREE_PERCENT + DISK_RECOVERY_MARGIN_PERCENT
                    && state.active_since.take().is_some()
                {
                    changes.push(change(
                        Kind::DiskRecovered,
                        format!("Disk space recovered. {}", disk_detail(&disk.mount_point, state)),
                        state.announced && !muted,
                    ));
                    state.announced = false;
                }
            }
            if state.active_since.is_some() && !state.announced && !muted {
                state.announced = true;
                changes.push(change(
                    Kind::AlertsResumed,
                    format!(
                        "Disk space is still low after maintenance. {}",
                        disk_detail(&disk.mount_point, state)
                    ),
                    true,
                ));
            }
        }
    }

    pub fn alerts(
        &self,
        key: &str,
        m: &MachineView,
        network_fresh: bool,
        now: DateTime<Utc>,
    ) -> Vec<FleetMachineAlert> {
        let muted = self.maintenance_until.is_some_and(|t| t > now);
        let mut alerts = Vec::new();
        if let Some(since) = self.offline_alert {
            alerts.push(FleetMachineAlert {
                id: format!("{key}:offline"),
                machine_key: key.into(),
                machine_name: m.display_name.clone(),
                kind: FleetAlertKind::Offline,
                since,
                detail: "Offline for at least three minutes.".into(),
                stale: !network_fresh || m.tailnet.is_none(),
                muted,
            });
        }
        // One disk alert per machine. Keep distinct capacity readings in the detail;
        // shared filesystems (such as macOS System/Data) share a single reading.
        let mut disk_groups: BTreeMap<(u64, u64), Vec<&str>> = BTreeMap::new();
        let mut disk_since = None;
        let mut disk_stale = false;
        for (mount, disk) in &self.disks {
            if let Some(since) = disk.active_since {
                disk_stale |= m.health.as_ref().is_none_or(|h| {
                    h.stale || !h.disks.iter().any(|d| d.mount_point == *mount) || disk.at != Some(h.as_of)
                });
                disk_since = Some(disk_since.map_or(since, |old: DateTime<Utc>| old.min(since)));
                disk_groups.entry((disk.free, disk.total)).or_default().push(mount);
            }
        }
        if let Some(since) = disk_since {
            let detail = disk_groups
                .into_iter()
                .map(|((free, total), mounts)| {
                    disk_detail(
                        &mounts.join(", "),
                        &DiskState {
                            free,
                            total,
                            ..Default::default()
                        },
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            alerts.push(FleetMachineAlert {
                id: format!("{key}:disk"),
                machine_key: key.into(),
                machine_name: m.display_name.clone(),
                kind: FleetAlertKind::LowDisk,
                since,
                detail,
                stale: disk_stale,
                muted,
            });
        }
        alerts
    }
}

fn disk_detail(mount: &str, disk: &DiskState) -> String {
    format!(
        "{mount}: {:.1}% free ({:.1} GiB of {:.1} GiB).",
        free_percent(disk.free, disk.total),
        gib(disk.free),
        gib(disk.total)
    )
}
