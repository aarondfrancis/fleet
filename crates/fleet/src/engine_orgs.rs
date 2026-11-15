//! GitHub organizations: adding and signing in, the cached API clients, and runner and job polls.

use std::sync::Arc;

use chrono::Utc;
use futures::future::join_all;

use super::{Engine, EngineError, SecretStore};
use crate::gh;
use crate::github::{GithubClient, GithubError};
use crate::model::{OrgAuth, OrgConfig, OrgObservation, RepoScope, Timed, default_api_base};
use crate::sync::{MutexExt, RwLockExt};

#[derive(Debug, thiserror::Error)]
enum GithubAuthError {
    #[error("No token in the Keychain for this organization.")]
    MissingToken,
    #[error("Couldn't read the GitHub token from the Keychain: {0}")]
    Secret(String),
    #[error("{0}")]
    Gh(#[from] gh::GhError),
}

/// Where one kind of GitHub data, and the latest error fetching it, live in an org's observations.
type ObservationSlot<T> = fn(&mut OrgObservation) -> (&mut Option<Timed<T>>, &mut Option<Timed<String>>);

impl Engine {
    /// Keychain calls can block, so they run off the async runtime.
    async fn secrets_call<T: Send + 'static>(
        &self,
        call: impl FnOnce(&dyn SecretStore) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let secrets = self.secrets.clone();
        tokio::task::spawn_blocking(move || call(secrets.as_ref()))
            .await
            .map_err(|e| format!("The Keychain request stopped unexpectedly: {e}"))?
    }

    fn find_org(&self, login: &str) -> Result<OrgConfig, EngineError> {
        let key = login.to_lowercase();
        self.config()
            .orgs
            .into_iter()
            .find(|o| o.login.to_lowercase() == key)
            .ok_or_else(|| {
                EngineError::NotFound(format!(
                    "{login} isn't an added organization. Add it in Settings ▸ GitHub first."
                ))
            })
    }

    /// API client for an org, with its token from the Keychain or from `gh`. An expired token is
    /// read again into the same client, so its ETag cache and rate-limit pause survive.
    /// `Err` carries a user-facing reason (no token, gh missing or logged out).
    async fn github_client(&self, org: &OrgConfig) -> Result<Arc<GithubClient>, GithubAuthError> {
        let key = org.login.to_lowercase();
        let cached = self.github.locked().get(&key).cloned();
        if let Some(client) = &cached
            && client.token_request().is_none()
        {
            return Ok(client.clone());
        }
        let request = cached.as_ref().and_then(|c| c.token_request());
        let token = match org.auth {
            OrgAuth::Token => {
                let account = key.clone();
                self.secrets_call(move |secrets| secrets.get(&account))
                    .await
                    .map_err(GithubAuthError::Secret)?
                    .ok_or(GithubAuthError::MissingToken)?
            }
            OrgAuth::GhCli => gh::token(&gh::host_for_api_base(&org.api_base)).await?,
        };
        if let Some(client) = cached {
            client.set_token(&token, request.unwrap_or_default());
            return Ok(client);
        }
        let client = Arc::new(GithubClient::new(&org.api_base, &token));
        self.github.locked().insert(key, client.clone());
        Ok(client)
    }

    /// GitHub errors, reworded for how this org signs in.
    fn github_error_text(&self, org: &OrgConfig, e: &GithubError) -> String {
        match (org.auth, e) {
            (OrgAuth::GhCli, GithubError::Forbidden(_)) => format!(
                "Your gh login can't list {}'s runners. GitHub requires the {} scope (and org owner rights) for that. Run `{}` in Terminal.",
                org.login,
                gh::RUNNERS_SCOPE,
                gh::refresh_command(&gh::host_for_api_base(&org.api_base))
            ),
            (OrgAuth::GhCli, GithubError::Unauthorized) => {
                "gh's token was rejected. Run `gh auth login` in Terminal; the app picks up the new login automatically.".into()
            }
            _ => e.to_string(),
        }
    }

