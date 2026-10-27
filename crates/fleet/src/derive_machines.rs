//! Assembling machines. Each runner-capable tailnet machine becomes one
//! machine, joined with its inventory host, SSH observations and GitHub runners.
//! Runner names and paths come from the inventory; health also works without one.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::machine::{MachineReport, MachineTarget, OsFamily};
use chrono::{DateTime, Utc};

use super::{GhEntry, GithubIndex, connection, runner_view};
use crate::desired::model::{FleetPlanAction, Host, Inventory, PlanItem, Platform, StatusRow};
use crate::desired::plan::generate_plan;
use crate::desired::state::build_status_rows;
use crate::fleet_plan::{
    DesiredRunnerRole, FleetPlanInputs, MachineDesired, RunnerDesired, host_identity_conflicts, peer_for_host,
};
use crate::model::*;

pub(super) struct Machines {
    /// In the inventory or with runners.
    pub fleet: Vec<MachineView>,
    /// Other runner-capable tailnet machines.
    pub other: Vec<MachineView>,
}

/// One machine before it's turned into a view.
struct Slot<'a> {
    id: String,
    peer: Option<&'a TailnetPeer>,
    host: Option<&'a Host>,
    identity_error: Option<String>,
}

/// The last check's status rows and plan items, by runner and by host.
pub(super) struct Checked {
    rows: HashMap<String, StatusRow>,
    pub plan: Vec<PlanItem>,
}

impl Checked {
    /// Compares the inventory with the last check, when both are available.
    pub fn build(plan: &FleetPlanInputs) -> Option<Self> {
        let inventory = plan.inventory.as_ref()?.as_ref().ok()?;
        let snapshot = plan.snapshot.as_ref()?;
        Some(Self {
            rows: build_status_rows(inventory, snapshot, None)
                .unwrap_or_default()
                .into_iter()
                .filter(|r| r.drift != "unmanaged")
                .map(|r| (r.runner.to_lowercase(), r))
                .collect(),
            plan: generate_plan(inventory, snapshot, None, None).unwrap_or_default(),
        })
    }
}

/// Inputs shared by every machine in one derivation.
struct DeriveCtx<'a> {
    obs: &'a Observations,
    plan: &'a FleetPlanInputs,
    inventory: Option<&'a Inventory>,
    checked: Option<&'a Checked>,
    gh: &'a GithubIndex,
    now: DateTime<Utc>,
}

/// A machine's latest SSH read, and whether it is current.
struct Observed<'a> {
    report: Option<&'a Timed<MachineReport>>,
    connected: bool,
}

