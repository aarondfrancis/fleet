//! Shared attention findings for the desktop and local HTTP clients.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::desired::model::{CheckStatus, FleetPlanAction};
use crate::machine_diagnostics::DiagnosticStatus;
use crate::model::{DataSource, FleetView, MachineConnection, MachineView, count_u32};
use crate::monitoring::FleetAlertKind;
use FleetIssueCategory::{
    Alert, Connection, Diagnostic, Github, Inventory, Operation, Queue, Runner, Storage, Tailnet,
};

/// The observation responsible for a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum FleetIssueCategory {
    Alert,
    Diagnostic,
    Inventory,
    Connection,
    Runner,
    Queue,
    Github,
    Tailnet,
    Storage,
    Operation,
}

/// What resolves a finding in Settings, for findings that aren't about one machine or runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum FleetIssueFix {
    /// Add the inventory's organization for live runner status and jobs.
    AddOrganization,
    /// Repair an added organization's GitHub sign-in.
    OrganizationSignIn,
}

/// A stable, actionable finding; stale and muted conditions remain unresolved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FleetIssue {
    pub id: String,
    pub category: FleetIssueCategory,
    pub title: String,
    pub detail: String,
    pub machine_id: Option<String>,
    pub runner_key: Option<String>,
    pub stale: bool,
    pub muted: bool,
    pub fix: Option<FleetIssueFix>,
}

impl FleetIssue {
    fn new(
        id: impl Into<String>,
        category: FleetIssueCategory,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            category,
            title: title.into(),
            detail: detail.into(),
            machine_id: None,
            runner_key: None,
            stale: false,
            muted: false,
            fix: None,
        }
    }
}

/// Rebuild findings after monitoring has attached its current alerts.
pub(crate) fn attach(view: &mut FleetView) {
    let issues: Vec<FleetIssue> = [
        alert_issues(view),
        fleet_issues(view),
        machine_issues(view),
        runner_issues(view),
        queue_issues(view),
        github_issues(view),
        control_check_issues(view),
        operation_issues(view),
    ]
    .into_iter()
    .flatten()
    .collect();
    view.summary.attention = count_u32(issues.len());
    view.issues = issues;
}

fn alert_issues(view: &FleetView) -> Vec<FleetIssue> {
    view.monitoring
        .alerts
        .iter()
        .map(|alert| {
            let mut issue = FleetIssue::new(
                format!("alert:{}", alert.id),
                Alert,
                format!(
                    "{}: {}",
                    alert.machine_name,
                    match alert.kind {
                        FleetAlertKind::Offline => "offline",
                        FleetAlertKind::LowDisk => "low disk space",
                    }
                ),
                &alert.detail,
            );
            issue.machine_id = view
                .machines
                .iter()
                .chain(&view.other_machines)
                .find(|m| m.preferences_key() == alert.machine_key || m.id == alert.machine_key)
                .map(|m| m.id.clone());
            issue.stale = alert.stale;
            issue.muted = alert.muted;
            issue
        })
        .collect()
}

/// Problems with the inventory, Tailscale, local storage and the runner group.
fn fleet_issues(view: &FleetView) -> Vec<FleetIssue> {
    let mut issues = Vec::new();
    for (id, title, error, category) in [
        (
            "inventory",
            "The inventory has a problem",
            &view.plan.inventory_error,
            Inventory,
        ),
        (
            "plan-github",
            "GitHub couldn't be read for the inventory check",
            &view.plan.github_error,
            Inventory,
        ),
        ("doctor", "Runner checks failed", &view.plan.checks_error, Inventory),
        ("tailnet", "Tailscale", &view.tailnet.error, Tailnet),
        (
            "monitoring-storage",
            "History storage has a problem",
            &view.monitoring.storage_error,
            Storage,
        ),
    ] {
        if let Some(error) = error {
            issues.push(FleetIssue::new(id, category, title, error));
        }
    }
    if view.tailnet.local_online == Some(false) && view.tailnet.error.is_none() {
        issues.push(FleetIssue::new(
            "tailnet-offline",
            Tailnet,
            "Tailscale is offline",
            "This Mac is disconnected from the tailnet. Open Tailscale and connect.",
        ));
    }
    for (index, item) in view
        .plan
        .control
        .iter()
        .enumerate()
        .filter(|(_, p)| p.action != FleetPlanAction::NoChange)
    {
        issues.push(FleetIssue::new(
            format!("control:{index}"),
            Inventory,
            "Runner group needs attention",
            &item.reason,
        ));
    }
    issues
}

fn machine_issues(view: &FleetView) -> Vec<FleetIssue> {
    view.machines
        .iter()
        .chain(&view.other_machines)
        .flat_map(|machine| {
            let mut issues = issues_for_machine(machine);
            for issue in &mut issues {
                issue.machine_id = Some(machine.id.clone());
            }
            issues
        })
        .collect()
}

