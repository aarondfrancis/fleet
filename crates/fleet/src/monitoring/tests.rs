use super::*;
use crate::model::{DataSource, FleetView, MachineConnection};
use chrono::{Duration, TimeZone};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 8, 12, 0, 0).unwrap()
}
fn view(at: DateTime<Utc>) -> FleetView {
    let (config, obs) = crate::demo::dataset(at);
    let mut view = crate::derive::derive(
        &config,
        &obs,
        &crate::demo_desired::plan_inputs(at),
        at,
        DataSource::Live,
    );
    view.machines.retain(|m| m.display_name == "studio");
    view.other_machines.clear();
    sample(&mut view, at);
    view.monitoring.desktop_notifications = true;
    view
}
fn sample(view: &mut FleetView, at: DateTime<Utc>) {
    view.generated_at = at;
    view.tailnet.as_of = Some(at);
    view.tailnet.local_online = Some(true);
    view.tailnet.error = None;
    let m = &mut view.machines[0];
    if m.tailnet.as_ref().unwrap().online {
        m.connection = MachineConnection::Connected { checked_at: at };
    }
    if let Some(health) = &mut m.health {
        health.as_of = at;
        health.stale = !m.tailnet.as_ref().unwrap().online;
    }
}
fn offline(view: &mut FleetView) {
    view.machines[0].tailnet.as_mut().unwrap().online = false;
    view.machines[0].connection = MachineConnection::Offline { last_seen: None };
    view.machines[0].health.as_mut().unwrap().stale = true;
}
fn attach(store: &MonitorStore, v: &mut FleetView) {
    let at = v.generated_at;
    store.attach(v, at, true);
}
fn store() -> (tempfile::TempDir, MonitorStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = MonitorStore::load(dir.path().join("monitoring.json"));
    (dir, store)
}

#[test]
fn offline_alert_is_debounced_once_and_recovers_once_across_restart() {
    let (dir, mut store) = store();
    let mut v = view(now());
    assert!(store.deliver(&v, now()).is_empty());
    offline(&mut v);
    let mut notices = Vec::new();
    for tick in 1..=15 {
        let at = now() + Duration::seconds(tick * 15);
        sample(&mut v, at);
        notices.extend(store.deliver(&v, at));
    }
    assert_eq!(
        notices.iter().map(|e| &e.kind).collect::<Vec<_>>(),
        [&FleetEventKind::OfflineAlert]
    );
    let mut store = MonitorStore::load(dir.path().join("monitoring.json"));
    sample(&mut v, now() + Duration::minutes(5));
    assert!(store.deliver(&v, v.generated_at).is_empty());
    attach(&store, &mut v);
    assert_eq!(v.monitoring.alerts.len(), 1);
    v.machines[0].tailnet.as_mut().unwrap().online = true;
    sample(&mut v, now() + Duration::minutes(6));
    let recovery = store.deliver(&v, v.generated_at);
    assert_eq!(
        recovery.iter().map(|e| &e.kind).collect::<Vec<_>>(),
        [&FleetEventKind::Online]
    );
    attach(&store, &mut v);
    assert!(v.monitoring.alerts.is_empty());
    assert!(store.deliver(&v, v.generated_at).is_empty());
}

#[test]
fn local_tailnet_outage_and_observation_gaps_cannot_accrue_offline_time() {
    let (_dir, mut store) = store();
    let mut v = view(now());
    store.deliver(&v, now());
    offline(&mut v);
    for tick in 1..=20 {
        let at = now() + Duration::seconds(tick * 15);
        sample(&mut v, at);
        if (3..=18).contains(&tick) {
            v.tailnet.local_online = Some(false);
        }
        assert!(store.deliver(&v, at).is_empty());
    }
    // A stale cached peer never advances the alert clock.
    v.tailnet.as_of = Some(now());
    assert!(store.deliver(&v, now() + Duration::minutes(8)).is_empty());
    sample(&mut v, now() + Duration::minutes(9));
    assert!(store.deliver(&v, v.generated_at).is_empty());
}

