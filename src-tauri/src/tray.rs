//! The menu bar item: a glance at the fleet, and the way back to the dashboard once its
//! window is closed.

use fleet::model::FleetSummary;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};
use tokio::sync::watch;

const OPEN: &str = "tray-open";
const SETTINGS: &str = "tray-settings";
const QUIT: &str = "tray-quit";

/// The fleet summary the menu's status lines show.
pub struct TrayStatus(watch::Sender<FleetSummary>);

impl TrayStatus {
    /// Show `summary` in the menu. Never waits for the menu: it follows the latest summary.
    pub fn show(&self, summary: &FleetSummary) {
        self.0.send_if_modified(|shown| {
            let changed = shown != summary;
            if changed {
                shown.clone_from(summary);
            }
            changed
        });
    }
}

/// Adds the menu bar item, which stays until the app quits, and manages its [`TrayStatus`].
pub fn start(app: &AppHandle) -> tauri::Result<()> {
    let summary = FleetSummary::default();
    // Status is information, not a command, so its lines are disabled.
    let status = status_lines(&summary)
        .into_iter()
        .map(|line| MenuItem::new(app, line, false, None::<&str>))
        .collect::<tauri::Result<Vec<_>>>()?;

    let menu = Menu::new(app)?;
    for line in &status {
        menu.append(line)?;
    }
    menu.append_items(&[
        &PredefinedMenuItem::separator(app)?,
        &MenuItem::with_id(app, OPEN, "Open Runner Dashboard", true, None::<&str>)?,
        &MenuItem::with_id(app, SETTINGS, "Settings…", true, None::<&str>)?,
        &PredefinedMenuItem::separator(app)?,
        &MenuItem::with_id(app, QUIT, "Quit Runner Dashboard", true, None::<&str>)?,
    ])?;

    TrayIconBuilder::with_id("main")
        // A template image: black on transparent, which macOS tints to suit the menu bar.
        .icon(tauri::include_image!("icons/tray-icon.png"))
        .icon_as_template(true)
        .tooltip("Runner Dashboard")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            OPEN => crate::show_main_window(app),
            SETTINGS => crate::open_settings(app),
            QUIT => app.exit(0),
            _ => {}
        })
        .build(app)?;

    let (latest, mut changes) = watch::channel(summary);
    app.manage(TrayStatus(latest));
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        while changes.changed().await.is_ok() {
            let lines = status_lines(&changes.borrow_and_update());
            let items = status.clone();
            // Menu items change on the main thread, which is busy while a menu is open.
            // Queue the edit instead of holding this task until then.
            let queued = app.run_on_main_thread(move || {
                for (item, line) in items.iter().zip(lines) {
                    if let Err(e) = item.set_text(line) {
                        tracing::warn!("updating the menu bar status failed: {e}");
                    }
                }
            });
            if let Err(e) = queued {
                tracing::warn!("updating the menu bar status failed: {e}");
            }
        }
    });
    Ok(())
}

/// The menu's status lines: runners, then queued jobs, then what needs attention.
fn status_lines(s: &FleetSummary) -> [String; 3] {
    let runners = if s.runners == 0 {
        "No runners".to_owned()
    } else {
        let idle = s
            .runners
            .saturating_sub(s.runners_busy + s.runners_offline + s.runners_unknown);
        let mut line = format!("Runners: {} working, {idle} idle", s.runners_busy);
        for (count, label) in [(s.runners_offline, "offline"), (s.runners_unknown, "unknown")] {
            if count > 0 {
                line.push_str(&format!(", {count} {label}"));
            }
        }
        line
    };
    let queue = match s.queued_jobs {
        0 => "No queued jobs".to_owned(),
        1 => "1 queued job".to_owned(),
        jobs => format!("{jobs} queued jobs"),
    };
    let attention = match s.attention {
        0 => "Nothing needs attention".to_owned(),
        issues => format!("Needs attention: {issues}"),
    };
    [runners, queue, attention]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines_leave_out_empty_runner_states() {
        let all_well = FleetSummary {
            runners: 6,
            runners_online: 6,
            runners_busy: 2,
            ..FleetSummary::default()
        };
        assert_eq!(
            status_lines(&all_well),
            [
                "Runners: 2 working, 4 idle",
                "No queued jobs",
                "Nothing needs attention"
            ]
        );

        let troubled = FleetSummary {
            runners: 6,
            runners_online: 3,
            runners_busy: 2,
            runners_offline: 2,
            runners_unknown: 1,
            queued_jobs: 1,
            attention: 3,
            ..FleetSummary::default()
        };
        assert_eq!(
            status_lines(&troubled),
            [
                "Runners: 2 working, 1 idle, 2 offline, 1 unknown",
                "1 queued job",
                "Needs attention: 3"
            ]
        );
    }

    #[test]
    fn status_lines_for_a_fleet_without_runners() {
        let empty = FleetSummary {
            queued_jobs: 4,
            ..FleetSummary::default()
        };
        assert_eq!(
            status_lines(&empty),
            ["No runners", "4 queued jobs", "Nothing needs attention"]
        );
    }
}
