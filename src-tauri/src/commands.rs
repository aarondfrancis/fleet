//! Tauri commands. Every mutation re-emits the fleet view so the UI updates
//! from the same event stream as background polls.

use std::sync::Arc;

use fleet::engine::desired::{ChangeSecrets, ReviewView};
use fleet::engine::{Engine, EngineError};
use fleet::fleet_plan::ChangeRequest;
use fleet::model::{AppConfig, FleetView, OrgAuth, RepoScope};
use fleet::sync::MutexExt;
use tauri::{AppHandle, State};

use crate::poller::{Poller, emit_view};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error("{0}")]
    Invalid(String),
}

impl serde::Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

type Result<T> = std::result::Result<T, AppError>;
type EngineState<'a> = State<'a, Arc<Engine>>;

/// Picks up `gh auth refresh` / `gh auth login` done since the last poll, then polls every source now.
pub fn refresh(engine: &Engine, poller: &Poller) {
    engine.reload_gh_credentials();
    poller.refresh_now();
}

pub fn switch_demo_mode(app: &AppHandle, engine: &Engine, poller: &Poller, on: bool) -> Result<()> {
    engine.set_demo_mode(on)?;
    if !on {
        poller.refresh_now();
    }
    emit_view(app, engine);
    Ok(())
}

#[tauri::command]
pub fn get_fleet(engine: EngineState) -> FleetView {
    engine.view()
}

#[tauri::command]
pub fn get_config(engine: EngineState) -> AppConfig {
    engine.config()
}

#[tauri::command]
pub fn refresh_now(engine: EngineState, poller: State<'_, Poller>) {
    refresh(&engine, &poller);
}

#[tauri::command]
pub async fn add_org(
    app: AppHandle,
    engine: EngineState<'_>,
    login: String,
    auth: OrgAuth,
    token: Option<String>,
    repo_scope: RepoScope,
) -> Result<()> {
    engine.add_org(&login, auth, token.as_deref(), repo_scope, None).await?;
    emit_view(&app, &engine);
    Ok(())
}

/// The GitHub CLI's install/login state, for Settings.
#[tauri::command]
pub async fn get_gh_status() -> fleet::gh::GhStatus {
    fleet::gh::status("github.com").await
}

#[tauri::command]
pub async fn use_gh_for_org(app: AppHandle, engine: EngineState<'_>, login: String) -> Result<()> {
    engine.use_gh_for_org(&login).await?;
    emit_view(&app, &engine);
    Ok(())
}

#[tauri::command]
pub async fn update_org_token(app: AppHandle, engine: EngineState<'_>, login: String, token: String) -> Result<()> {
    engine.update_org_token(&login, &token).await?;
    emit_view(&app, &engine);
    Ok(())
}

#[tauri::command]
pub fn set_org_scope(app: AppHandle, engine: EngineState, login: String, repo_scope: RepoScope) -> Result<()> {
    engine.set_org_scope(&login, repo_scope)?;
    emit_view(&app, &engine);
    Ok(())
}

#[tauri::command]
pub async fn remove_org(app: AppHandle, engine: EngineState<'_>, login: String) -> Result<()> {
    engine.remove_org(&login).await?;
    emit_view(&app, &engine);
    Ok(())
}

#[tauri::command]
pub fn ignore_peer(app: AppHandle, engine: EngineState, node_id: String) -> Result<()> {
    engine.ignore_peer(&node_id)?;
    emit_view(&app, &engine);
    Ok(())
}

#[tauri::command]
pub fn unignore_peer(app: AppHandle, engine: EngineState, node_id: String) -> Result<()> {
    engine.unignore_peer(&node_id)?;
    emit_view(&app, &engine);
    Ok(())
}

#[tauri::command]
pub fn set_machine_preferences(
    app: AppHandle,
    engine: EngineState,
    id: String,
    display_name: String,
    description: String,
) -> Result<()> {
    engine.set_machine_preferences(&id, &display_name, &description)?;
    emit_view(&app, &engine);
    Ok(())
}

