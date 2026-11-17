mod commands;
mod keychain;
mod local_api;
mod notifications;
mod poller;
mod tray;

use std::sync::{Arc, Once};

use fleet::engine::Engine;
use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Listener, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

fn open_settings(app: &AppHandle) {
    enter_dock(app);
    if let Some(w) = app.get_webview_window("settings") {
        // Cosmetic: the window exists either way.
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("index.html#settings".into()))
        .title("Settings")
        .inner_size(900.0, 640.0)
        .min_inner_size(680.0, 440.0)
        .center()
        .build();
    match built {
        Ok(settings) => {
            let app = app.clone();
            settings.on_window_event(move |event| {
                if matches!(event, WindowEvent::Destroyed) {
                    leave_dock_after(&app, "settings");
                }
            });
        }
        Err(e) => tracing::error!("settings window: {e}"),
    }
}

/// Bring the main window to the front. Startup, the menu bar item, the Dock icon and
/// opening the app again all end up here.
fn show_main_window(app: &AppHandle) {
    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    enter_dock(app);
    // Cosmetic: the window exists either way.
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
}

/// Closing the main window hides it, state intact, and the app carries on in the menu bar.
/// Only Quit, from the app menu or the menu bar item, ends the app.
fn keep_running_when_closed(app: &AppHandle) {
    let Some(main) = app.get_webview_window("main") else {
        return;
    };
    let app = app.clone();
    main.clone().on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if let Err(e) = main.hide() {
                tracing::warn!("hiding the main window failed: {e}");
                return;
            }
            leave_dock_after(&app, "main");
        }
    });
}

/// Show the Dock icon, for a window that is about to open.
#[cfg(target_os = "macos")]
fn enter_dock(app: &AppHandle) {
    if let Err(e) = app.set_activation_policy(tauri::ActivationPolicy::Regular) {
        tracing::warn!("showing the Dock icon failed: {e}");
    }
}

/// Drop the Dock icon if the window labelled `closed` was the last one open, leaving the
/// menu bar item.
#[cfg(target_os = "macos")]
fn leave_dock_after(app: &AppHandle, closed: &str) {
    let another_open = app
        .webview_windows()
        .iter()
        .any(|(label, w)| label != closed && w.is_visible().unwrap_or(false));
    if !another_open && let Err(e) = app.set_activation_policy(tauri::ActivationPolicy::Accessory) {
        tracing::warn!("hiding the Dock icon failed: {e}");
    }
}

// Only macOS has a Dock icon to show and drop.
#[cfg(not(target_os = "macos"))]
fn enter_dock(_app: &AppHandle) {}

#[cfg(not(target_os = "macos"))]
fn leave_dock_after(_app: &AppHandle, _closed: &str) {}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, Some("CmdOrCtrl+,"))?;
    let refresh = MenuItem::with_id(app, "refresh", "Refresh Now", true, Some("CmdOrCtrl+R"))?;
    let demo = MenuItem::with_id(app, "toggle-demo", "Toggle Demo Data", true, Some("CmdOrCtrl+Shift+D"))?;
    let app_menu = Submenu::with_items(
        app,
        "Runner Dashboard",
        true,
        &[
            &PredefinedMenuItem::about(app, None, Some(AboutMetadata::default()))?,
            &PredefinedMenuItem::separator(app)?,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;
    // Edit menu is required for ⌘C/⌘V/⌘A to work in text fields.
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let view = Submenu::with_items(app, "View", true, &[&refresh, &demo])?;
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;
    Menu::with_items(app, &[&app_menu, &edit, &view, &window])
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Apps launched from Finder have no terminal, so also log to
    // ~/Library/Logs/Runner Dashboard/runner-dashboard.log for diagnosis.
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "runner_dashboard_lib=info,fleet=info".into())
    };
    let log_file = std::env::var_os("HOME").and_then(|home| {
        let dir = std::path::PathBuf::from(home).join("Library/Logs/Runner Dashboard");
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("runner-dashboard.log"))
            .ok()
    });
    match log_file {
        Some(file) => tracing_subscriber::fmt()
            .with_env_filter(filter())
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init(),
        None => tracing_subscriber::fmt().with_env_filter(filter()).init(),
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Runner Dashboard starting");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                // Settings always opens at its compact default; old saved geometry
                // must not override it. Keep remembering the main window's layout.
                .with_denylist(&["settings"])
                // Size and position are restored; visibility is not. The main window
                // starts hidden however the last run ended, closed to the menu bar or not.
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::all() & !tauri_plugin_window_state::StateFlags::VISIBLE,
                )
                .build(),
        )
        .setup(|app| {
            let config_path = app.path().app_config_dir()?.join("config.json");
            let engine = Arc::new(Engine::load(config_path, Arc::new(keychain::Keychain))?);
            app.manage(engine.clone());
            let api = local_api::start(engine.clone(), app.path().app_config_dir()?);
            app.manage(api);
            // Before the poller, which pushes each view's summary to the menu bar item.
            tray::start(app.handle())?;
            poller::start(app.handle().clone(), engine);

            app.set_menu(build_menu(app.handle())?)?;
            app.on_menu_event(|app, event| match event.id().as_ref() {
                "settings" => open_settings(app),
                "refresh" => commands::refresh(&app.state::<Arc<Engine>>(), &app.state::<poller::Poller>()),
                "toggle-demo" => {
                    let engine = app.state::<Arc<Engine>>();
                    let on = !engine.config().demo_mode;
                    if let Err(e) = commands::switch_demo_mode(app, &engine, &app.state::<poller::Poller>(), on) {
                        tracing::warn!("toggling demo data failed: {e}");
                    }
                }
                _ => {}
            });

            // The main window starts hidden; show it once React has painted,
            // or after a timeout so a frontend failure never leaves it invisible.
            // Once only: neither a frontend reload nor the timeout may reopen a
            // window the user has since closed.
            let first_show = Arc::new(Once::new());
            let (handle, once) = (app.handle().clone(), first_show.clone());
            app.listen("frontend-ready", move |_| once.call_once(|| show_main_window(&handle)));
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                first_show.call_once(|| show_main_window(&handle));
            });
            keep_running_when_closed(app.handle());
            let handle = app.handle().clone();
            app.listen("open-settings", move |_| open_settings(&handle));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_fleet,
            commands::get_config,
            commands::refresh_now,
            commands::add_org,
            commands::update_org_token,
            commands::set_org_scope,
            commands::remove_org,
            commands::ignore_peer,
            commands::unignore_peer,
            commands::set_machine_preferences,
            commands::set_machine_maintenance,
            commands::get_monitoring_events,
            commands::set_desktop_notifications,
            commands::test_desktop_notification,
            commands::set_demo_mode,
            commands::open_github_url,
            commands::get_local_api_status,
            commands::get_gh_status,
            commands::use_gh_for_org,
            commands::set_local_api,
            commands::desired_refresh,
            commands::desired_doctor,
            commands::desired_logs,
            commands::desired_review,
            commands::desired_execute,
            commands::desired_dismiss_operation,
        ])
        .build(tauri::generate_context!())
        .expect("error while running Runner Dashboard")
        .run(|app, event| {
            // Clicking the Dock icon, or opening the app again from Finder or Spotlight,
            // brings back a window that was closed to the menu bar.
            #[cfg(target_os = "macos")]
            if matches!(event, tauri::RunEvent::Reopen { .. }) {
                show_main_window(app);
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (app, event);
        });
}
