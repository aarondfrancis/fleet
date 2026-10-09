//! Typed query validation, stable pagination and unambiguous entity resolution.

use axum::extract::{FromRequestParts, Query};
use axum::http::request::Parts;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::MAX_PAGE_SIZE;
use super::common::ApiError;
use crate::model::{FleetView, MachineConnection, MachineView, RunnerActivity, RunnerView};

pub(super) struct Parameters<T>(pub T);
impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequestParts<S> for Parameters<T> {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|e| ApiError::invalid(e.body_text()))
    }
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct NoQuery {}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) enum MachineScope {
    #[default]
    All,
    Fleet,
    Discovered,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) enum Connection {
    Connected,
    Stale,
    Offline,
    Error,
    Disabled,
    Pending,
}
impl Connection {
    pub fn matches(self, connection: &MachineConnection) -> bool {
        matches!(
            (self, connection),
            (Self::Connected, MachineConnection::Connected { .. })
                | (Self::Stale, MachineConnection::Stale { .. })
                | (Self::Offline, MachineConnection::Offline { .. })
                | (Self::Error, MachineConnection::Error { .. })
                | (Self::Disabled, MachineConnection::Disabled)
                | (Self::Pending, MachineConnection::Pending)
        )
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) enum Activity {
    Busy,
    Idle,
    Offline,
    Unknown,
}
impl Activity {
    pub fn matches(self, activity: &RunnerActivity) -> bool {
        matches!(
            (self, activity),
            (Self::Busy, RunnerActivity::Busy { .. })
                | (Self::Idle, RunnerActivity::Idle)
                | (Self::Offline, RunnerActivity::Offline)
                | (Self::Unknown, RunnerActivity::Unknown)
        )
    }
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct MachineQuery {
    #[serde(default)]
    pub scope: MachineScope,
    pub os: Option<crate::machine::OsFamily>,
    pub connection: Option<Connection>,
    pub q: Option<String>,
    pub limit: Option<u16>,
    pub after: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RunnerQuery {
    pub machine: Option<String>,
    pub org: Option<String>,
    pub activity: Option<Activity>,
    pub label: Option<String>,
    pub repo: Option<String>,
    pub limit: Option<u16>,
    pub after: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct OrgQuery {
    pub org: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct MachineFilter {
    pub machine: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct QueueQuery {
    pub org: Option<String>,
    pub repo: Option<String>,
    pub label: Option<String>,
    pub limit: Option<u16>,
    pub after: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct EventQuery {
    /// Current machine ID/name, resolved to its stable history identity.
    pub machine: Option<String>,
    /// Exact history identity, including machines no longer in the inventory.
    pub machine_key: Option<String>,
    pub before: Option<String>,
    pub limit: Option<u16>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct IssueQuery {
    pub machine: Option<String>,
    pub category: Option<crate::issues::FleetIssueCategory>,
    pub stale: Option<bool>,
    pub muted: Option<bool>,
    pub limit: Option<u16>,
    pub after: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ChangesQuery {
    pub since: Option<String>,
}

pub(super) fn limit(value: Option<u16>, default: usize) -> Result<usize, ApiError> {
    match value.map(usize::from) {
        None => Ok(default),
        Some(n) if (1..=MAX_PAGE_SIZE).contains(&n) => Ok(n),
        Some(_) => Err(ApiError::invalid(format!(
            "limit must be between 1 and {MAX_PAGE_SIZE}."
        ))),
    }
}

pub(super) fn page<T>(
    mut items: Vec<T>,
    limit: usize,
    after: Option<&str>,
    key: impl Fn(&T) -> String,
) -> (Vec<T>, usize, Option<String>) {
    items.sort_by_cached_key(&key);
    let total = items.len();
    items.retain(|item| after.is_none_or(|cursor| key(item).as_str() > cursor));
    let more = items.len() > limit;
    items.truncate(limit);
    let next = more.then(|| items.last().map(key)).flatten();
    (items, total, next)
}

pub(super) fn resolve_machine<'a>(view: &'a FleetView, id: &str) -> Result<&'a MachineView, ApiError> {
    let machines = || view.machines.iter().chain(&view.other_machines);
    if let Some(machine) = machines().find(|m| m.id == id) {
        return Ok(machine);
    }
    let aliases: Vec<_> = machines()
        .filter(|m| m.preferences_key() == id || m.desired.as_ref().is_some_and(|d| d.host.id == id))
        .collect();
    let candidates = if aliases.is_empty() {
        machines()
            .filter(|m| {
                m.display_name.eq_ignore_ascii_case(id)
                    || m.host.eq_ignore_ascii_case(id)
                    || m.tailnet.as_ref().is_some_and(|p| {
                        p.hostname.eq_ignore_ascii_case(id)
                            || p.dns_name.as_ref().is_some_and(|n| n.eq_ignore_ascii_case(id))
                    })
            })
            .collect()
    } else {
        aliases
    };
    match candidates.as_slice() {
        [machine] => Ok(machine),
        [] => Err(ApiError::not_found(format!("No machine {id:?}."))),
        _ => Err(ApiError::ambiguous(
            "machine",
            candidates.iter().map(|m| m.id.clone()).collect(),
        )),
    }
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct FlatRunner {
    pub machine_name: Option<String>,
    #[serde(flatten)]
    pub runner: RunnerView,
}

pub(super) fn flat_runners(view: &FleetView) -> Vec<FlatRunner> {
    view.machines
        .iter()
        .chain(&view.other_machines)
        .flat_map(|m| {
            m.runners.iter().map(|r| FlatRunner {
                machine_name: Some(m.display_name.clone()),
                runner: r.clone(),
            })
        })
        .chain(view.unmatched_runners.iter().map(|r| FlatRunner {
            machine_name: None,
            runner: r.clone(),
        }))
        .collect()
}

pub(super) fn resolve_runner(view: &FleetView, key: &str, org: Option<&str>) -> Result<FlatRunner, ApiError> {
    let runners: Vec<_> = flat_runners(view)
        .into_iter()
        .filter(|r| org.is_none_or(|o| r.runner.org.as_ref().is_some_and(|s| s.eq_ignore_ascii_case(o))))
        .collect();
    if let Some(runner) = runners.iter().find(|r| r.runner.key == key) {
        return Ok(runner.clone());
    }
    let mut candidates: Vec<_> = runners
        .into_iter()
        .filter(|r| r.runner.name.eq_ignore_ascii_case(key))
        .collect();
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => Err(ApiError::not_found(format!("No runner {key:?}."))),
        _ => Err(ApiError::ambiguous(
            "runner",
            candidates.iter().map(|r| r.runner.key.clone()).collect(),
        )),
    }
}

pub(super) fn matches(value: Option<&str>, filter: Option<&str>) -> bool {
    filter.is_none_or(|filter| value.is_some_and(|v| v.eq_ignore_ascii_case(filter)))
}