#[test]
fn disk_alert_requires_new_samples_and_recovers_only_with_margin() {
    let (_dir, mut store) = store();
    let mut v = view(now());
    store.deliver(&v, now());
    let disk = &mut v.machines[0].health.as_mut().unwrap().disks[0];
    disk.total_bytes = 1000;
    disk.available_bytes = 90;
    sample(&mut v, now() + Duration::minutes(1));
    assert!(store.deliver(&v, v.generated_at).is_empty());
    assert!(store.deliver(&v, now() + Duration::seconds(119)).is_empty());
    sample(&mut v, now() + Duration::minutes(2));
    assert_eq!(store.deliver(&v, v.generated_at)[0].kind, FleetEventKind::LowDisk);
    v.machines[0].health.as_mut().unwrap().disks[0].available_bytes = 110;
    sample(&mut v, now() + Duration::minutes(3));
    assert!(store.deliver(&v, v.generated_at).is_empty());
    attach(&store, &mut v);
    assert_eq!(v.monitoring.alerts.len(), 1);
    let disk = v.machines[0].health.as_mut().unwrap().disks.pop().unwrap();
    sample(&mut v, now() + Duration::minutes(4));
    assert!(store.deliver(&v, v.generated_at).is_empty());
    attach(&store, &mut v);
    assert!(v.monitoring.alerts[0].stale);
    v.machines[0]
        .health
        .as_mut()
        .unwrap()
        .disks
        .push(crate::machine::DiskStats {
            available_bytes: 120,
            ..disk
        });
    sample(&mut v, now() + Duration::minutes(5));
    assert_eq!(store.deliver(&v, v.generated_at)[0].kind, FleetEventKind::DiskRecovered);
    attach(&store, &mut v);
    assert!(v.monitoring.alerts.is_empty());
}

#[test]
fn maintenance_keeps_history_and_notifies_still_active_alert_once_when_resumed() {
    let (dir, mut store) = store();
    let mut v = view(now());
    store.deliver(&v, now());
    store
        .maintenance(&v.machines[0], Some(now() + Duration::hours(1)), now())
        .unwrap();
    offline(&mut v);
    for tick in 1..=15 {
        sample(&mut v, now() + Duration::seconds(tick * 15));
        assert!(store.deliver(&v, v.generated_at).is_empty());
    }
    let mut store = MonitorStore::load(dir.path().join("monitoring.json"));
    attach(&store, &mut v);
    assert!(v.monitoring.alerts[0].muted);
    store.maintenance(&v.machines[0], None, v.generated_at).unwrap();
    sample(&mut v, now() + Duration::minutes(4));
    assert_eq!(store.deliver(&v, v.generated_at)[0].kind, FleetEventKind::AlertsResumed);
    sample(&mut v, now() + Duration::seconds(255));
    assert!(store.deliver(&v, v.generated_at).is_empty());
}

#[test]
fn boots_and_services_use_observed_changes_not_uptime_jitter_or_missing_data() {
    let (_dir, mut store) = store();
    let mut v = view(now());
    store.deliver(&v, now());
    v.machines[0].health.as_mut().unwrap().uptime_secs = Some(0);
    sample(&mut v, now() + Duration::minutes(1));
    store.deliver(&v, v.generated_at);
    assert_eq!(store.events(None, None, v.generated_at).len(), 1);
    v.machines[0].health.as_mut().unwrap().boot_id = Some("new-boot".into());
    v.machines[0].runners[0].local.as_mut().unwrap().service_state = "inactive".into();
    sample(&mut v, now() + Duration::minutes(2));
    store.deliver(&v, v.generated_at);
    let events = store.events(None, None, v.generated_at);
    assert_eq!(
        events.iter().map(|e| &e.kind).collect::<Vec<_>>(),
        [
            &FleetEventKind::RunnerServiceChanged,
            &FleetEventKind::Rebooted,
            &FleetEventKind::MonitoringStarted
        ]
    );
    v.machines[0].runners[0].local.as_mut().unwrap().service_state = "unknown".into();
    sample(&mut v, now() + Duration::minutes(3));
    store.deliver(&v, v.generated_at);
    assert_eq!(store.events(None, None, v.generated_at), events);
}

