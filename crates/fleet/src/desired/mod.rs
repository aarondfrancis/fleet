//! Desired state: Runner Fleet, ported into Runner Dashboard.
//!
//! `inventory/fleet.toml` declares the organization, its trusted runner group,
//! and every host and runner. This module compares it with GitHub and the
//! hosts and carries out the guarded changes the user reviews in the app.
//!
//! - `inventory`: parse + validate (same format and rules as Runner Fleet)
//! - `github`, `remote`, `process`: the `gh`, SSH and subprocess boundaries
//! - `state`, `plan`: pure status rows and plan items from a snapshot
//! - `operations`: apply / retire / decommission / restart, and doctor
//! - `windows_logon`: scripts for runners in a Windows desktop session
//! - `testing`: fakes for the boundaries, for tests (feature `test-support`)

pub mod github;
pub mod inventory;
pub mod model;
pub mod operations;
pub mod plan;
pub mod process;
pub mod py;
pub mod remote;
pub mod state;
pub mod windows_logon;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Duration;

/// The real `gh`, `ssh` and `tailscale` boundaries. GUI apps get a minimal
/// PATH, so the programs are located the same way the rest of the app does.
pub fn real_boundaries(organization: &str, timeout: Duration) -> (github::GithubClient, remote::RemoteClient) {
    let commands: Arc<dyn process::CommandRunner> = Arc::new(process::SubprocessRunner);
    let gh = crate::gh::find()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "gh".into());
    (
        github::GithubClient::new(commands.clone(), organization, &gh, timeout),
        remote::RemoteClient::new(commands, timeout, remote::Programs::installed()),
    )
}

/// Runner Fleet's `--json` shapes (snake_case keys), for comparing this port
/// with the original CLI.
pub mod parity {
    use serde_json::{Value, json};

    use super::model::{DoctorCheck, Inventory, NO_VALUE, PlanItem, StatusRow};

    pub fn inventory(inventory: &Inventory) -> Value {
        json!({
            "organization": inventory.organization.name,
            "runner_group": inventory.organization.runner_group,
            "hosts": inventory.hosts.iter().map(|h| json!({
                "id": h.id,
                "platform": h.platform,
                "ssh": h.ssh_target.clone().unwrap_or_else(|| h.ssh_setting.clone()),
                "ssh_transport": h.ssh_transport,
                "root": h.root,
                "service_mode": h.service_mode,
                "runners": h.runners.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }

    pub fn status(rows: &[StatusRow]) -> Value {
        Value::Array(
            rows.iter()
                .map(|r| {
                    json!({
                        "runner": r.runner, "host": r.host.as_deref().unwrap_or(NO_VALUE),
                        "github_state": r.github_state,
                        "service_state": r.service_state.as_deref().unwrap_or(NO_VALUE),
                        "busy": r.busy_text(), "version": r.version.as_deref().unwrap_or(NO_VALUE),
                        "drift": r.drift, "desired": r.desired,
                    })
                })
                .collect(),
        )
    }

    pub fn plan(items: &[PlanItem]) -> Value {
        Value::Array(
            items
                .iter()
                .map(|i| {
                    json!({
                        "action": i.action, "host": i.host, "runner": i.runner,
                        "reason": i.reason, "command": i.command, "blocked": i.blocked,
                    })
                })
                .collect(),
        )
    }

    /// Doctor checks serialize with the original's keys.
    pub fn doctor(checks: &[DoctorCheck]) -> Value {
        json!(checks)
    }
}