    /// Fetch one kind of GitHub data for every org, keeping the last good value beside the newest error.
    async fn poll_github<T>(
        &self,
        fetch: impl AsyncFn(&GithubClient, &OrgConfig) -> Result<T, GithubError>,
        refetch_token_on: fn(&GithubError) -> bool,
        slot: ObservationSlot<T>,
    ) {
        let orgs = self.config().orgs;
        join_all(orgs.iter().map(|org| async {
            let key = org.login.to_lowercase();
            let client = match self.github_client(org).await {
                Ok(client) => client,
                Err(reason) => {
                    let mut obs = self.obs.write_locked();
                    let entry = obs.github.entry(key).or_default();
                    entry.missing_token = matches!(reason, GithubAuthError::MissingToken);
                    *slot(entry).1 = Some(Timed::new(Utc::now(), reason.to_string()));
                    return;
                }
            };
            let result = fetch(&client, org).await;
            if result.as_ref().is_err_and(refetch_token_on) {
                client.expire_token();
            }
            let mut obs = self.obs.write_locked();
            let entry = obs.github.entry(key).or_default();
            entry.missing_token = false;
            entry.rate_limit = client.rate_limit();
            let (value, error) = slot(entry);
            match result {
                Ok(fetched) => {
                    *value = Some(Timed::new(Utc::now(), fetched));
                    *error = None;
                }
                Err(e) => *error = Some(Timed::new(Utc::now(), self.github_error_text(org, &e))),
            }
        }))
        .await;
    }

    pub async fn poll_github_runners(&self) {
        self.poll_github(
            async |client, org| client.org_runners(&org.login).await,
            // A 403 here means the credential can't list runners. Fetching the token
            // again picks up a fix such as `gh auth refresh --scopes admin:org`.
            |e| matches!(e, GithubError::Unauthorized | GithubError::Forbidden(_)),
            |org| (&mut org.runners, &mut org.error),
        )
        .await;
    }

    pub async fn poll_github_jobs(&self) {
        self.poll_github(
            async |client, org| client.scan_jobs(&org.login, &org.repo_scope).await,
            // Repositories the token can't read only mark the scan incomplete, so a 403
            // says nothing about the credential.
            |e| matches!(e, GithubError::Unauthorized),
            |org| (&mut org.jobs, &mut org.jobs_error),
        )
        .await;
    }

    /// Adds an org after checking the credential can list its runners.
    /// `GhCli` uses the GitHub CLI's login; `Token` stores `token` in the Keychain.
    pub async fn add_org(
        &self,
        login: &str,
        auth: OrgAuth,
        token: Option<&str>,
        repo_scope: RepoScope,
        api_base: Option<String>,
    ) -> Result<(), EngineError> {
        let login = login.trim().trim_start_matches('@').to_string();
        if login.is_empty() || login.contains('/') || login.contains(' ') {
            return Err(EngineError::Invalid("Enter an organization login, like `acme`.".into()));
        }
        let key = login.to_lowercase();
        if self.config().orgs.iter().any(|o| o.login.to_lowercase() == key) {
            return Err(EngineError::Invalid(format!("{login} is already added.")));
        }
        let api_base = api_base.unwrap_or_else(default_api_base);
        let org = OrgConfig {
            login: login.clone(),
            api_base: api_base.clone(),
            repo_scope,
            added_at: Utc::now(),
            auth,
        };
        let token = match auth {
            OrgAuth::GhCli => gh::token(&gh::host_for_api_base(&api_base))
                .await
                .map_err(|e| EngineError::Invalid(e.to_string()))?,
            OrgAuth::Token => {
                let t = token.map(str::trim).unwrap_or_default();
                if t.is_empty() {
                    return Err(EngineError::Invalid("A token is required.".into()));
                }
                t.to_string()
            }
        };
        let client = GithubClient::new(&api_base, &token);
        let runners = client
            .org_runners(&login)
            .await
            .map_err(|e| EngineError::Invalid(self.github_error_text(&org, &e)))?;

        if auth == OrgAuth::Token {
            let account = key.clone();
            self.secrets_call(move |secrets| secrets.set(&account, &token))
                .await
                .map_err(EngineError::Storage)?;
        }
        self.github.locked().insert(key.clone(), Arc::new(client));
        self.update_config(|c| c.orgs.push(org))?;
        let mut obs = self.obs.write_locked();
        let entry = obs.github.entry(key).or_default();
        entry.runners = Some(Timed::new(Utc::now(), runners));
        entry.missing_token = false;
        entry.error = None;
        Ok(())
    }