pub(super) fn assemble(
    config: &AppConfig,
    obs: &Observations,
    plan: &FleetPlanInputs,
    checked: Option<&Checked>,
    gh: &GithubIndex,
    matched: &mut HashSet<String>,
    now: DateTime<Utc>,
) -> Machines {
    let snapshot = obs.tailscale.as_ref().map(|t| &t.value);
    let self_node = snapshot.and_then(|s| s.self_node_id.as_deref());
    let peers: Vec<TailnetPeer> = snapshot
        .map(|s| {
            s.peers
                .iter()
                .filter(|p| Some(p.node_id.as_str()) != self_node)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let ignored: HashSet<&str> = config
        .ignored_peers
        .iter()
        .map(|i| i.tailscale_node_id.as_str())
        .collect();
    let inventory = plan.inventory.as_ref().and_then(|r| r.as_ref().ok());
    let ctx = DeriveCtx {
        obs,
        plan,
        inventory,
        checked,
        gh,
        now,
    };

    let slots = slots(&peers, &ignored, inventory, &plan.ssh_hosts);
    let mut fleet = Vec::new();
    let mut other = Vec::new();
    for slot in &slots {
        let mut view = machine_view(slot, &ctx, matched);
        let key = view.preferences_key();
        if let Some(preferences) = config
            .machine_preferences
            .get(&key)
            .or_else(|| config.machine_preferences.get(&view.id))
        {
            if let Some(name) = &preferences.display_name {
                view.display_name.clone_from(name);
            }
            view.description.clone_from(&preferences.description);
        }
        let is_fleet = slot.host.is_some() || !view.runners.is_empty();
        if is_fleet {
            fleet.push(view);
        } else if slot.peer.is_some_and(TailnetPeer::can_host_runners) {
            other.push(view);
        }
    }
    let order = |m: &MachineView| (m.display_name.to_lowercase(), m.id.clone());
    fleet.sort_by_key(order);
    other.sort_by_key(order);
    Machines { fleet, other }
}

/// Every machine: inventory hosts first (matched to their tailnet machine),
/// then the remaining tailnet machines.
fn slots<'a>(
    peers: &'a [TailnetPeer],
    ignored: &HashSet<&str>,
    inventory: Option<&'a Inventory>,
    ssh_hosts: &BTreeMap<String, String>,
) -> Vec<Slot<'a>> {
    let mut slots: Vec<Slot<'a>> = Vec::new();
    let mut claimed_nodes: HashSet<&str> = HashSet::new();

    for host in inventory.map(|i| i.hosts.as_slice()).unwrap_or_default() {
        let resolved = host
            .ssh_target
            .as_deref()
            .and_then(|t| ssh_hosts.get(t))
            .map(String::as_str);
        let peer = peer_for_host(host, resolved, peers);
        if let Some(p) = peer {
            claimed_nodes.insert(&p.node_id);
        }
        slots.push(Slot {
            id: peer.map_or_else(|| inventory_machine_key(&host.id), |p| tailnet_machine_key(&p.node_id)),
            peer,
            host: Some(host),
            identity_error: None,
        });
    }

    let conflicts = inventory
        .map(|inventory| host_identity_conflicts(inventory, ssh_hosts, peers))
        .unwrap_or_default();
    for slot in &mut slots {
        if let Some(host) = slot.host
            && let Some(error) = conflicts.get(&host.id)
        {
            slot.identity_error = Some(error.clone());
            slot.id = inventory_machine_key(&host.id);
            // Shared peer facts and preferences cannot identify either inventory host.
            slot.peer = None;
        }
    }

    for p in peers {
        if claimed_nodes.contains(p.node_id.as_str()) || ignored.contains(p.node_id.as_str()) {
            continue;
        }
        claimed_nodes.insert(&p.node_id);
        slots.push(Slot {
            id: tailnet_machine_key(&p.node_id),
            peer: Some(p),
            host: None,
            identity_error: None,
        });
    }
    slots
}

fn machine_view(slot: &Slot<'_>, ctx: &DeriveCtx<'_>, matched: &mut HashSet<String>) -> MachineView {
    let observation = observation(slot, ctx.obs);
    let observed_connection = connection(slot.peer, observation, ctx.now);
    let report = observation.and_then(|o| o.report.as_ref());
    let observed = Observed {
        report,
        connected: matches!(observed_connection, MachineConnection::Connected { .. }),
    };
    let connection = if let Some(error) = &slot.identity_error {
        MachineConnection::Error { detail: error.clone() }
    } else if slot.host.is_some_and(|h| !h.enabled) {
        MachineConnection::Disabled
    } else {
        observed_connection
    };
    let fresh = matches!(connection, MachineConnection::Connected { .. });

    let health = report.map(|r| {
        let v = &r.value;
        HealthView {
            as_of: r.at,
            stale: !fresh,
            cpu_percent: v.cpu_percent,
            load_average: v.load_average,
            logical_cores: v.logical_cores,
            memory_total_bytes: v.memory_total_bytes,
            memory_available_bytes: v.memory_available_bytes,
            disks: v.disks.clone(),
            uptime_secs: v.uptime_secs,
            boot_id: v.boot_id.clone(),
        }
    });

    let (mut runners, mut desired) = match (slot.host, ctx.inventory) {
        (Some(host), Some(inventory)) => (
            inventory_runners(slot, host, inventory, ctx, &observed, matched),
            Some(machine_desired(host, ctx.plan, ctx.checked)),
        ),
        _ => (Vec::new(), None),
    };
    if let Some(error) = &slot.identity_error {
        if let Some(desired) = &mut desired {
            desired.error = Some(error.clone());
            desired.pending_changes = 0;
            for item in &mut desired.plan {
                item.blocked = true;
                item.reason.clone_from(error);
            }
        }
        for runner in &mut runners {
            if let Some(desired) = &mut runner.desired {
                desired.status = None;
                for item in &mut desired.plan {
                    item.blocked = true;
                    item.reason.clone_from(error);
                }
            }
        }
    }
    runners.sort_by_key(|r| (r.name.to_lowercase(), r.key.clone()));

    MachineView {
        id: slot.id.clone(),
        description: None,
        display_name: slot
            .host
            .map(|h| h.id.clone())
            .or_else(|| slot.peer.map(|p| p.hostname.clone()))
            .unwrap_or_else(|| slot.id.clone()),
        host: slot
            .host
            .and_then(|h| h.ssh_target.clone())
            .or_else(|| slot.peer.map(TailnetPeer::address))
            .unwrap_or_default(),
        os: report
            .map(|r| r.value.os)
            .or_else(|| slot.peer.map(|p| OsFamily::from_tailnet(&p.os)))
            .or_else(|| slot.host.map(|h| h.platform.into())),
        os_version: report.and_then(|r| r.value.os_version.clone()),
        tailnet: slot.peer.cloned(),
        connection,
        health,
        diagnostics: report.map(|r| crate::machine_diagnostics::MachineDiagnostics {
            as_of: r.at,
            stale: !fresh,
            checks: crate::machine_diagnostics::evaluate(&r.value, slot.host),
        }),
        monitoring: crate::monitoring::FleetMachineMonitoring::default(),
        runners,
        warnings: report.map(|r| r.value.warnings.clone()).unwrap_or_default(),
        desired,
    }
}

