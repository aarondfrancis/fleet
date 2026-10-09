//! Desired state as the UI sees it: what the inventory says about a machine
//! or runner, how that compares with the last check, and the state of checks
//! and changes. Pure data; `derive_machines` fills it in.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::desired::model::{DoctorCheck, FleetSnapshot, Host, Inventory, Organization, PlanItem, StatusRow};
use crate::model::TailnetPeer;

/// A change the user can review and run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ChangeRequest {
    /// Install/reconcile one host's runners.
    Apply {
        host: String,
        runner: Option<String>,
    },
    /// Retire the legacy runner `runner` replaces.
    Retire {
        host: String,
        runner: String,
    },
    Decommission {
        host: String,
        runner: String,
    },
    Restart {
        runner: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum OperationStatus {
    Running,
    Succeeded,
    Failed,
}

/// The current or last change that ran.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OperationView {
    pub title: String,
    pub request: ChangeRequest,
    pub status: OperationStatus,
    pub messages: Vec<String>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// The last host checks (Runner Fleet's `doctor`).
#[derive(Debug, Clone, PartialEq)]
pub struct DoctorView {
    /// The one host checked, or `None` for all of them.
    pub host: Option<String>,
    pub checks: Vec<DoctorCheck>,
    pub error: Option<String>,
    pub checked_at: DateTime<Utc>,
}

/// What the inventory declares for a machine, and how it compares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MachineDesired {
    pub host: Host,
    /// Plan items for this host's runners, from the last check.
    pub plan: Vec<PlanItem>,
    /// Items an apply would carry out.
    pub pending_changes: u32,
    /// Why the last check couldn't read this host.
    pub error: Option<String>,
    pub checked_at: Option<DateTime<Utc>>,
    /// This host's rows from the last host checks.
    pub checks: Vec<DoctorCheck>,
    pub checks_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DesiredRunnerRole {
    /// An inventory runner.
    Declared,
    /// The legacy runner an inventory runner replaces.
    Legacy { replaced_by: String },
    /// Declared `decommission = true`.
    Decommission,
    /// On this machine, but not in the inventory.
    Undeclared,
}

/// What the inventory says about one runner, and how it compares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RunnerDesired {
    pub host_id: String,
    pub role: DesiredRunnerRole,
    /// Labels the inventory gives it (empty unless declared).
    pub desired_labels: Vec<String>,
    /// Its row from the last check.
    pub status: Option<StatusRow>,
    /// Plan items for this runner from the last check.
    pub plan: Vec<PlanItem>,
}

/// Fleet-wide desired-state status.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FleetPlanStatus {
    pub inventory_path: String,
    pub inventory_error: Option<String>,
    pub organization: Option<Organization>,
    pub github_error: Option<String>,
    pub policy_errors: Vec<String>,
    /// Plan items that aren't about one host (runner group policy).
    pub control: Vec<PlanItem>,
    /// When GitHub was last checked; host probe times are independent.
    pub checked_at: Option<DateTime<Utc>>,
    pub refreshing: bool,
    /// Fleet-wide rows of the last host checks (GitHub auth, inventory).
    pub control_checks: Vec<DoctorCheck>,
    pub checks_at: Option<DateTime<Utc>>,
    pub checks_error: Option<String>,
    pub doctor_running: bool,
    pub operation: Option<OperationView>,
}

/// Everything `derive` needs about desired state. Not sent to the UI.
#[derive(Debug, Clone, Default)]
pub struct FleetPlanInputs {
    pub inventory_path: String,
    /// `None` until an inventory has been read.
    pub inventory: Option<Result<Inventory, String>>,
    pub snapshot: Option<FleetSnapshot>,
    /// GitHub freshness; local runner data uses `host_checked_at` exclusively.
    pub checked_at: Option<DateTime<Utc>>,
    pub host_checked_at: BTreeMap<String, DateTime<Utc>>,
    /// SSH target → the hostname SSH resolves it to.
    pub ssh_hosts: BTreeMap<String, String>,
    pub doctor: Option<DoctorView>,
    pub refreshing: bool,
    pub doctor_running: bool,
    pub operation: Option<OperationView>,
}

/// The tailnet machine an inventory host's SSH target names. `ssh_hostname`
/// is what SSH resolves the target to (`ssh -G`), which matters for aliases
/// like `mini` → `mac-mini.<tailnet>.ts.net`.
pub fn peer_for_host<'a>(host: &Host, ssh_hostname: Option<&str>, peers: &'a [TailnetPeer]) -> Option<&'a TailnetPeer> {
    let target = host.ssh_target.as_deref()?;
    let written = target.rsplit('@').next()?;
    ssh_hostname
        .into_iter()
        .chain([written])
        .find_map(|name| peer_named(&name.to_ascii_lowercase(), peers))
}

fn peer_named<'a>(name: &str, peers: &'a [TailnetPeer]) -> Option<&'a TailnetPeer> {
    let short = name.split('.').next().unwrap_or(name);
    peers
        .iter()
        .find(|p| p.dns_name.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(name)))
        .or_else(|| peers.iter().find(|p| p.ips.iter().any(|ip| ip == name)))
        .or_else(|| {
            peers.iter().find(|p| {
                p.hostname.eq_ignore_ascii_case(short)
                    || p.dns_name
                        .as_deref()
                        .and_then(|d| d.split('.').next())
                        .is_some_and(|d| d.eq_ignore_ascii_case(short))
            })
        })
}

/// Hosts that share a tailnet peer or SSH destination must not own independent execution targets.
pub fn host_identity_conflicts(
    inventory: &Inventory,
    ssh_hosts: &BTreeMap<String, String>,
    peers: &[TailnetPeer],
) -> BTreeMap<String, String> {
    let mut owners: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for host in &inventory.hosts {
        let resolved = host.ssh_target.as_ref().and_then(|target| ssh_hosts.get(target));
        if let Some(peer) = peer_for_host(host, resolved.map(String::as_str), peers) {
            owners
                .entry(format!("peer:{}", peer.node_id))
                .or_default()
                .insert(host.id.clone());
        }
        for target in host
            .ssh_target
            .as_deref()
            .into_iter()
            .chain(resolved.map(String::as_str))
        {
            let hostname = target
                .rsplit('@')
                .next()
                .unwrap_or(target)
                .trim_end_matches('.')
                .to_ascii_lowercase();
            owners
                .entry(format!("ssh:{hostname}"))
                .or_default()
                .insert(host.id.clone());
        }
    }
    let mut conflicts: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for ids in owners.values().filter(|ids| ids.len() > 1) {
        for id in ids {
            conflicts.entry(id.clone()).or_default().extend(ids.iter().cloned());
        }
    }
    conflicts.into_iter().map(|(id, owners)| {
        let hosts = owners.into_iter().collect::<Vec<_>>().join(", ");
        (id, format!("Inventory hosts {hosts} share the same machine or SSH destination. Give each machine one inventory host before making changes."))
    }).collect()
}
