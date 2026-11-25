//! What the engine keeps between desired-state calls: the cached inventory,
//! the last check, doctor results, pending reviews and the running change.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use chrono::{DateTime, Utc};

use super::super::Engine;
use super::ReviewView;
use super::ssh::SshTransportIdentity;
use crate::desired::inventory::load_inventory_from_env;
use crate::desired::model::{FleetSnapshot, Host, Inventory};
use crate::fleet_plan::{DoctorView, FleetPlanInputs, OperationStatus, OperationView};
use crate::sync::MutexExt;

/// Reviews expire so a stale screen can't be executed much later.
const REVIEW_TTL: chrono::TimeDelta = chrono::TimeDelta::minutes(15);

#[derive(Debug, Default)]
pub struct DesiredStore {
    pub(super) snapshot: Option<Snapshot>,
    pub(super) refreshing: bool,
    pub(super) doctor: Option<DoctorView>,
    pub(super) doctor_running: bool,
    reviews: HashMap<String, Review>,
    pub(super) operation: Option<OperationView>,
    /// An execute is between taking its review and starting its operation.
    pub(super) starting: bool,
    /// SSH target → the hostname SSH resolves it to, from the last check.
    pub(super) ssh_hosts: BTreeMap<String, String>,
    /// The parsed inventory, re-read when the file changes.
    inventory: Option<InventoryCache>,
}

#[derive(Debug)]
struct InventoryCache {
    path: PathBuf,
    modified: Option<std::time::SystemTime>,
    value: Result<Inventory, String>,
}

#[derive(Debug)]
pub(super) struct Snapshot {
    value: FleetSnapshot,
    /// When GitHub was last read, independently of each host probe.
    checked_at: DateTime<Utc>,
    /// When each host was last probed (a host-scoped check updates one host).
    host_checked_at: BTreeMap<String, DateTime<Utc>>,
}

/// A review, and every fact it was built from that execution must find unchanged.
#[derive(Debug, Clone)]
pub(super) struct Review {
    pub(super) view: ReviewView,
    pub(super) created_at: DateTime<Utc>,
    pub(super) inventory_path: PathBuf,
    pub(super) inventory: Inventory,
    pub(super) ssh_hosts: BTreeMap<String, String>,
    pub(super) ssh_transport: Option<SshTransportIdentity>,
    pub(super) identities: BTreeMap<String, RunnerIdentity>,
}

/// A runner's registration and observed directory, which plan text may omit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RunnerIdentity {
    registration: Option<i64>,
    path: Option<String>,
}

/// The identity of every runner on `host`, including legacy runners.
pub(super) fn runner_identities(host: &Host, snapshot: &FleetSnapshot) -> BTreeMap<String, RunnerIdentity> {
    host.runner_names()
        .map(|name| {
            let identity = RunnerIdentity {
                registration: snapshot.github.get(name).map(|runner| runner.id),
                path: snapshot.local.get(name).map(|runner| runner.path.clone()),
            };
            (name.to_string(), identity)
        })
        .collect()
}

impl DesiredStore {
    pub(super) fn change_running(&self) -> bool {
        self.starting
            || self
                .operation
                .as_ref()
                .is_some_and(|o| o.status == OperationStatus::Running)
    }

    /// Forgets everything that was read from or reviewed against the inventory.
    pub(super) fn clear_inventory_facts(&mut self) {
        self.snapshot = None;
        self.doctor = None;
        self.reviews.clear();
    }

    pub(super) fn forget_inventory(&mut self) {
        self.clear_inventory_facts();
        self.inventory = None;
    }

    pub(super) fn add_review(&mut self, review: Review) {
        self.prune_reviews();
        self.reviews.insert(review.view.id.clone(), review);
    }

    /// Removes and returns a review that hasn't expired.
    pub(super) fn take_review(&mut self, id: &str) -> Option<Review> {
        self.prune_reviews();
        self.reviews.remove(id)
    }

    fn prune_reviews(&mut self) {
        let now = Utc::now();
        self.reviews.retain(|_, r| now - r.created_at < REVIEW_TTL);
    }

    /// Records a fresh check of every selected host, or of one host. A
    /// host-scoped check refreshes GitHub fully, and that one host.
    pub(super) fn merge_snapshot(&mut self, inventory: &Inventory, fresh: FleetSnapshot, host: Option<&str>) {
        let now = Utc::now();
        match (&mut self.snapshot, host) {
            (Some(current), Some(host_id)) => {
                let names: Vec<&str> = inventory
                    .find_host(host_id)
                    .map(|h| h.runner_names().collect())
                    .unwrap_or_default();
                current.value.local.retain(|k, _| !names.contains(&k.as_str()));
                current.value.local.extend(fresh.local);
                current.value.host_errors.remove(host_id);
                current.value.host_errors.extend(fresh.host_errors);
                current.value.github = fresh.github;
                current.value.github_error = fresh.github_error;
                current.value.policy_errors = fresh.policy_errors;
                current.checked_at = now;
                current.host_checked_at.insert(host_id.to_string(), now);
            }
            _ => {
                let scoped_hosts: Vec<String> = inventory
                    .selected_hosts(host)
                    .map(|hs| hs.iter().map(|h| h.id.clone()).collect())
                    .unwrap_or_default();
                self.snapshot = Some(Snapshot {
                    value: fresh,
                    checked_at: now,
                    host_checked_at: scoped_hosts.into_iter().map(|h| (h, now)).collect(),
                });
            }
        }
    }
}

impl Engine {
    /// Desired-state inputs for `derive`. The inventory is parsed again only
    /// when the file's modification time changes (a `stat` per view).
    pub(crate) fn plan_inputs(&self) -> FleetPlanInputs {
        let path = self.inventory_path();
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let cached = self
            .desired
            .locked()
            .inventory
            .as_ref()
            .is_some_and(|c| c.path == path && c.modified == modified);
        // Parse outside the lock: views are derived often and the file is edited by hand.
        let loaded = (!cached).then(|| load_inventory_from_env(&path).map_err(|e| e.0));
        // A parse of a file that changed again meanwhile isn't cached; a later view reads it again.
        let current = loaded.is_some() && std::fs::metadata(&path).and_then(|m| m.modified()).ok() == modified;
        let mut store = self.desired.locked();
        let stored = store
            .inventory
            .as_ref()
            .is_some_and(|c| c.path == path && c.modified == modified);
        if let Some(value) = loaded.filter(|_| current && !stored) {
            if store.inventory.as_ref().is_some_and(|cache| cache.value != value) {
                store.clear_inventory_facts();
            }
            store.inventory = Some(InventoryCache {
                path: path.clone(),
                modified,
                value,
            });
        }
        FleetPlanInputs {
            inventory_path: path.display().to_string(),
            inventory: store.inventory.as_ref().map(|c| c.value.clone()),
            snapshot: store.snapshot.as_ref().map(|s| s.value.clone()),
            checked_at: store.snapshot.as_ref().map(|s| s.checked_at),
            host_checked_at: store
                .snapshot
                .as_ref()
                .map(|s| s.host_checked_at.clone())
                .unwrap_or_default(),
            ssh_hosts: store.ssh_hosts.clone(),
            doctor: store.doctor.clone(),
            refreshing: store.refreshing,
            doctor_running: store.doctor_running,
            operation: store.operation.clone(),
        }
    }
}