fn issues_for_machine(machine: &MachineView) -> Vec<FleetIssue> {
    let mut issues = Vec::new();
    let has_alert = |kind| machine.monitoring.alerts.iter().any(|a| a.kind == kind);
    let offline_alert = has_alert(FleetAlertKind::Offline);
    let disk_alert = has_alert(FleetAlertKind::LowDisk);
    if let Some(diagnostics) = &machine.diagnostics
        && !diagnostics.stale
        && matches!(machine.connection, MachineConnection::Connected { .. })
    {
        let warnings: Vec<_> = diagnostics
            .checks
            .iter()
            .filter(|c| c.status == DiagnosticStatus::Warn && !(disk_alert && c.id == "disk-space"))
            .collect();
        if !warnings.is_empty() {
            issues.push(FleetIssue::new(
                format!("{}:diagnostics", machine.id),
                Diagnostic,
                format!(
                    "{}: {} diagnostic warning{}",
                    machine.display_name,
                    warnings.len(),
                    if warnings.len() == 1 { "" } else { "s" }
                ),
                warnings
                    .iter()
                    .map(|c| format!("{}: {}", c.title, c.value))
                    .collect::<Vec<_>>()
                    .join(" · "),
            ));
        }
    }
    if let Some(desired) = &machine.desired {
        if let Some(error) = &desired.error {
            if !offline_alert {
                issues.push(FleetIssue::new(
                    format!("{}:check", machine.id),
                    Inventory,
                    format!("{} couldn't be checked", machine.display_name),
                    error,
                ));
            }
        } else {
            let steps: Vec<_> = desired
                .plan
                .iter()
                .filter(|p| p.action != FleetPlanAction::NoChange)
                .collect();
            if !steps.is_empty() {
                issues.push(FleetIssue::new(
                    format!("{}:changes", machine.id),
                    Inventory,
                    format!("{}: runner plan needs attention", machine.display_name),
                    steps
                        .iter()
                        .map(|p| {
                            format!(
                                "{}: {}{}",
                                p.runner,
                                p.reason,
                                if p.blocked { " (blocked)" } else { "" }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" · "),
                ));
            }
        }
        for check in desired.checks.iter().filter(|c| c.status != CheckStatus::Pass) {
            issues.push(FleetIssue::new(
                format!("{}:doctor:{}", machine.id, check.check),
                Inventory,
                format!("{}: {}", machine.display_name, check.check),
                &check.detail,
            ));
        }
    }
    if !offline_alert && machine.desired.as_ref().is_none_or(|d| d.error.is_none()) {
        let problem = match &machine.connection {
            MachineConnection::Stale { reason, .. } => Some(("stale", reason.as_str())),
            MachineConnection::Error { detail } => Some(("SSH unavailable", detail.as_str())),
            MachineConnection::Offline { .. } => Some(("offline", "Offline on the tailnet")),
            MachineConnection::Connected { .. } | MachineConnection::Pending | MachineConnection::Disabled => None,
        };
        if let Some((title, detail)) = problem {
            let mut issue = FleetIssue::new(
                format!("{}:connection", machine.id),
                Connection,
                format!("{}: {title}", machine.display_name),
                detail,
            );
            issue.stale = matches!(machine.connection, MachineConnection::Stale { .. });
            issues.push(issue);
        }
    }
    issues
}

fn runner_issues(view: &FleetView) -> Vec<FleetIssue> {
    view.machines
        .iter()
        .chain(&view.other_machines)
        .flat_map(|m| &m.runners)
        .chain(&view.unmatched_runners)
        .flat_map(|runner| {
            let warnings = (!runner.warnings.is_empty()).then(|| {
                FleetIssue::new(
                    format!("{}:warnings", runner.key),
                    Runner,
                    &runner.name,
                    runner.warnings.join(" · "),
                )
            });
            // Runners update themselves, so only an overdue update needs attention.
            let overdue = runner.github.as_ref().and_then(|g| {
                let update = g.update.as_ref().filter(|u| u.overdue)?;
                Some((g.version.as_deref()?, update))
            });
            let update = overdue.map(|(version, update)| {
                FleetIssue::new(
                    format!("{}:update", runner.key),
                    Runner,
                    format!("{}: runner update overdue", runner.name),
                    format!(
                        "Runs {version}; the latest release is {}. It has missed an update since {}, and GitHub stops sending jobs to runners that miss one for 30 days.",
                        update.latest,
                        update.missing_since.format("%Y-%m-%d"),
                    ),
                )
            });
            [warnings, update].into_iter().flatten().map(|mut issue| {
                issue.runner_key = Some(runner.key.clone());
                issue.machine_id.clone_from(&runner.machine_id);
                issue
            })
        })
        .collect()
}

fn queue_issues(view: &FleetView) -> Vec<FleetIssue> {
    view.queue
        .jobs
        .iter()
        .filter(|q| q.compatible_online == 0)
        .map(|job| {
            let mut issue = FleetIssue::new(
                format!("queue:{}:{}", job.org, job.job.id),
                Queue,
                format!("{}: no compatible online runner observed", job.job.name),
                if job.compatible_runners.is_empty() {
                    format!("No observed runner has labels {}", job.job.labels.join(", "))
                } else {
                    "All observed label-compatible runners are offline or unknown".into()
                },
            );
            issue.stale = !view.queue.complete;
            issue
        })
        .collect()
}

fn github_issues(view: &FleetView) -> Vec<FleetIssue> {
    let mut issues = Vec::new();
    if let Some(org) = &view.plan.organization
        && view.source == DataSource::Live
        && !view.orgs.iter().any(|o| o.login.eq_ignore_ascii_case(&org.name))
    {
        let mut issue = FleetIssue::new(
            "watch-org",
            Github,
            format!("Watch {} on GitHub live", org.name),
            "Runner state comes from the last check. Add the organization in Settings for live status and jobs.",
        );
        issue.fix = Some(FleetIssueFix::AddOrganization);
        issues.push(issue);
    }
    for org in &view.orgs {
        if let Some(error) = org
            .error
            .as_deref()
            .or_else(|| (!org.has_token).then_some("Token missing from Keychain"))
        {
            let mut issue = FleetIssue::new(
                format!("org:{}", org.login),
                Github,
                format!("GitHub: {}", org.login),
                error,
            );
            issue.fix = Some(FleetIssueFix::OrganizationSignIn);
            issues.push(issue);
        }
        if !org.runner_repo_errors.is_empty() {
            let mut issue = FleetIssue::new(
                format!("org:{}:repos", org.login),
                Github,
                format!("GitHub: {} repository runners", org.login),
                org.runner_repo_errors.join(" · "),
            );
            issue.fix = Some(FleetIssueFix::OrganizationSignIn);
            issues.push(issue);
        }
    }
    issues
}

fn control_check_issues(view: &FleetView) -> Vec<FleetIssue> {
    view.plan
        .control_checks
        .iter()
        .filter(|c| c.status != CheckStatus::Pass)
        .map(|check| {
            FleetIssue::new(
                format!("doctor:{}:{}", check.host, check.check),
                Inventory,
                &check.check,
                &check.detail,
            )
        })
        .collect()
}

fn operation_issues(view: &FleetView) -> Vec<FleetIssue> {
    view.plan
        .operation
        .iter()
        .filter_map(|operation| {
            let error = operation.error.as_ref()?;
            Some(FleetIssue::new("last-operation", Operation, &operation.title, error))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_diagnostics::{DiagnosticGroup, MachineDiagnostic, MachineDiagnostics};
    use crate::monitoring::{FleetMachineAlert, FleetMonitoring};
    use chrono::Utc;

    #[test]
    fn alerts_replace_duplicate_disk_and_offline_findings_but_keep_other_diagnostics() {
        let now = Utc::now();
        let (config, observations) = crate::demo::dataset(now);
        let mut view = crate::derive::derive(
            &config,
            &observations,
            &crate::demo_desired::plan_inputs(now),
            now,
            DataSource::Live,
        );
        view.machines.truncate(1);
        view.other_machines.clear();
        view.monitoring = FleetMonitoring::default();
        let machine = &mut view.machines[0];
        machine.desired = None;
        machine.runners.clear();
        machine.connection = MachineConnection::Connected { checked_at: now };
        machine.diagnostics = Some(MachineDiagnostics {
            as_of: now,
            stale: false,
            checks: ["disk-space", "sleep"]
                .map(|id| MachineDiagnostic {
                    id: id.into(),
                    group: DiagnosticGroup::System,
                    title: id.into(),
                    status: DiagnosticStatus::Warn,
                    value: "warning".into(),
                    detail: "evidence".into(),
                })
                .to_vec(),
        });
        let alert = FleetMachineAlert {
            id: "disk".into(),
            machine_key: machine.preferences_key(),
            machine_name: machine.display_name.clone(),
            kind: FleetAlertKind::LowDisk,
            since: now,
            detail: "Low disk space".into(),
            stale: true,
            muted: true,
        };
        machine.monitoring.alerts = vec![alert.clone()];
        view.monitoring.alerts = vec![alert];
        attach(&mut view);
        let diagnostic = view
            .issues
            .iter()
            .find(|i| i.category == FleetIssueCategory::Diagnostic)
            .unwrap();
        assert_eq!(diagnostic.detail, "sleep: warning");
        assert_eq!(
            view.issues
                .iter()
                .filter(|i| i.category == FleetIssueCategory::Alert)
                .count(),
            1
        );
        let alert = view
            .issues
            .iter()
            .find(|i| i.category == FleetIssueCategory::Alert)
            .unwrap();
        assert!(alert.stale && alert.muted);
        assert_eq!(view.summary.attention as usize, view.issues.len());
        view.machines[0].connection = MachineConnection::Offline { last_seen: None };
        view.machines[0].monitoring.alerts[0].kind = FleetAlertKind::Offline;
        view.monitoring.alerts[0].kind = FleetAlertKind::Offline;
        attach(&mut view);
        assert!(!view.issues.iter().any(|i| matches!(
            i.category,
            FleetIssueCategory::Diagnostic | FleetIssueCategory::Connection
        )));
    }
}
