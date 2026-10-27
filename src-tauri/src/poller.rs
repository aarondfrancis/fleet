//! Background polling. Each source has its own cadence; after every poll the
//! fresh `FleetView` is pushed to the frontend as `fleet-updated`. A five-second tick
//! re-derives the view and pushes it only when its content changed.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use fleet::engine::Engine;
use fleet::model::FleetView;
use fleet::monitoring::FleetMachineEvent;
use fleet::sync::MutexExt;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::watch;

pub const FLEET_UPDATED: &str = "fleet-updated";

pub struct Poller {
    /// A generation counter, so a request made while a loop is polling isn't lost.
    refresh: watch::Sender<u64>,
    /// Numbers views in the order their derivation starts, independent of the wall clock.
    derivations: AtomicU64,
    last_push: Mutex<LastPush>,
}

impl Poller {
    /// Poll every source now, or right after a poll already in progress.
    pub fn refresh_now(&self) {
        self.refresh
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Push {
    Always,
    IfChanged,
}

/// The last view pushed to the UI.
#[derive(Debug, Default)]
struct LastPush {
    derivation: u64,
    revision: Option<String>,
}

impl LastPush {
    /// Whether to push `view`, numbered by `derivation`: never one whose derivation started
    /// before the last push's (a tick's view can be overtaken by a poll's), and for `IfChanged`
    /// only when its content differs.
    fn accept(&mut self, derivation: u64, view: &FleetView, when: Push) -> bool {
        let revision = fleet::local_api::revision(view).ok();
        let unchanged = when == Push::IfChanged && revision.is_some() && revision == self.revision;
        if derivation < self.derivation || unchanged {
            return false;
        }
        self.derivation = derivation;
        self.revision = revision;
        true
    }
}

/// Number the view about to be derived.
fn next_derivation(app: &AppHandle) -> u64 {
    app.state::<Poller>().derivations.fetch_add(1, Ordering::Relaxed)
}

pub fn emit_view(app: &AppHandle, engine: &Engine) {
    let derivation = next_derivation(app);
    push(app, derivation, engine.view(), Push::Always);
}

fn push(app: &AppHandle, derivation: u64, view: FleetView, when: Push) {
    // Emit under the lock so pushes reach the UI in the order they were accepted.
    let mut last = app.state::<Poller>().inner().last_push.locked();
    if last.accept(derivation, &view, when)
        && let Err(e) = app.emit(FLEET_UPDATED, view)
    {
        tracing::warn!("emit failed: {e}");
    }
}

/// Starts the polling loops and manages the [`Poller`] that Refresh Now and [`emit_view`] use.
pub fn start(app: AppHandle, engine: Arc<Engine>) {
    // Managed before any loop can push.
    app.manage(Poller {
        refresh: watch::Sender::new(0),
        derivations: AtomicU64::new(0),
        last_push: Mutex::default(),
    });
    let refresh = &app.state::<Poller>().inner().refresh;

    spawn_loop(&app, &engine, refresh, Duration::from_secs(15), |e| async move {
        e.poll_tailscale().await
    });
    spawn_loop(&app, &engine, refresh, Duration::from_secs(60), |e| async move {
        if !e.tailnet_available() {
            e.poll_tailscale().await;
        }
        e.poll_machines().await
    });
    spawn_loop(&app, &engine, refresh, Duration::from_secs(20), |e| async move {
        e.poll_github_runners().await
    });
    spawn_loop(&app, &engine, refresh, Duration::from_secs(45), |e| async move {
        e.poll_github_jobs().await
    });

    // Re-derive periodically even without new data so history is recorded and
    // staleness thresholds advance on screen; unchanged views aren't pushed.
    {
        let app = app.clone();
        let engine = engine.clone();
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(5));
            loop {
                tick.tick().await;
                let monitor = engine.clone();
                let derivation = next_derivation(&app);
                match tauri::async_runtime::spawn_blocking(move || monitor.record_monitoring(Utc::now())).await {
                    Ok((events, view)) => {
                        push(&app, derivation, view, Push::IfChanged);
                        deliver_notifications(&app, &engine, events).await;
                    }
                    Err(error) => {
                        tracing::warn!("recording machine history failed: {error}");
                        push(&app, next_derivation(&app), engine.view(), Push::IfChanged);
                    }
                }
            }
        });
    }

    // Desired state: one read-only check shortly after launch, then on demand.
    if !engine.config().demo_mode {
        let engine = engine.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            if let Err(e) = engine.desired_refresh(None).await {
                tracing::info!("desired-state check skipped: {e}");
            }
        });
    }

    // Desired state is part of the fleet view: push it whenever a check or
    // change progresses.
    {
        let app = app.clone();
        let engine = engine.clone();
        let mut changes = engine.desired_changes();
        tauri::async_runtime::spawn(async move {
            while changes.changed().await.is_ok() {
                emit_view(&app, &engine);
            }
        });
    }
}

