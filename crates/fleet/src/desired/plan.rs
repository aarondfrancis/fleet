//! Desired versus actual, as a list of plan items. Ported from `plan.py`.
//! Pure: it never contacts GitHub or a host.

use super::model::{
    FleetPlanAction, FleetSnapshot, GithubRunner, Host, Inventory, LocalRunner, LookupError, PlanItem, Runner,
    is_active, iter_runners,
};
use super::py::repr;
use super::state::label_drift;

/// An empty `runner_name` selects every runner, as in the original.
pub fn generate_plan(
    inventory: &Inventory,
    snapshot: &FleetSnapshot,
    host_id: Option<&str>,
    runner_name: Option<&str>,
) -> Result<Vec<PlanItem>, LookupError> {
    let runner_name = runner_name.filter(|n| !n.is_empty());
    let hosts = inventory.selected_hosts(host_id)?;
    let group = &inventory.organization.runner_group;
    let mut items = Vec::new();
    if !snapshot.policy_errors.is_empty() {
        items.push(
            PlanItem::new(
                FleetPlanAction::HardenGroup,
                "control",
                group,
                snapshot.policy_errors.join("; "),
            )
            .blocked(),
        );
    }
    let mut matched = false;
    for (host, runner) in iter_runners(&hosts) {
        if runner_name.is_some_and(|n| n != runner.name) {
            continue;
        }
        matched = true;
        let github = snapshot.github.get(&runner.name);
        let local = snapshot.local.get(&runner.name);
        let host_error = snapshot.host_errors.get(&host.id);

        if snapshot.github_error.is_some() || host_error.is_some() {
            let mut reasons = Vec::new();
            if let Some(e) = &snapshot.github_error {
                reasons.push(format!("GitHub unavailable: {e}"));
            }
            if let Some(e) = host_error {
                reasons.push(format!("host unavailable: {e}"));
            }
            items.push(PlanItem::new(FleetPlanAction::Inspect, &host.id, &runner.name, reasons.join("; ")).blocked());
        } else if runner.decommission {
            items.push(decommission_plan(host, runner, github, local));
            continue;
        } else {
            items.extend(desired_runner_plan(host, runner, group, github, local));
        }
        if runner.replaces.is_some() {
            append_legacy_plan(&mut items, &host.id, runner, snapshot);
        }
    }
    if let (Some(name), false) = (runner_name, matched) {
        return Err(LookupError(format!("desired runner {} is not selected", repr(name))));
    }
    Ok(items)
}

/// Steps that bring one desired runner, which GitHub and its host both
/// reported on, to the inventory.
fn desired_runner_plan(
    host: &Host,
    runner: &Runner,
    group: &str,
    github: Option<&GithubRunner>,
    local: Option<&LocalRunner>,
) -> Vec<PlanItem> {
    let apply = format!("bin/fleet apply {} --runner {}", host.id, runner.name);
    let item = |action, reason: String| PlanItem::new(action, &host.id, &runner.name, reason);
    let (github, local) = match (github, local.filter(|l| l.configured)) {
        (None, None) => {
            let reason = format!(
                "register in {} and start {}",
                host.runner_path(runner),
                host.service_mode
            );
            return vec![item(FleetPlanAction::Install, reason).command(apply)];
        }
        (None, Some(_)) => {
            return vec![
                item(
                    FleetPlanAction::RepairRegistration,
                    "local configuration exists but GitHub has no exact runner; manual inspection required".into(),
                )
                .blocked(),
            ];
        }
        (Some(_), None) => {
            return vec![
                item(
                    FleetPlanAction::RecoverLocal,
                    "GitHub runner exists but its exact inventory directory is not configured".into(),
                )
                .blocked(),
            ];
        }
        (Some(github), Some(local)) => (github, local),
    };
    let mut items = Vec::new();
    let (missing, unexpected) = label_drift(&host.labels_for(runner), github);
    if !missing.is_empty() || !unexpected.is_empty() {
        let mut changes = Vec::new();
        if !missing.is_empty() {
            changes.push(format!("add {}", missing.join(", ")));
        }
        if !unexpected.is_empty() {
            changes.push(format!("remove {}", unexpected.join(", ")));
        }
        items.push(item(FleetPlanAction::ReconcileLabels, changes.join("; ")).command(apply.clone()));
    }
    match github.in_desired_group {
        Some(false) => {
            items.push(item(FleetPlanAction::AssignGroup, format!("move into {group}")).command(apply.clone()));
        }
        None => items.push(
            item(
                FleetPlanAction::InspectGroup,
                format!("desired group {} was not found", repr(group)),
            )
            .blocked(),
        ),
        Some(true) => {}
    }
    if !is_active(&local.service_state) {
        if github.busy {
            items.push(
                item(
                    FleetPlanAction::PreserveService,
                    format!("service is {}, but GitHub reports a busy job", local.service_state),
                )
                .blocked(),
            );
        } else {
            items.push(
                item(
                    FleetPlanAction::StartService,
                    format!("service is {}", local.service_state),
                )
                .command(apply),
            );
        }
    } else if github.status != "online" {
        items.push(
            item(
                FleetPlanAction::InspectOffline,
                "GitHub reports the runner offline while its native service is active".into(),
            )
            .command(format!("bin/fleet logs {}", runner.name))
            .blocked(),
        );
    }
    if items.is_empty() {
        items.push(item(FleetPlanAction::NoChange, "desired state is satisfied".into()));
    }
    items
}