#[test]
fn demo_disabled_hosts_and_target_changes_do_not_inherit_alerts() {
    let (_dir, mut store) = store();
    let mut v = view(now());
    v.source = DataSource::Demo;
    store.deliver(&v, now());
    assert!(store.events(None, None, now()).is_empty());
    v.source = DataSource::Live;
    v.machines[0].connection = MachineConnection::Disabled;
    store.deliver(&v, now());
    assert!(store.events(None, None, now()).is_empty());
    sample(&mut v, now());
    store.deliver(&v, now());
    v.machines[0].desired.as_mut().unwrap().host.ssh_target = Some("different-host".into());
    sample(&mut v, now() + Duration::minutes(1));
    store.deliver(&v, v.generated_at);
    assert_eq!(
        store.events(None, None, v.generated_at)[0].kind,
        FleetEventKind::TargetChanged
    );
}

#[test]
fn history_is_paged_retained_and_corruption_is_not_overwritten() {
    let (dir, mut store) = store();
    let mut v = view(now());
    store.deliver(&v, now());
    for tick in 1..=60 {
        sample(&mut v, now() + Duration::seconds(tick * 15));
        v.machines[0].tailnet.as_mut().unwrap().online = tick % 2 == 0;
        store.deliver(&v, v.generated_at);
    }
    let first = store.events(None, None, v.generated_at);
    assert_eq!(first.len(), 50);
    let second = store.events(None, Some(&first.last().unwrap().id), v.generated_at);
    assert_eq!(second.len(), 11);
    assert!(first.iter().all(|e| second.iter().all(|b| e.id != b.id)));
    assert!(store.events(None, None, now() + Duration::days(31)).is_empty());
    let path = dir.path().join("broken.json");
    std::fs::write(&path, "broken").unwrap();
    let mut broken = MonitorStore::load(path.clone());
    assert!(broken.deliver(&v, v.generated_at).is_empty());
    assert!(broken.storage_error.is_some());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "broken");
}

#[test]
fn failed_writes_hold_notifications_and_roll_back_maintenance_until_storage_recovers() {
    let (dir, mut store) = store();
    let mut v = view(now());
    store.deliver(&v, now());
    let path = dir.path().join("monitoring.json");
    // A directory at the destination makes atomic replacement fail on every platform.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        store
            .maintenance(&v.machines[0], Some(now() + Duration::hours(1)), now())
            .is_err()
    );
    attach(&store, &mut v);
    assert!(v.machines[0].monitoring.maintenance_until.is_none());
    offline(&mut v);
    for tick in 1..=15 {
        sample(&mut v, now() + Duration::seconds(tick * 15));
        assert!(store.deliver(&v, v.generated_at).is_empty());
    }
    assert!(store.storage_error.is_some());
    std::fs::remove_dir(&path).unwrap();
    let notices = store.deliver(&v, v.generated_at);
    assert_eq!(
        notices.iter().map(|e| &e.kind).collect::<Vec<_>>(),
        [&FleetEventKind::OfflineAlert]
    );
    assert!(store.storage_error.is_none());
    let mut reloaded = MonitorStore::load(path);
    assert!(reloaded.deliver(&v, v.generated_at).is_empty());
}

#[test]
fn adding_a_discovered_machine_to_inventory_preserves_its_history() {
    let (_dir, mut store) = store();
    let mut v = view(now());
    let desired = v.machines[0].desired.take();
    store.deliver(&v, now());
    let first_id = store.events(None, None, now())[0].id.clone();
    v.machines[0].desired = desired;
    sample(&mut v, now() + Duration::minutes(1));
    store.deliver(&v, v.generated_at);
    attach(&store, &mut v);
    let machine = &v.machines[0];
    assert!(machine.monitoring.machine_key.starts_with("inv:"));
    assert!(machine.monitoring.events.iter().any(|e| e.id == first_id));
    assert!(
        machine
            .monitoring
            .events
            .iter()
            .all(|e| e.machine_key == machine.preferences_key())
    );
}

