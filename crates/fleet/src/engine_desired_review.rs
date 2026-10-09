//! Changes to desired state: what can be requested, the review shown before
//! running one, and the pure function that builds it from a fresh snapshot.

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::super::EngineError;
use crate::desired::model::{FleetPlanAction, FleetSnapshot, Host, Inventory, LookupError, NO_VALUE, PlanItem};
use crate::desired::operations::{
    OperationError, check_declared_for_decommission, check_enabled, check_idle, check_one_runner_at_a_time,
    check_restartable, inventory_runner, retirement_target, select_runners,
};
use crate::desired::plan::generate_plan;
use crate::desired::state::build_status_rows;
use crate::fleet_plan::ChangeRequest;

static REVIEW_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The inventory host a change is about.
pub(super) fn request_host(request: &ChangeRequest, inventory: &Inventory) -> Result<String, EngineError> {
    match request {
        ChangeRequest::Apply { host, .. }
        | ChangeRequest::Retire { host, .. }
        | ChangeRequest::Decommission { host, .. } => Ok(host.clone()),
        ChangeRequest::Restart { runner } => inventory
            .find_runner(runner)
            .map(|(h, _, _)| h.id.clone())
            .map_err(|e| EngineError::NotFound(e.0)),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ReviewView {
    pub id: String,
    pub request: ChangeRequest,
    pub title: String,
    /// Plan items for exactly this change, from a fresh read of the host.
    pub items: Vec<PlanItem>,
    /// Why it can't run as shown. Empty when it can.
    pub blockers: Vec<String>,
    /// The Windows service account whose password installing needs.
    pub needs_service_password: Option<String>,
    /// systemd hosts: offered for hosts without non-interactive sudo.
    pub accepts_sudo_password: bool,
    /// Retire/decommission must be confirmed by typing this.
    pub confirm_phrase: Option<String>,
    pub checked_at: DateTime<Utc>,
    pub ready: bool,
}

// Deserialize only: passwords come from the UI and are never sent back.
#[derive(Clone, Default, PartialEq, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSecrets {
    pub service_password: Option<String>,
    pub sudo_password: Option<String>,
}

impl std::fmt::Debug for ChangeSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangeSecrets")
            .field(
                "service_password",
                &self.service_password.as_ref().map(|_| "[redacted]"),
            )
            .field("sudo_password", &self.sudo_password.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// What one kind of change would do.
struct Proposal {
    title: String,
    items: Vec<PlanItem>,
    confirm_phrase: Option<String>,
}

fn not_found(e: LookupError) -> EngineError {
    EngineError::NotFound(e.0)
}

fn refused(e: OperationError) -> EngineError {
    EngineError::Invalid(e.0)
}

fn missing(e: OperationError) -> EngineError {
    EngineError::NotFound(e.0)
}

/// Collects the message of a failed guard as a blocker.
fn block(blockers: &mut Vec<String>, guard: Result<(), OperationError>) {
    if let Err(e) = guard {
        blockers.push(e.0);
    }
}

pub(super) fn build_review(
    inventory: &Inventory,
    host: &Host,
    request: &ChangeRequest,
    snapshot: &FleetSnapshot,
) -> Result<ReviewView, EngineError> {
    let mut blockers = Vec::new();
    if let Some(e) = &snapshot.github_error {
        blockers.push(format!("GitHub unavailable: {e}"));
    }
    if let Some(e) = snapshot.host_errors.get(&host.id) {
        blockers.push(format!("{} unavailable: {e}", host.id));
    }
    let proposal = match request {
        ChangeRequest::Apply { runner, .. } => {
            review_apply(inventory, host, runner.as_deref(), snapshot, &mut blockers)?
        }
        ChangeRequest::Retire { runner, .. } => review_retire(inventory, host, runner, snapshot, &mut blockers)?,
        ChangeRequest::Decommission { runner, .. } => {
            review_decommission(inventory, host, runner, snapshot, &mut blockers)?
        }
        ChangeRequest::Restart { runner } => review_restart(inventory, host, runner, snapshot, &mut blockers)?,
    };
    let Proposal {
        title,
        items,
        confirm_phrase,
    } = proposal;
    let installs = items.iter().any(|i| i.action == FleetPlanAction::Install && !i.blocked);
    let needs_service_password = match (&host.service_account, host.needs_service_password() && installs) {
        (Some(account), true) => Some(account.clone()),
        _ => None,
    };
    let accepts_sudo_password = host.uses_sudo()
        && (matches!(
            request,
            ChangeRequest::Retire { .. } | ChangeRequest::Decommission { .. } | ChangeRequest::Restart { .. }
        ) || items.iter().any(|i| {
            !i.blocked && (i.action == FleetPlanAction::Install || i.action == FleetPlanAction::StartService)
        }));
    Ok(ReviewView {
        id: format!(
            "review_{}_{}_{}",
            host.id,
            Utc::now().timestamp_millis(),
            REVIEW_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ),
        request: request.clone(),
        title,
        ready: blockers.is_empty(),
        items,
        blockers,
        needs_service_password,
        accepts_sudo_password,
        confirm_phrase,
        checked_at: Utc::now(),
    })
}

fn review_apply(
    inventory: &Inventory,
    host: &Host,
    runner: Option<&str>,
    snapshot: &FleetSnapshot,
    blockers: &mut Vec<String>,
) -> Result<Proposal, EngineError> {
    let items = generate_plan(inventory, snapshot, Some(&host.id), runner).map_err(not_found)?;
    block(blockers, check_enabled(host));
    match select_runners(host, runner) {
        Ok(selected) => {
            // A migration only blocks once there is something to apply.
            if items.iter().any(PlanItem::is_runnable) {
                block(blockers, check_one_runner_at_a_time(host, runner, &selected));
            }
        }
        Err(e) => blockers.push(e.0),
    }
    blockers.extend(snapshot.policy_errors.iter().cloned());
    let title = match runner {
        Some(r) => format!("Apply {r} on {}", host.id),
        None => format!("Apply {}", host.id),
    };
    Ok(Proposal {
        title,
        items,
        confirm_phrase: None,
    })
}

fn review_retire(
    inventory: &Inventory,
    host: &Host,
    runner: &str,
    snapshot: &FleetSnapshot,
    blockers: &mut Vec<String>,
) -> Result<Proposal, EngineError> {
    let declared = host.runners.iter().any(|r| r.name == runner);
    let (_, legacy) = retirement_target(host, runner).map_err(if declared { refused } else { missing })?;
    let items: Vec<PlanItem> = generate_plan(inventory, snapshot, Some(&host.id), Some(runner))
        .map_err(not_found)?
        .into_iter()
        .filter(|i| i.runner == legacy.name)
        .collect();
    if !items.iter().any(|i| i.action == FleetPlanAction::RetireLegacy) {
        blockers.push(match items.first() {
            Some(i) => i.reason.clone(),
            None => format!("{} is already gone.", legacy.name),
        });
    }
    Ok(Proposal {
        title: format!("Retire {}", legacy.name),
        items,
        confirm_phrase: Some(format!("retire {}", legacy.name)),
    })
}

fn review_decommission(
    inventory: &Inventory,
    host: &Host,
    runner: &str,
    snapshot: &FleetSnapshot,
    blockers: &mut Vec<String>,
) -> Result<Proposal, EngineError> {
    let declared = inventory_runner(host, runner).map_err(missing)?;
    block(blockers, check_declared_for_decommission(declared));
    let items = generate_plan(inventory, snapshot, Some(&host.id), Some(runner)).map_err(not_found)?;
    if !items
        .iter()
        .any(|i| i.action == FleetPlanAction::Decommission && !i.blocked)
        && let Some(i) = items.first()
    {
        blockers.push(i.reason.clone());
    }
    Ok(Proposal {
        title: format!("Decommission {runner}"),
        items,
        confirm_phrase: Some(format!("decommission {runner}")),
    })
}

fn review_restart(
    inventory: &Inventory,
    host: &Host,
    runner: &str,
    snapshot: &FleetSnapshot,
    blockers: &mut Vec<String>,
) -> Result<Proposal, EngineError> {
    let (_, declared, role) = inventory.find_runner(runner).map_err(not_found)?;
    block(blockers, check_restartable(runner, declared, role));
    if snapshot.github_error.is_none() {
        block(blockers, check_idle(runner, &snapshot.github));
    }
    let row = build_status_rows(inventory, snapshot, Some(&host.id))
        .map_err(not_found)?
        .into_iter()
        .find(|r| r.runner == runner);
    let items = row
        .map(|r| {
            vec![PlanItem::new(
                FleetPlanAction::Restart,
                &host.id,
                runner,
                format!(
                    "restart the {} service (GitHub: {}, busy: {})",
                    r.service_state.as_deref().unwrap_or(NO_VALUE),
                    r.github_state,
                    r.busy_text()
                ),
            )]
        })
        .unwrap_or_default();
    Ok(Proposal {
        title: format!("Restart {runner}"),
        items,
        confirm_phrase: None,
    })
}