fn observation<'a>(slot: &Slot<'_>, obs: &'a Observations) -> Option<&'a MachineObservation> {
    if slot.identity_error.is_some() {
        return None;
    }
    let key = slot
        .host
        .map_or_else(|| slot.id.clone(), |h| inventory_machine_key(&h.id));
    let target = slot
        .host
        .cloned()
        .map(MachineTarget::Inventory)
        .or_else(|| slot.peer.map(MachineTarget::discovered))?;
    obs.machines.get(&key).filter(|o| o.target == target)
}

/// Declared runners (and the legacy runners they replace) and GitHub runners
/// carrying this host's `host-*` label.
fn inventory_runners(
    slot: &Slot<'_>,
    host: &Host,
    inventory: &Inventory,
    ctx: &DeriveCtx<'_>,
    observed: &Observed<'_>,
    matched: &mut HashSet<String>,
) -> Vec<RunnerView> {
    let DeriveCtx {
        plan, checked, gh, now, ..
    } = *ctx;
    let mut runners: Vec<RunnerView> = Vec::new();
    let org = inventory.organization.name.to_lowercase();
    let Observed { report, connected } = *observed;
    let local = |name: &str| {
        if slot.identity_error.is_some() {
            return None;
        }
        if let Some(report) = report
            && plan.host_checked_at.get(&host.id).is_none_or(|at| report.at >= *at)
        {
            report.value.runners.get(name).map(|r| (r, report.at, !connected))
        } else {
            plan.snapshot
                .as_ref()
                .and_then(|s| s.local.get(name))
                .zip(plan.host_checked_at.get(&host.id))
                .map(|(r, at)| {
                    (
                        r,
                        *at,
                        now - *at > super::SSH_STALE_AFTER
                            || plan
                                .snapshot
                                .as_ref()
                                .is_some_and(|s| s.host_errors.contains_key(&host.id)),
                    )
                })
        }
    };
    let mut declared: Vec<(String, DesiredRunnerRole, Vec<String>)> = Vec::new();
    for runner in &host.runners {
        let role = if runner.decommission {
            DesiredRunnerRole::Decommission
        } else {
            DesiredRunnerRole::Declared
        };
        declared.push((runner.name.clone(), role, host.labels_for(runner)));
        if let Some(old) = &runner.replaces {
            declared.push((
                old.name.clone(),
                DesiredRunnerRole::Legacy {
                    replaced_by: runner.name.clone(),
                },
                Vec::new(),
            ));
        }
    }

    for (name, role, labels) in declared {
        let desired = runner_desired(host, &name, role.clone(), labels, checked);
        if let Some(existing) = runners.iter_mut().find(|r| r.name.eq_ignore_ascii_case(&name)) {
            existing.desired = Some(desired);
            continue;
        }
        let live = gh.find(&org, &name).filter(|e| !matched.contains(&e.key));
        // Without live GitHub data (e.g. the org isn't added to the app), what
        // the last check read from GitHub, marked stale by its age.
        let checked = live
            .is_none()
            .then(|| checked_github(plan, &org, &name, host, now))
            .flatten();
        let gh_entry = live.or(checked.as_ref());
        let configured = local(&name).is_some_and(|(l, _, _)| l.configured);
        // Retired legacy runners and finished decommissions have nothing left to show.
        let gone = gh_entry.is_none() && !configured;
        if gone && !matches!(role, DesiredRunnerRole::Declared) {
            continue;
        }
        if let Some(e) = live {
            matched.insert(e.key.clone());
        }
        let key = gh_entry.map_or_else(|| format!("inv:{}:{name}", host.id), |e| e.key.clone());
        let mut view = runner_view(key, Some(slot.id.as_str()), gh_entry, local(&name), gh);
        view.name = name;
        view.org.get_or_insert_with(|| org.clone());
        view.desired = Some(desired);
        runners.push(view);
    }

    if slot.identity_error.is_some() {
        return runners;
    }
    let Some(label) = host.labels.iter().find(|l| l.starts_with("host-")) else {
        return runners;
    };
    let labelled: Vec<_> = gh
        .runners
        .iter()
        .filter(|e| e.org == org && e.repo.is_none() && !matched.contains(&e.key) && e.runner.labels.contains(label))
        .collect();
    for e in labelled {
        matched.insert(e.key.clone());
        let mut view = runner_view(e.key.clone(), Some(slot.id.as_str()), Some(e), None, gh);
        view.desired = Some(runner_desired(
            host,
            &e.runner.name,
            DesiredRunnerRole::Undeclared,
            Vec::new(),
            checked,
        ));
        runners.push(view);
    }
    runners
}