#[test]
fn maintenance_expiry_announces_an_unresolved_disk_alert_even_with_partial_recovery() {
    let (_dir, mut store) = store();
    let mut v = view(now());
    store.deliver(&v, now());
    store
        .maintenance(&v.machines[0], Some(now() + Duration::hours(1)), now())
        .unwrap();
    let disk = &mut v.machines[0].health.as_mut().unwrap().disks[0];
    disk.total_bytes = 1000;
    disk.available_bytes = 90;
    for minute in 1..=2 {
        sample(&mut v, now() + Duration::minutes(minute));
        assert!(store.deliver(&v, v.generated_at).is_empty());
    }
    v.machines[0].health.as_mut().unwrap().disks[0].available_bytes = 110;
    sample(&mut v, now() + Duration::hours(1));
    assert_eq!(store.deliver(&v, v.generated_at)[0].kind, FleetEventKind::AlertsResumed);
    attach(&store, &mut v);
    assert!(v.machines[0].monitoring.maintenance_until.is_none());
    assert!(!v.monitoring.alerts[0].muted);
    assert!(
        v.monitoring
            .events
            .iter()
            .any(|e| e.kind == FleetEventKind::MaintenanceEnded)
    );
    sample(&mut v, now() + Duration::minutes(61));
    assert!(store.deliver(&v, v.generated_at).is_empty());
}

#[test]
fn disk_alerts_group_shared_capacity_and_keep_distinct_drives_in_one_machine_alert() {
    let (_dir, mut store) = store();
    let mut v = view(now());
    store.deliver(&v, now());
    v.machines[0].health.as_mut().unwrap().disks = [("/", 90), ("/System/Volumes/Data", 90), ("/Volumes/Builds", 40)]
        .into_iter()
        .map(|(mount, free)| crate::machine::DiskStats {
            mount_point: mount.into(),
            total_bytes: 1000,
            available_bytes: free,
        })
        .collect();
    for minute in 1..=2 {
        sample(&mut v, now() + Duration::minutes(minute));
        store.deliver(&v, v.generated_at);
    }
    attach(&store, &mut v);
    assert_eq!(v.monitoring.alerts.len(), 1);
    let detail = &v.monitoring.alerts[0].detail;
    assert_eq!(detail.matches("9.0% free").count(), 1);
    assert!(detail.contains("/, /System/Volumes/Data:"));
    assert!(detail.contains("/Volumes/Builds: 4.0% free"));
}

// Most observation tests model a native API that accepts every request.
impl MonitorStore {
    fn deliver(&mut self, view: &FleetView, at: DateTime<Utc>) -> Vec<FleetMachineEvent> {
        let events = self.record(view, at);
        if !events.is_empty() {
            self.acknowledge_notifications(&events).unwrap();
        }
        events
    }
}

fn disk_notice(store: &mut MonitorStore, v: &mut FleetView) -> Vec<FleetMachineEvent> {
    store.record(v, now());
    let disk = &mut v.machines[0].health.as_mut().unwrap().disks[0];
    disk.total_bytes = 1000;
    disk.available_bytes = 90;
    sample(v, now() + Duration::minutes(1));
    assert!(store.record(v, v.generated_at).is_empty());
    sample(v, now() + Duration::minutes(2));
    let events = store.record(v, v.generated_at);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, FleetEventKind::LowDisk);
    events
}

#[test]
fn failed_native_request_retries_after_restart_until_accepted() {
    let (dir, mut store) = store();
    let mut v = view(now());
    let events = disk_notice(&mut store, &mut v);
    // Fake native API: failures leave the batch unacknowledged.
    let mut requests = Vec::new();
    let mut send = |store: &mut MonitorStore, batch: &[FleetMachineEvent], result: Result<(), String>| {
        requests.push(batch.to_vec());
        result?;
        store.acknowledge_notifications(batch)
    };
    assert!(send(&mut store, &events, Err("unavailable".into())).is_err());
    drop(store);
    let mut store = MonitorStore::load(dir.path().join("monitoring.json"));
    sample(&mut v, now() + Duration::seconds(179));
    assert!(store.record(&v, v.generated_at).is_empty());
    sample(&mut v, now() + Duration::minutes(3));
    let retry = store.record(&v, v.generated_at);
    assert_eq!(retry, events);
    send(&mut store, &retry, Ok(())).unwrap();
    assert_eq!(requests, [events.clone(), events]);
    drop(store);
    let mut store = MonitorStore::load(dir.path().join("monitoring.json"));
    sample(&mut v, now() + Duration::minutes(4));
    assert!(store.record(&v, v.generated_at).is_empty());
    v.machines[0].health.as_mut().unwrap().disks[0].available_bytes = 120;
    sample(&mut v, now() + Duration::minutes(5));
    let recovery = store.record(&v, v.generated_at);
    assert_eq!(recovery[0].kind, FleetEventKind::DiskRecovered);
    store.acknowledge_notifications(&recovery).unwrap();
    assert!(store.record(&v, v.generated_at).is_empty());
}

