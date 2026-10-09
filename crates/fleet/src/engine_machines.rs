//! Bounded SSH polling, keeping the last successful read beside failures.

use std::sync::Arc;

use chrono::Utc;
use futures::{StreamExt, stream};

use super::{Engine, EngineError};
use crate::machine::{MachineSource, MachineTarget};
use crate::model::{MachineObservation, MachinePreferences, Timed, inventory_machine_key};
use crate::sync::RwLockExt;

impl Engine {
    /// Save personal labels without changing machine identity or SSH configuration.
    pub fn set_machine_preferences(&self, id: &str, display_name: &str, description: &str) -> Result<(), EngineError> {
        if self.config().demo_mode {
            return Err(EngineError::Invalid(
                "Turn off demo data to label your machines.".into(),
            ));
        }
        let display_name = display_name.trim();
        let description = description.trim();
        if display_name.chars().count() > 120 || display_name.chars().any(char::is_control) {
            return Err(EngineError::Invalid(
                "Use a display name of up to 120 characters on one line.".into(),
            ));
        }
        if description.chars().count() > 2000 {
            return Err(EngineError::Invalid(
                "Use a description of up to 2,000 characters.".into(),
            ));
        }
        let view = self.view();
        let machine = view
            .machines
            .into_iter()
            .chain(view.other_machines)
            .find(|m| m.id == id)
            .ok_or_else(|| EngineError::NotFound("This machine is no longer listed. Refresh and try again.".into()))?;
        let key = machine.preferences_key();
        let preferences = MachinePreferences {
            display_name: (!display_name.is_empty()).then(|| display_name.to_string()),
            description: (!description.is_empty()).then(|| description.to_string()),
        };
        self.update_config(|config| {
            // A peer may have gained an inventory entry since its first label.
            config.machine_preferences.remove(&machine.id);
            if preferences == MachinePreferences::default() {
                config.machine_preferences.remove(&key);
            } else {
                config.machine_preferences.insert(key, preferences);
            }
        })
    }

    /// Replace SSH collection with a deterministic boundary in tests.
    pub fn with_machine_source(mut self, source: Arc<dyn MachineSource>) -> Self {
        self.machine_source = source;
        self
    }

    /// Read health and runner state, with at most four concurrent SSH sessions.
    /// Concurrent refresh requests coalesce; failures preserve the last good data.
    pub async fn poll_machines(&self) {
        let Ok(_poll) = self.machine_poll.try_lock() else {
            return;
        };
        self.poll_machines_inner(None).await;
    }

    /// Await the requested observation scope even when automatic polling is in progress.
    pub(crate) async fn poll_machines_wait(&self, machine: Option<&str>) {
        let _poll = self.machine_poll.lock().await;
        self.poll_machines_inner(machine).await;
    }

    async fn poll_machines_inner(&self, machine: Option<&str>) {
        if self.config().demo_mode {
            return;
        }

        if let Ok(inventory) = self.read_inventory().await {
            self.resolve_ssh_hosts(&inventory).await;
        }
        let view = self.view();
        let targets = view
            .machines
            .into_iter()
            .chain(view.other_machines)
            .filter(|m| machine.is_none_or(|id| m.id == id || m.preferences_key() == id))
            .filter(|m| m.tailnet.as_ref().is_none_or(|p| p.online))
            .filter_map(|m| {
                if let Some(d) = m.desired {
                    d.host
                        .enabled
                        .then(|| (inventory_machine_key(&d.host.id), MachineTarget::Inventory(d.host)))
                } else {
                    m.tailnet.map(|p| (m.id, MachineTarget::discovered(&p)))
                }
            })
            .collect::<Vec<_>>();
        let source = &self.machine_source;
        let mut reads = stream::iter(targets.into_iter().map(|(id, target)| {
            let source = source.clone();
            async move {
                let inspected = target.clone();
                let result = tokio::task::spawn_blocking(move || source.inspect(&inspected))
                    .await
                    .map_err(|e| format!("The SSH check stopped unexpectedly: {e}"))
                    .and_then(|r| r.map_err(|e| e.to_string()));
                (id, target, result)
            }
        }))
        .buffer_unordered(4);
        while let Some((id, target, result)) = reads.next().await {
            let now = Utc::now();
            {
                let mut observations = self.obs.write_locked();
                let entry = observations.machines.entry(id).or_insert_with(|| MachineObservation {
                    target: target.clone(),
                    report: None,
                    last_error: None,
                });
                if entry.target != target {
                    *entry = MachineObservation {
                        target,
                        report: None,
                        last_error: None,
                    };
                }
                match result {
                    Ok(report) => {
                        entry.report = Some(Timed::new(now, report));
                        entry.last_error = None;
                    }
                    Err(error) => entry.last_error = Some(Timed::new(now, error)),
                }
            }
            self.view_changed();
        }
    }
}