    /// Switches an org to the GitHub CLI login (after checking it works) and
    /// removes any stored token.
    pub async fn use_gh_for_org(&self, login: &str) -> Result<(), EngineError> {
        let key = login.to_lowercase();
        let mut org = self.find_org(login)?;
        org.auth = OrgAuth::GhCli;
        let token = gh::token(&gh::host_for_api_base(&org.api_base))
            .await
            .map_err(|e| EngineError::Invalid(e.to_string()))?;
        let client = GithubClient::new(&org.api_base, &token);
        client
            .org_runners(&org.login)
            .await
            .map_err(|e| EngineError::Invalid(self.github_error_text(&org, &e)))?;
        self.update_config(|c| {
            if let Some(o) = c.orgs.iter_mut().find(|o| o.login.to_lowercase() == key) {
                o.auth = OrgAuth::GhCli;
            }
        })?;
        self.delete_token(&key).await;
        self.github.locked().insert(key, Arc::new(client));
        Ok(())
    }

    /// Re-reads gh's login on the next poll (after `gh auth refresh` / `login`).
    pub fn reload_gh_credentials(&self) {
        let orgs = self.config().orgs;
        let cache = self.github.locked();
        for o in orgs.iter().filter(|o| o.auth == OrgAuth::GhCli) {
            if let Some(client) = cache.get(&o.login.to_lowercase()) {
                client.expire_token();
            }
        }
    }

    pub async fn update_org_token(&self, login: &str, token: &str) -> Result<(), EngineError> {
        let key = login.to_lowercase();
        let org = self.find_org(login)?;
        let token = token.trim().to_string();
        let client = GithubClient::new(&org.api_base, &token);
        client.org_runners(&org.login).await?;
        let account = key.clone();
        self.secrets_call(move |secrets| secrets.set(&account, &token))
            .await
            .map_err(EngineError::Storage)?;
        self.update_config(|c| {
            if let Some(o) = c.orgs.iter_mut().find(|o| o.login.to_lowercase() == key) {
                o.auth = OrgAuth::Token;
            }
        })?;
        self.github.locked().insert(key, Arc::new(client));
        Ok(())
    }

    pub fn set_org_scope(&self, login: &str, scope: RepoScope) -> Result<(), EngineError> {
        let key = login.to_lowercase();
        self.update_config(|c| {
            if let Some(o) = c.orgs.iter_mut().find(|o| o.login.to_lowercase() == key) {
                o.repo_scope = scope;
            }
        })
    }

    pub async fn remove_org(&self, login: &str) -> Result<(), EngineError> {
        let key = login.to_lowercase();
        self.update_config(|c| c.orgs.retain(|o| o.login.to_lowercase() != key))?;
        self.delete_token(&key).await;
        self.github.locked().remove(&key);
        self.obs.write_locked().github.remove(&key);
        Ok(())
    }

    /// The org no longer uses a stored token, so a failed delete only leaves an unused Keychain item.
    async fn delete_token(&self, key: &str) {
        let account = key.to_string();
        if let Err(e) = self.secrets_call(move |secrets| secrets.delete(&account)).await {
            tracing::warn!("couldn't delete the stored GitHub token for {key}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    use super::*;
    use crate::engine::MemorySecrets;
    use crate::test_server::serve;

    #[tokio::test]
    async fn rejected_token_is_read_again_into_the_same_etag_cache() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let router = Router::new().route(
            "/orgs/{org}/actions/runners",
            get(move |headers: HeaderMap| {
                let header = |name: &str| headers.get(name).map(|v| v.to_str().unwrap().to_string());
                let request = (header("authorization"), header("if-none-match"));
                log.locked().push(request.clone());
                async move {
                    match (request.0.as_deref(), request.1.as_deref()) {
                        (Some("Bearer old"), None) => {
                            ([("etag", "\"v1\"")], Json(json!({"total_count": 0, "runners": []}))).into_response()
                        }
                        (Some("Bearer new"), Some("\"v1\"")) => StatusCode::NOT_MODIFIED.into_response(),
                        _ => StatusCode::UNAUTHORIZED.into_response(),
                    }
                }
            }),
        );
        let base = serve(router).await;
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemorySecrets::default());
        let engine = Engine::load(dir.path().join("config.json"), secrets.clone()).unwrap();
        engine
            .add_org("acme", OrgAuth::Token, Some("old"), RepoScope::None, Some(base))
            .await
            .unwrap();
        // The token is replaced in the Keychain; GitHub now rejects the old one.
        secrets.set("acme", "new").unwrap();
        engine.poll_github_runners().await;
        engine.poll_github_runners().await;
        let etag = Some("\"v1\"".to_string());
        let bearer = |token: &str| Some(format!("Bearer {token}"));
        assert_eq!(
            *seen.locked(),
            vec![
                (bearer("old"), None),
                (bearer("old"), etag.clone()),
                (bearer("new"), etag),
            ]
        );
        assert_eq!(engine.view().orgs[0].error, None);
    }
}