#[test]
fn acknowledgement_failure_preserves_outbox_and_acknowledges_only_requested_ids() {
    let (dir, mut store) = store();
    let mut v = view(now());
    let events = disk_notice(&mut store, &mut v);
    let path = dir.path().join("monitoring.json");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(store.acknowledge_notifications(&events).is_err());
    assert!(store.storage_error.is_some());
    std::fs::remove_dir(&path).unwrap();
    v.machines[0].health.as_mut().unwrap().disks[0].available_bytes = 120;
    sample(&mut v, now() + Duration::minutes(3));
    let both = store.record(&v, v.generated_at);
    assert_eq!(both.len(), 2);
    store.acknowledge_notifications(&events).unwrap();
    drop(store);
    let mut store = MonitorStore::load(path);
    sample(&mut v, now() + Duration::minutes(4));
    assert_eq!(store.record(&v, v.generated_at), both[1..]);
}

#[test]
fn disabling_purges_failed_requests_and_does_not_replay_disabled_period() {
    let (dir, mut store) = store();
    let mut v = view(now());
    disk_notice(&mut store, &mut v);
    v.monitoring.desktop_notifications = false;
    v.machines[0].health.as_mut().unwrap().disks[0].available_bytes = 120;
    sample(&mut v, now() + Duration::minutes(3));
    assert!(store.record(&v, v.generated_at).is_empty());
    drop(store);
    let mut store = MonitorStore::load(dir.path().join("monitoring.json"));
    v.monitoring.desktop_notifications = true;
    sample(&mut v, now() + Duration::minutes(4));
    assert!(store.record(&v, v.generated_at).is_empty());
    assert!(
        store
            .events(None, None, v.generated_at)
            .iter()
            .any(|e| e.kind == FleetEventKind::DiskRecovered)
    );
}

#[test]
fn old_history_without_outbox_fields_is_readable() {
    let (dir, mut store) = store();
    let v = view(now());
    store.record(&v, now());
    let path = dir.path().join("monitoring.json");
    let mut journal: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    journal.as_object_mut().unwrap().remove("pending_notifications");
    journal.as_object_mut().unwrap().remove("notification_retry_at");
    std::fs::write(&path, serde_json::to_vec(&journal).unwrap()).unwrap();
    let store = MonitorStore::load(path);
    assert!(store.storage_error.is_none());
    assert_eq!(
        store.events(None, None, now())[0].kind,
        FleetEventKind::MonitoringStarted
    );
}

#[test]
fn disabled_and_removed_machines_purge_outbox_before_retry_is_due() {
    for removed in [false, true] {
        let (dir, mut store) = store();
        let mut v = view(now());
        disk_notice(&mut store, &mut v);
        // Five seconds after the request: neither retry nor checkpoint would be due.
        let at = now() + Duration::seconds(125);
        sample(&mut v, at);
        if removed {
            v.machines.clear();
        } else {
            v.machines[0].connection = MachineConnection::Disabled;
        }
        assert!(store.record(&v, at).is_empty());
        drop(store);
        let mut store = MonitorStore::load(dir.path().join("monitoring.json"));
        let mut v = view(now() + Duration::minutes(3));
        v.machines[0].health.as_mut().unwrap().disks[0].total_bytes = 1000;
        v.machines[0].health.as_mut().unwrap().disks[0].available_bytes = 90;
        assert!(store.record(&v, v.generated_at).is_empty());
        sample(&mut v, now() + Duration::minutes(4));
        assert!(store.record(&v, v.generated_at).is_empty());
    }
}

