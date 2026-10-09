//! Background polling. Each source has its own cadence; after every poll the
//! fresh `FleetView` is pushed to the frontend as `fleet-updated`.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use fleet::engine::Engine;
use fleet::monitoring::FleetMachineEvent;
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

pub const FLEET_UPDATED: &str = "fleet-updated";

pub struct Poller {
    pub refresh: Arc<Notify>,
}

pub fn emit_view(app: &AppHandle, engine: &Engine) {
    if let Err(e) = app.emit(FLEET_UPDATED, engine.view()) {
        tracing::warn!("emit failed: {e}");
    }
}

pub fn start(app: AppHandle, engine: Arc<Engine>) -> Poller {
    let refresh = Arc::new(Notify::new());

    spawn_loop(&app, &engine, &refresh, Duration::from_secs(15), |e| async move {
        e.poll_tailscale().await
    });
    spawn_loop(&app, &engine, &refresh, Duration::from_secs(60), |e| async move {
        if !e.tailnet_available() {
            e.poll_tailscale().await;
        }
        e.poll_machines().await
    });
    spawn_loop(&app, &engine, &refresh, Duration::from_secs(20), |e| async move {
        e.poll_github_runners().await
    });
    spawn_loop(&app, &engine, &refresh, Duration::from_secs(45), |e| async move {
        e.poll_github_jobs().await
    });

    // Re-derive periodically even without new data so "as of" ages and
    // staleness thresholds advance on screen.
    {
        let app = app.clone();
        let engine = engine.clone();
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(5));
            loop {
                tick.tick().await;
                let monitor = engine.clone();
                match tauri::async_runtime::spawn_blocking(move || monitor.record_monitoring(chrono::Utc::now())).await
                {
                    Ok(events) => deliver_notifications(&app, &engine, events).await,
                    Err(error) => tracing::warn!("recording machine history failed: {error}"),
                }
                emit_view(&app, &engine);
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

    Poller { refresh }
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

fn spawn_loop<F, Fut>(app: &AppHandle, engine: &Arc<Engine>, refresh: &Arc<Notify>, period: Duration, poll: F)
where
    F: Fn(Arc<Engine>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let app = app.clone();
    let engine = engine.clone();
    let refresh = refresh.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            if !engine.config().demo_mode {
                poll(engine.clone()).await;
                emit_view(&app, &engine);
            }
            tokio::select! {
                _ = tokio::time::sleep(period) => {}
                _ = refresh.notified() => {}
            }
        }
    });
}