/// Ask the OS to show due alerts, then record that it accepted them so they aren't sent again.
/// Unaccepted requests stay queued and are retried by a later tick.
async fn deliver_notifications(app: &AppHandle, engine: &Arc<Engine>, events: Vec<FleetMachineEvent>) {
    let config = engine.config();
    if events.is_empty() || !config.desktop_notifications || config.demo_mode {
        return;
    }
    if let Err(error) = crate::notifications::send_alerts(app, &events) {
        tracing::warn!("notification request failed; will retry: {error}");
        return;
    }
    let monitor = engine.clone();
    match tauri::async_runtime::spawn_blocking(move || monitor.acknowledge_notifications(&events)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!("saving notification acknowledgement failed: {error}"),
        Err(error) => tracing::warn!("notification acknowledgement task failed: {error}"),
    }
}

fn spawn_loop<F, Fut>(app: &AppHandle, engine: &Arc<Engine>, refresh: &watch::Sender<u64>, period: Duration, poll: F)
where
    F: Fn(Arc<Engine>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let app = app.clone();
    let engine = engine.clone();
    let mut refresh = refresh.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            if !engine.config().demo_mode {
                poll(engine.clone()).await;
                emit_view(&app, &engine);
            }
            wait_for_next_poll(period, &mut refresh).await;
        }
    });
}

/// Sleep for `period`, or until a refresh requested since the last wait, even during the poll.
async fn wait_for_next_poll(period: Duration, refresh: &mut watch::Receiver<u64>) {
    tokio::select! {
        _ = tokio::time::sleep(period) => {}
        Ok(()) = refresh.changed() => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;
    use fleet::model::DataSource;

    fn view() -> FleetView {
        let now = Utc::now();
        let (config, obs) = fleet::demo::dataset(now);
        let plan = fleet::demo_desired::plan_inputs(now);
        fleet::derive::derive(&config, &obs, &plan, now, DataSource::Demo)
    }

    #[test]
    fn ticks_push_changed_views_only() {
        let mut last = LastPush::default();
        let first = view();
        assert!(last.accept(0, &first, Push::IfChanged));
        let mut same = first.clone();
        same.generated_at += TimeDelta::seconds(5);
        assert!(!last.accept(1, &same, Push::IfChanged), "only the time changed");
        assert!(last.accept(2, &same, Push::Always));
        let mut changed = same;
        changed.machines.pop().expect("demo machines");
        assert!(last.accept(3, &changed, Push::IfChanged));
    }

    #[test]
    fn a_view_derived_before_the_last_push_is_dropped() {
        let mut last = LastPush::default();
        let first = view();
        let mut newer = first.clone();
        newer.machines.pop().expect("demo machines");
        assert!(last.accept(2, &newer, Push::Always));
        assert!(!last.accept(1, &first, Push::IfChanged), "a tick overtaken by a poll");
        assert!(!last.accept(1, &first, Push::Always));
    }

    #[test]
    fn a_backward_clock_step_does_not_freeze_pushes() {
        let mut last = LastPush::default();
        let first = view();
        assert!(last.accept(0, &first, Push::Always));
        let mut after_step = first.clone();
        after_step.generated_at -= TimeDelta::hours(2);
        after_step.machines.pop().expect("demo machines");
        assert!(last.accept(1, &after_step, Push::IfChanged));
        assert!(last.accept(2, &after_step, Push::Always));
    }

    #[tokio::test]
    async fn refresh_requested_during_a_poll_wakes_the_next_wait() {
        let poller = Poller {
            refresh: watch::Sender::new(0),
            derivations: AtomicU64::new(0),
            last_push: Mutex::default(),
        };
        let mut refresh = poller.refresh.subscribe();
        // The loop is still polling, not waiting, when Refresh Now arrives.
        poller.refresh_now();
        let wait = wait_for_next_poll(Duration::from_secs(3600), &mut refresh);
        tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .expect("the refresh request was kept");
    }
}