/// The replacement is online, idle, configured and its service active.
fn replacement_healthy(github: Option<&GithubRunner>, local: Option<&LocalRunner>) -> bool {
    github.is_some_and(|g| g.status == "online" && !g.busy) && local.is_some_and(|l| l.configured && l.service_active())
}

fn append_legacy_plan(items: &mut Vec<PlanItem>, host_id: &str, runner: &Runner, snapshot: &FleetSnapshot) {
    let Some(legacy) = &runner.replaces else { return };
    let old_github = snapshot.github.get(&legacy.name);
    let old_local = snapshot.local.get(&legacy.name);
    if old_github.is_none() && !old_local.is_some_and(|l| l.configured) {
        return;
    }
    let healthy = replacement_healthy(snapshot.github.get(&runner.name), snapshot.local.get(&runner.name));
    let item = if !healthy {
        PlanItem::new(
            FleetPlanAction::PreserveLegacy,
            host_id,
            &legacy.name,
            format!(
                "keep capacity until {} is online, idle, configured, and active",
                runner.name
            ),
        )
        .blocked()
    } else if old_github.is_some_and(|g| g.busy) {
        PlanItem::new(
            FleetPlanAction::PreserveLegacy,
            host_id,
            &legacy.name,
            "legacy runner is busy",
        )
        .blocked()
    } else {
        PlanItem::new(
            FleetPlanAction::RetireLegacy,
            host_id,
            &legacy.name,
            format!(
                "replacement {} is healthy; exact directory {} will be preserved",
                runner.name, legacy.directory
            ),
        )
        .command(format!("bin/fleet retire {host_id} --runner {}", runner.name))
    };
    items.push(item);
}

fn decommission_plan(
    host: &Host,
    runner: &Runner,
    github: Option<&GithubRunner>,
    local: Option<&LocalRunner>,
) -> PlanItem {
    let configured = local.is_some_and(|l| l.configured);
    let path = host.runner_path(runner);
    let Some(github) = github else {
        return if configured {
            PlanItem::new(
                FleetPlanAction::RepairRegistration,
                &host.id,
                &runner.name,
                "local configuration exists but GitHub has no exact runner; manual inspection required",
            )
            .blocked()
        } else {
            PlanItem::new(
                FleetPlanAction::NoChange,
                &host.id,
                &runner.name,
                format!("decommissioned; exact directory {path} is preserved"),
            )
        };
    };
    if github.busy {
        return PlanItem::new(
            FleetPlanAction::PreserveDecommission,
            &host.id,
            &runner.name,
            "runner is busy; decommission after its job finishes",
        )
        .blocked();
    }
    if !configured && github.status == "online" {
        return PlanItem::new(
            FleetPlanAction::Inspect,
            &host.id,
            &runner.name,
            "GitHub reports this name online but its exact inventory directory is not configured",
        )
        .blocked();
    }
    let detail = if configured {
        format!("stop and deregister {path}; the directory is preserved")
    } else {
        "delete the offline GitHub registration; no local directory is configured".into()
    };
    PlanItem::new(FleetPlanAction::Decommission, &host.id, &runner.name, detail)
        .command(format!("bin/fleet decommission {} --runner {}", host.id, runner.name))
}
