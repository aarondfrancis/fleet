//! Local history and notification preferences, independent of remote operations.

use super::{Engine, EngineError};
use crate::local_api::MAX_PAGE_SIZE;
use crate::model::{DataSource, FleetView};
use crate::monitoring::FleetMachineEvent;
use crate::sync::MutexExt;
use chrono::{DateTime, Duration, Utc};

impl Engine {
    /// Record transitions and return durable notification requests due for delivery or retry,
    /// with the fleet view they were evaluated on, including the alerts this round changed.
    /// Performs local disk I/O; call off the async runtime.
    pub fn record_monitoring(&self, now: DateTime<Utc>) -> (Vec<FleetMachineEvent>, FleetView) {
        let mut view = self.view();
        let mut monitoring = self.monitoring.locked();
        // Read the preference under the same lock as preference changes and outbox updates.
        view.monitoring.desktop_notifications = self.config().desktop_notifications;
        let events = monitoring.record(&view, now);
        if view.source == DataSource::Live {
            let (at, notifications) = (view.generated_at, view.monitoring.desktop_notifications);
            monitoring.attach(&mut view, at, notifications);
            crate::issues::attach(&mut view);
        }
        (events, view)
    }

    /// Persist native API request acceptance; this does not confirm OS display.
    /// Performs local disk I/O; call off the async runtime.
    pub fn acknowledge_notifications(&self, events: &[FleetMachineEvent]) -> Result<(), EngineError> {
        self.monitoring
            .locked()
            .acknowledge_notifications(events)
            .map_err(EngineError::Storage)
    }

    /// Read one bounded history page, with an exclusive, stable event cursor.
    pub fn monitoring_events(&self, machine_key: Option<&str>, before: Option<&str>) -> Vec<FleetMachineEvent> {
        if self.config().demo_mode {
            return Vec::new();
        }
        self.monitoring.locked().events(machine_key, before, Utc::now())
    }

    /// Bounded journal access for HTTP history; the caller handles demo snapshots separately.
    pub(crate) fn monitoring_event_page(
        &self,
        machine_key: Option<&str>,
        before: Option<&str>,
        limit: usize,
    ) -> Vec<FleetMachineEvent> {
        self.monitoring
            .locked()
            .event_page(machine_key, before, limit.min(MAX_PAGE_SIZE + 1), Utc::now())
    }

    /// Pause desktop notifications for a machine for an hour; history continues.
    pub fn set_machine_maintenance(&self, id: &str, paused: bool) -> Result<(), EngineError> {
        if self.config().demo_mode {
            return Err(EngineError::Invalid(
                "Turn off demo data to change alert settings.".into(),
            ));
        }
        let view = self.view();
        let machine = view
            .machines
            .iter()
            .chain(&view.other_machines)
            .find(|m| m.id == id)
            .ok_or_else(|| EngineError::NotFound("This machine is no longer listed. Refresh and try again.".into()))?;
        let now = Utc::now();
        self.monitoring
            .locked()
            .maintenance(machine, paused.then_some(now + Duration::hours(1)), now)
            .map_err(EngineError::Storage)
    }

    /// Enable or disable native alert notifications without changing in-app alerts.
    pub fn set_desktop_notifications(&self, enabled: bool) -> Result<(), EngineError> {
        let mut monitoring = self.monitoring.locked();
        if enabled && !self.config().desktop_notifications {
            // A previous purge may have failed: do not enable until it is durable.
            monitoring.discard_notifications().map_err(EngineError::Storage)?;
        }
        self.update_config(|c| c.desktop_notifications = enabled)?;
        if !enabled {
            // The saved preference already disables delivery. Purge failures remain visible
            // in monitoring.storage_error and must be resolved before enabling again.
            let _ = monitoring.discard_notifications();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MemorySecrets;
    use crate::monitoring::FleetEventKind;
    use std::sync::Arc;

    fn pending_engine() -> (tempfile::TempDir, Engine) {
        let dir = tempfile::tempdir().unwrap();
        let event = FleetMachineEvent {
            id: "1".into(),
            machine_key: "inv:fake-host".into(),
            machine_name: "fake-host".into(),
            at: Utc::now(),
            kind: FleetEventKind::LowDisk,
            detail: "Low free space.".into(),
        };
        let journal = serde_json::json!({
            "version": 1, "next_id": 2, "machines": {}, "events": [event],
            "pending_notifications": [event], "notification_retry_at": Utc::now() + Duration::minutes(1),
        });
        std::fs::write(
            dir.path().join("monitoring.json"),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        let engine = Engine::load(dir.path().join("config.json"), Arc::new(MemorySecrets::default())).unwrap();
        (dir, engine)
    }

    #[test]
    fn enabling_discards_previous_disabled_period_requests_without_a_poll() {
        let (dir, engine) = pending_engine();
        assert!(!engine.config().desktop_notifications);
        engine.set_desktop_notifications(true).unwrap();
        assert!(engine.config().desktop_notifications);
        let journal: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("monitoring.json")).unwrap()).unwrap();
        assert_eq!(journal["pending_notifications"], serde_json::json!([]));
        assert!(journal["notification_retry_at"].is_null());
    }

    #[test]
    fn startup_monitoring_preserves_outbox_before_discovery() {
        let (dir, engine) = pending_engine();
        engine.update_config(|c| c.desktop_notifications = true).unwrap();
        engine
            .set_inventory_path(Some(dir.path().join("not-yet-available.toml").display().to_string()))
            .unwrap();
        let path = dir.path().join("monitoring.json");
        let before: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let view = engine.view();
        assert!(view.machines.is_empty());
        assert!(view.tailnet.as_of.is_none());
        assert!(engine.record_monitoring(view.generated_at).0.is_empty());
        let after: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(after["pending_notifications"], before["pending_notifications"]);
    }

    #[test]
    fn failed_disable_purge_keeps_notifications_disabled_until_durable_reenable() {
        let (dir, engine) = pending_engine();
        // Keep the seeded queue while modeling an enabled preference.
        engine.update_config(|c| c.desktop_notifications = true).unwrap();
        let path = dir.path().join("monitoring.json");
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        engine.set_desktop_notifications(false).unwrap();
        assert!(!engine.config().desktop_notifications);
        assert!(engine.view().monitoring.storage_error.is_some());
        assert!(engine.set_desktop_notifications(true).is_err());
        assert!(!engine.config().desktop_notifications);
        std::fs::remove_dir(&path).unwrap();
        engine.set_desktop_notifications(true).unwrap();
        let journal: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(journal["pending_notifications"], serde_json::json!([]));
    }
}
