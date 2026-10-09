//! Fictional history for previews. Never reads or writes the live journal.

use super::{FleetAlertKind, FleetEventKind as Kind, FleetMachineAlert, FleetMachineEvent};
use crate::model::{FleetView, inventory_machine_key};
use chrono::{DateTime, Duration, Utc};

pub(crate) fn attach(view: &mut FleetView, now: DateTime<Utc>) {
    for machine in view.machines.iter_mut().chain(&mut view.other_machines) {
        let key = machine.preferences_key();
        machine.monitoring.machine_key.clone_from(&key);
        let mut add = |minutes, kind, detail: String| {
            view.monitoring.events.push(FleetMachineEvent {
                id: String::new(),
                machine_key: key.clone(),
                machine_name: machine.display_name.clone(),
                at: now - Duration::minutes(minutes),
                kind,
                detail,
            });
        };
        add(
            120,
            Kind::MonitoringStarted,
            "Started observing this machine. Earlier events are not known.".into(),
        );
        if key == inventory_machine_key("studio") {
            add(
                38,
                Kind::Rebooted,
                "A different operating-system boot session was observed.".into(),
            );
            add(37, Kind::RunnerServiceChanged, "studio-1: inactive → running.".into());
        }
        if key == inventory_machine_key("mac-mini-02") {
            let detail = "/: 5.7% free (13.0 GiB of 228.2 GiB).".to_owned();
            add(12, Kind::LowDisk, detail.clone());
            let alert = FleetMachineAlert {
                id: format!("{key}:disk"),
                machine_key: key,
                machine_name: machine.display_name.clone(),
                kind: FleetAlertKind::LowDisk,
                since: now - Duration::minutes(13),
                detail,
                stale: false,
                muted: false,
            };
            machine.monitoring.alerts.push(alert.clone());
            view.monitoring.alerts.push(alert);
        }
    }
    // Match the monotonic cursor order used by persisted live history.
    view.monitoring.events.sort_by_key(|e| e.at);
    for (index, event) in view.monitoring.events.iter_mut().enumerate() {
        event.id = format!("{:020}", index + 1);
    }
    view.monitoring.events.reverse();
    for machine in view.machines.iter_mut().chain(&mut view.other_machines) {
        machine.monitoring.events = view
            .monitoring
            .events
            .iter()
            .filter(|e| e.machine_key == machine.monitoring.machine_key)
            .cloned()
            .collect();
    }
}
