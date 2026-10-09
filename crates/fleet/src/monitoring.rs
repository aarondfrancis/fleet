//! Local event history and edge-triggered alerts. Remote machines are never changed.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub(crate) mod demo;
mod state;
mod store;
pub(crate) use store::{EVENT_PAGE_SIZE, EVENT_RETENTION, MAX_EVENTS, MonitorStore};

/// Events retained on this Mac for up to 30 days (at most 5,000 entries).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum FleetEventKind {
    MonitoringStarted,
    TargetChanged,
    Online,
    Offline,
    SshUnavailable,
    SshRecovered,
    Rebooted,
    RunnerServiceChanged,
    OfflineAlert,
    LowDisk,
    DiskRecovered,
    MaintenanceStarted,
    MaintenanceEnded,
    AlertsResumed,
}

/// A timestamped observation, not a claim about events between polling samples.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FleetMachineEvent {
    pub id: String,
    pub machine_key: String,
    pub machine_name: String,
    pub at: DateTime<Utc>,
    pub kind: FleetEventKind,
    pub detail: String,
}

/// Alert categories supported by automatic monitoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum FleetAlertKind {
    Offline,
    LowDisk,
}

/// An unresolved condition; missing data never implies recovery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FleetMachineAlert {
    pub id: String,
    pub machine_key: String,
    pub machine_name: String,
    pub kind: FleetAlertKind,
    pub since: DateTime<Utc>,
    pub detail: String,
    pub stale: bool,
    pub muted: bool,
}

/// Machine-level history, alerts and a temporary notification pause.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FleetMachineMonitoring {
    pub machine_key: String,
    pub maintenance_until: Option<DateTime<Utc>>,
    pub alerts: Vec<FleetMachineAlert>,
    /// Most recent 50 events, newest first. Older events can be paged from history.
    pub events: Vec<FleetMachineEvent>,
}

/// Fleet-wide history and the health of this Mac's history storage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FleetMonitoring {
    pub desktop_notifications: bool,
    pub storage_error: Option<String>,
    pub alerts: Vec<FleetMachineAlert>,
    pub events: Vec<FleetMachineEvent>,
}

#[cfg(test)]
mod tests;