#[tauri::command]
pub fn set_demo_mode(app: AppHandle, engine: EngineState, poller: State<'_, Poller>, on: bool) -> Result<()> {
    switch_demo_mode(&app, &engine, &poller, on)
}

/// Opens GitHub links in the browser. Only github.com https URLs are allowed.
#[tauri::command]
pub fn open_github_url(url: String) -> Result<()> {
    if !url.starts_with("https://github.com/") {
        return Err(AppError::Invalid("Only github.com links can be opened".into()));
    }
    tauri_plugin_opener::open_url(url, None::<&str>).map_err(|e| AppError::Invalid(e.to_string()))
}

#[tauri::command]
pub fn get_local_api_status(status: State<'_, crate::local_api::SharedStatus>) -> crate::local_api::LocalApiStatus {
    status.locked().clone()
}

/// Takes effect at next launch (the server isn't restarted live).
#[tauri::command]
pub fn set_local_api(engine: EngineState, on: bool) -> Result<()> {
    Ok(engine.set_local_api(on)?)
}

// Desired state: checks are read-only. Changes run only from a review the user saw,
// and only if the host still matches it; passwords are used once and never stored.

#[tauri::command]
pub async fn desired_refresh(engine: EngineState<'_>, host: Option<String>) -> Result<()> {
    Ok(engine.inner().desired_refresh(host).await?)
}

#[tauri::command]
pub async fn desired_doctor(engine: EngineState<'_>, host: Option<String>) -> Result<()> {
    Ok(engine.inner().desired_doctor(host).await?)
}

#[tauri::command]
pub async fn desired_logs(engine: EngineState<'_>, runner: String, lines: u32) -> Result<String> {
    Ok(engine.desired_logs(runner, lines).await?)
}

#[tauri::command]
pub async fn desired_review(engine: EngineState<'_>, request: ChangeRequest) -> Result<ReviewView> {
    Ok(engine.inner().desired_review(request).await?)
}

#[tauri::command]
pub async fn desired_execute(
    engine: EngineState<'_>,
    review_id: String,
    secrets: ChangeSecrets,
    confirmation: Option<String>,
) -> Result<()> {
    Ok(engine
        .inner()
        .desired_execute(&review_id, secrets, confirmation)
        .await?)
}

#[tauri::command]
pub fn desired_dismiss_operation(engine: EngineState) {
    engine.desired_dismiss_operation()
}

#[tauri::command]
pub async fn set_machine_maintenance(app: AppHandle, engine: EngineState<'_>, id: String, paused: bool) -> Result<()> {
    let owned = engine.inner().clone();
    tauri::async_runtime::spawn_blocking(move || owned.set_machine_maintenance(&id, paused))
        .await
        .map_err(|e| AppError::Invalid(e.to_string()))??;
    emit_view(&app, &engine);
    Ok(())
}

#[tauri::command]
pub fn get_monitoring_events(
    engine: EngineState<'_>,
    machine_key: Option<String>,
    before: Option<String>,
) -> Vec<fleet::monitoring::FleetMachineEvent> {
    engine.monitoring_events(machine_key.as_deref(), before.as_deref())
}

#[tauri::command]
pub async fn set_desktop_notifications(app: AppHandle, engine: EngineState<'_>, enabled: bool) -> Result<()> {
    let owned = engine.inner().clone();
    tauri::async_runtime::spawn_blocking(move || owned.set_desktop_notifications(enabled))
        .await
        .map_err(|e| AppError::Invalid(e.to_string()))??;
    emit_view(&app, &engine);
    Ok(())
}

#[tauri::command]
pub fn test_desktop_notification(app: AppHandle) -> Result<()> {
    crate::notifications::test(&app).map_err(AppError::Invalid)
}
