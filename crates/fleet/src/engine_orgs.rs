//! GitHub accounts: adding and signing in, the cached API clients, and runner, job and
//! release polls.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures::future::join_all;

use super::{Engine, EngineError, SecretStore};
use crate::gh;
use crate::github::{AccountRunners, GithubClient, GithubError, MAX_REPOS};
use crate::model::{AccountKind, AccountSetup, OrgAuth, OrgConfig, OrgObservation, RepoScope, Timed, default_api_base};
use crate::sync::{MutexExt, RwLockExt};

/// How often recent `actions/runner` releases are read.
const RELEASE_CHECK_EVERY: chrono::Duration = chrono::Duration::hours(1);

#[derive(Debug, thiserror::Error)]
enum GithubAuthError {
    #[error("No token in the Keychain for this organization.")]
    MissingToken,
    #[error("Couldn't read the GitHub token from the Keychain: {0}")]
    Secret(String),
    #[error("{0}")]
    Gh(#[from] gh::GhError),
}

/// Where the latest error fetching one kind of GitHub data lives in an org's observations.
type ErrorSlot = fn(&mut OrgObservation) -> &mut Option<Timed<String>>;

/// Records one poll of an account's runners and returns the organization list's error. A list
/// that fails keeps its last runners, which age into stale, and hides no others.
fn store_runners(o: &mut OrgObservation, at: DateTime<Utc>, fetched: AccountRunners) -> Option<GithubError> {
    let org_error = match fetched.org {
        Ok(runners) => {
            o.runners = Some(Timed::new(at, runners));
            None
        }
        Err(e) => Some(e),
    };
    o.repo_runners
        .retain(|repo, _| fetched.repos.iter().any(|(r, _)| r == repo));
    o.repo_errors.clear();
    for (repo, result) in fetched.repos {
        match result {
            Ok(runners) => {
                o.repo_runners.insert(repo, Timed::new(at, runners));
            }
            Err(e) => {
                o.repo_errors.insert(repo, e.to_string());
            }
        }
    }
    org_error
}

/// Runner repository names of `owner`, without the owner. A personal account needs at least one.
fn runner_repos(owner: &str, kind: AccountKind, repos: &[String]) -> Result<Vec<String>, EngineError> {
    let mut names: Vec<String> = Vec::new();
    for entry in repos.iter().map(|r| r.trim()).filter(|r| !r.is_empty()) {
        let name = match entry.split_once('/') {
            Some((o, name)) if o.eq_ignore_ascii_case(owner) => name,
            Some(_) => {
                return Err(EngineError::Invalid(format!(
                    "{entry} isn't one of {owner}'s repositories."
                )));
            }
            None => entry,
        };
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
            return Err(EngineError::Invalid(format!("{entry} isn't a repository name.")));
        }
        if !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            names.push(name.to_string());
        }
    }
    if kind == AccountKind::User && names.is_empty() {
        return Err(EngineError::Invalid(
            "A personal account only has repository runners. List at least one repository.".into(),
        ));
    }
    if names.len() > MAX_REPOS {
        return Err(EngineError::Invalid(format!(
            "List at most {MAX_REPOS} runner repositories."
        )));
    }
    Ok(names)
}

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

    /// API client for an org, with its token from the Keychain or from `gh`.
    /// `Err` carries a user-facing reason (no token, gh missing or logged out).
    async fn github_client(&self, org: &OrgConfig) -> Result<Arc<GithubClient>, GithubAuthError> {
        let key = org.login.to_lowercase();
        if let Some(c) = self.github.locked().get(&key) {
            return Ok(c.clone());
        }
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

    /// Forget a cached client so the next poll fetches a fresh token (e.g. after `gh auth refresh`).
    fn drop_github_client(&self, org: &OrgConfig) {
        self.github.locked().remove(&org.login.to_lowercase());
    }

    /// Fetch one kind of GitHub data for every org, keeping the last good value beside the newest error.
    async fn poll_github<T>(
        &self,
        fetch: impl AsyncFn(&GithubClient, &OrgConfig) -> Result<T, GithubError>,
        refetch_token_on: fn(&GithubError) -> bool,
        error_slot: ErrorSlot,
        store: fn(&mut OrgObservation, DateTime<Utc>, T) -> Option<GithubError>,
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
                    *error_slot(entry) = Some(Timed::new(Utc::now(), reason.to_string()));
                    return;
                }
            };
            let result = fetch(&client, org).await;
            let mut obs = self.obs.write_locked();
            let entry = obs.github.entry(key).or_default();
            entry.missing_token = false;
            entry.rate_limit = client.rate_limit();
            let error = match result {
                Ok(fetched) => store(entry, Utc::now(), fetched),
                Err(e) => Some(e),
            };
            *error_slot(entry) = error
                .as_ref()
                .map(|e| Timed::new(Utc::now(), self.github_error_text(org, e)));
            drop(obs);
            if error.as_ref().is_some_and(refetch_token_on) {
                self.drop_github_client(org);
            }
        }))
        .await;
    }

    /// Organization and repository runners for every account, then runner releases when due.
    pub async fn poll_github_runners(&self) {
        self.poll_github(
            async |client, org| client.account_runners(org).await,
            // A 403 here means the credential can't list the organization's runners. Fetching
            // the token again picks up a fix such as `gh auth refresh --scopes admin:org`.
            // Runner repositories that can't be listed are recorded beside the others.
            |e| matches!(e, GithubError::Unauthorized | GithubError::Forbidden(_)),
            |org| &mut org.error,
            store_runners,
        )
        .await;
        self.poll_runner_releases().await;
    }

    pub async fn poll_github_jobs(&self) {
        self.poll_github(
            async |client, org| client.scan_jobs(&org.login, &org.job_scope()).await,
            // Repositories the token can't read only mark the scan incomplete, so a 403
            // says nothing about the credential. Keeping the client keeps its ETag cache.
            |e| matches!(e, GithubError::Unauthorized),
            |org| &mut org.jobs_error,
            |org, at, scan| {
                org.jobs = Some(Timed::new(at, scan));
                None
            },
        )
        .await;
    }

    /// Reads recent `actions/runner` releases at most hourly, through the first github.com
    /// account that can. GitHub Enterprise Server accounts are never used.
    async fn poll_runner_releases(&self) {
        let now = Utc::now();
        if self
            .obs
            .read_locked()
            .runner_releases_checked_at
            .is_some_and(|at| now - at < RELEASE_CHECK_EVERY)
        {
            return;
        }
        let accounts: Vec<OrgConfig> = self
            .config()
            .orgs
            .into_iter()
            .filter(|o| o.api_base.trim_end_matches('/') == default_api_base())
            .collect();
        if accounts.is_empty() {
            return;
        }
        self.obs.write_locked().runner_releases_checked_at = Some(now);
        for org in &accounts {
            let Ok(client) = self.github_client(org).await else {
                continue;
            };
            match client.runner_releases().await {
                Ok(releases) => {
                    self.obs.write_locked().runner_releases = Some(Timed::new(now, releases));
                    return;
                }
                Err(e) => tracing::warn!("couldn't read the actions/runner releases through {}: {e}", org.login),
            }
        }
    }

    /// Checks a credential before it's saved: it must list the organization's runners, or a
    /// personal account's runners in at least one repository.
    async fn check_access(&self, client: &GithubClient, org: &OrgConfig) -> Result<AccountRunners, EngineError> {
        let invalid = |e: &GithubError| EngineError::Invalid(self.github_error_text(org, e));
        let runners = client.account_runners(org).await.map_err(|e| invalid(&e))?;
        if let Err(e) = &runners.org {
            return Err(invalid(e));
        }
        if org.kind == AccountKind::User
            && let Some((_, Err(e))) = runners.repos.iter().find(|(_, r)| r.is_err())
            && runners.repos.iter().all(|(_, r)| r.is_err())
        {
            return Err(EngineError::Invalid(e.to_string()));
        }
        Ok(runners)
    }

    /// Adds an organization or personal account after checking the credential can list its runners.
    /// `GhCli` uses the GitHub CLI's login; `Token` stores `token` in the Keychain.
    pub async fn add_org(
        &self,
        login: &str,
        auth: OrgAuth,
        token: Option<&str>,
        setup: AccountSetup,
        api_base: Option<String>,
    ) -> Result<(), EngineError> {
        let login = login.trim().trim_start_matches('@').to_string();
        if login.is_empty() || login.contains('/') || login.contains(' ') {
            return Err(EngineError::Invalid(
                "Enter an organization or user login, like `acme`.".into(),
            ));
        }
        let runner_repos = runner_repos(&login, setup.kind, &setup.runner_repos)?;
        let key = login.to_lowercase();
        if self.config().orgs.iter().any(|o| o.login.to_lowercase() == key) {
            return Err(EngineError::Invalid(format!("{login} is already added.")));
        }
        let api_base = api_base.unwrap_or_else(default_api_base);
        let org = OrgConfig {
            login: login.clone(),
            api_base: api_base.clone(),
            repo_scope: setup.repo_scope,
            added_at: Utc::now(),
            auth,
            kind: setup.kind,
            runner_repos,
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
        let runners = self.check_access(&client, &org).await?;
        // Repositories added later only show their errors; a new account must list them all.
        if let Some(e) = runners.repos.iter().find_map(|(_, r)| r.as_ref().err()) {
            return Err(EngineError::Invalid(e.to_string()));
        }

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
        // check_access refused an organization list error.
        let _ = store_runners(entry, Utc::now(), runners);
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
        self.check_access(&client, &org).await?;
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
        let mut cache = self.github.locked();
        for o in orgs.iter().filter(|o| o.auth == OrgAuth::GhCli) {
            cache.remove(&o.login.to_lowercase());
        }
    }

    pub async fn update_org_token(&self, login: &str, token: &str) -> Result<(), EngineError> {
        let key = login.to_lowercase();
        let mut org = self.find_org(login)?;
        org.auth = OrgAuth::Token;
        let token = token.trim().to_string();
        let client = GithubClient::new(&org.api_base, &token);
        self.check_access(&client, &org).await?;
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

    /// Sets the job scope and runner repositories. Repositories that can't be listed show
    /// their own errors on the next poll; removed ones are forgotten now.
    pub fn set_org_scope(&self, login: &str, scope: RepoScope, repos: &[String]) -> Result<(), EngineError> {
        let key = login.to_lowercase();
        let org = self.find_org(login)?;
        let repos = runner_repos(&org.login, org.kind, repos)?;
        let kept: Vec<String> = repos.iter().map(|n| format!("{}/{n}", org.login)).collect();
        self.update_config(|c| {
            if let Some(o) = c.orgs.iter_mut().find(|o| o.login.to_lowercase() == key) {
                o.repo_scope = scope;
                o.runner_repos = repos;
            }
        })?;
        if let Some(o) = self.obs.write_locked().github.get_mut(&key) {
            o.repo_runners.retain(|repo, _| kept.contains(repo));
            o.repo_errors.retain(|repo, _| kept.contains(repo));
        }
        Ok(())
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