#[test]
fn persisted_notifications_survive_incomplete_discovery_and_inventory() {
    for missing_evidence in [
        "pending",
        "failed",
        "stale",
        "offline",
        "inventory",
        "inventory-pending",
    ] {
        let (dir, mut store) = store();
        let mut v = view(now());
        // A discovered peer has no inventory entry to keep it visible during startup.
        v.machines[0].desired = None;
        let notice = disk_notice(&mut store, &mut v).pop().unwrap();
        let path = dir.path().join("monitoring.json");
        drop(store);
        let mut store = MonitorStore::load(path.clone());
        let mut absent = v.clone();
        absent.machines.clear();
        absent.other_machines.clear();
        absent.generated_at = now() + Duration::minutes(3);
        match missing_evidence {
            "pending" => {
                absent.tailnet.as_of = None;
                absent.tailnet.available = false;
            }
            "failed" => {
                absent.tailnet.error = Some("Discovery failed.".into());
            }
            "stale" => {}
            "offline" => {
                absent.tailnet.as_of = Some(absent.generated_at);
                absent.tailnet.local_online = Some(false);
            }
            "inventory" => {
                absent.tailnet.as_of = Some(absent.generated_at);
                absent.plan.inventory_error = Some("Inventory unavailable.".into());
            }
            "inventory-pending" => {
                absent.tailnet.as_of = Some(absent.generated_at);
                absent.plan.organization = None;
            }
            _ => unreachable!(),
        }
        assert!(store.record(&absent, absent.generated_at).is_empty());
        let journal: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            journal["pending_notifications"],
            serde_json::json!([notice]),
            "{missing_evidence}"
        );
        drop(store);
        let mut store = MonitorStore::load(path);
        sample(&mut v, now() + Duration::minutes(4));
        assert_eq!(store.record(&v, v.generated_at), vec![notice], "{missing_evidence}");
    }
}

#[test]
fn persisted_outbox_keeps_only_the_newest_256_unexpired_requests() {
    let (dir, mut store) = store();
    let mut v = view(now());
    let event = disk_notice(&mut store, &mut v).pop().unwrap();
    let path = dir.path().join("monitoring.json");
    let mut journal: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let pending: Vec<_> = (0..300)
        .map(|id| FleetMachineEvent {
            id: format!("queued-{id:03}"),
            at: if id == 0 { now() - Duration::days(31) } else { event.at },
            ..event.clone()
        })
        .collect();
    journal["pending_notifications"] = serde_json::to_value(pending).unwrap();
    std::fs::write(&path, serde_json::to_vec(&journal).unwrap()).unwrap();
    let mut store = MonitorStore::load(path.clone());
    sample(&mut v, now() + Duration::minutes(3));
    let batch = store.record(&v, v.generated_at);
    assert_eq!(batch.len(), 256);
    assert_eq!(batch.first().unwrap().id, "queued-044");
    assert_eq!(batch.last().unwrap().id, "queued-299");
    let journal: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(journal["pending_notifications"].as_array().unwrap().len(), 256);
    sample(&mut v, now() + Duration::days(31));
    assert!(store.record(&v, v.generated_at).is_empty());
    let journal: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(journal["pending_notifications"], serde_json::json!([]));
}

#[test]
fn failed_record_purge_is_saved_before_reenabling_even_when_memory_queue_is_empty() {
    let (dir, mut store) = store();
    let mut v = view(now());
    disk_notice(&mut store, &mut v);
    let path = dir.path().join("monitoring.json");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    v.monitoring.desktop_notifications = false;
    sample(&mut v, now() + Duration::seconds(125));
    assert!(store.record(&v, v.generated_at).is_empty());
    assert!(store.storage_error.is_some());
    assert!(store.discard_notifications().is_err());
    std::fs::remove_dir(&path).unwrap();
    store.discard_notifications().unwrap();
    let journal: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(journal["pending_notifications"], serde_json::json!([]));
}