/// A GitHub entry from the last desired-state check, for a runner the live
/// GitHub data doesn't list.
fn checked_github(plan: &FleetPlanInputs, org: &str, name: &str, host: &Host, now: DateTime<Utc>) -> Option<GhEntry> {
    let runner = plan.snapshot.as_ref()?.github.get(name)?;
    let at = plan.checked_at?;
    Some(GhEntry {
        key: format!("gh:{org}:{}", runner.id),
        org: org.to_string(),
        repo: None,
        update: None,
        runner: GhRunner {
            id: u64::try_from(runner.id).ok()?,
            name: runner.name.clone(),
            os: match host.platform {
                Platform::DarwinArm64 => "macOS",
                Platform::WindowsArm64 | Platform::WindowsX64 => "Windows",
                Platform::LinuxX64 => "Linux",
            }
            .into(),
            status: runner.status.clone(),
            busy: runner.busy,
            labels: runner.labels.iter().cloned().collect(),
            ephemeral: None,
            version: None,
            runner_group_id: None,
        },
        at,
        stale: now - at > super::GITHUB_STALE_AFTER,
    })
}

fn runner_desired(
    host: &Host,
    name: &str,
    role: DesiredRunnerRole,
    desired_labels: Vec<String>,
    checked: Option<&Checked>,
) -> RunnerDesired {
    RunnerDesired {
        host_id: host.id.clone(),
        role,
        desired_labels,
        status: checked.and_then(|c| c.rows.get(&name.to_lowercase()).cloned()),
        plan: checked
            .map(|c| {
                c.plan
                    .iter()
                    .filter(|i| i.host == host.id && i.runner.eq_ignore_ascii_case(name))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn machine_desired(host: &Host, plan: &FleetPlanInputs, checked: Option<&Checked>) -> MachineDesired {
    let items: Vec<PlanItem> = checked
        .map(|c| c.plan.iter().filter(|i| i.host == host.id).cloned().collect())
        .unwrap_or_default();
    let doctor = plan.doctor.as_ref();
    MachineDesired {
        host: host.clone(),
        pending_changes: count_u32(
            items
                .iter()
                .filter(|i| !i.blocked && i.action != FleetPlanAction::NoChange)
                .count(),
        ),
        plan: items,
        error: plan
            .snapshot
            .as_ref()
            .and_then(|s| s.host_errors.get(&host.id).cloned()),
        checked_at: plan.host_checked_at.get(&host.id).copied(),
        checks: doctor
            .map(|d| d.checks.iter().filter(|c| c.host == host.id).cloned().collect())
            .unwrap_or_default(),
        checks_at: doctor
            .filter(|d| d.checks.iter().any(|c| c.host == host.id))
            .map(|d| d.checked_at),
    }
}
