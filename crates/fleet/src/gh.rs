//! GitHub sign-in through the GitHub CLI (`gh`): the app asks `gh` for its
//! token when it needs one and keeps it in memory only. `gh` keeps owning the
//! credential (in its own Keychain item); nothing is copied into ours.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::process::Command;
use ts_rs::TS;

use std::time::Duration;

use crate::cli::{self, CLI_TIMEOUT, CliError};

/// Set to a `gh` binary path to use it instead of searching.
pub const GH_PATH_ENV: &str = "RUNNER_DASHBOARD_GH";

/// Scope GitHub requires for OAuth/classic tokens to list an org's runners.
pub const RUNNERS_SCOPE: &str = "admin:org";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct GhStatus {
    pub installed: bool,
    pub path: Option<String>,
    pub host: String,
    pub login: Option<String>,
    pub scopes: Vec<String>,
    /// Logged in, but without the scope needed to list org runners.
    pub missing_runners_scope: bool,
    /// What to tell the user when something's missing.
    pub problem: Option<String>,
    pub fix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GhError {
    #[error("The GitHub CLI (gh) isn't installed. Install it with `brew install gh`, then run `gh auth login`.")]
    NotInstalled,
    #[error("The GitHub CLI isn't logged in to {0}. Run `gh auth login --hostname {0}` in Terminal.")]
    NotLoggedIn(String),
    #[error("gh failed: {0}")]
    Failed(String),
    #[error("The GitHub CLI (gh) didn't respond within {0} seconds. Try again, or run `gh auth status` in Terminal.")]
    TimedOut(u64),
}

impl From<CliError> for GhError {
    fn from(e: CliError) -> Self {
        match e {
            CliError::Spawn(e) => Self::Failed(e.to_string()),
            CliError::TimedOut(limit) => Self::TimedOut(limit.as_secs()),
        }
    }
}

/// `gh` locations to try. GUI apps get a minimal PATH, so common install
/// locations are checked before PATH.
pub fn find() -> Option<PathBuf> {
    // Explicit override (tests use a stub so they never touch a real login).
    if let Some(p) = std::env::var_os(GH_PATH_ENV) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut candidates: Vec<PathBuf> = vec![
        "/opt/homebrew/bin/gh".into(),
        "/usr/local/bin/gh".into(),
        "/run/current-system/sw/bin/gh".into(),
    ];
    if let Some(h) = &home {
        candidates.push(h.join(".local/bin/gh"));
        candidates.push(h.join(".nix-profile/bin/gh"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|p| p.join("gh")));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// `https://api.github.com` -> `github.com`; GHES `https://ghe.example/api/v3` -> `ghe.example`.
pub fn host_for_api_base(api_base: &str) -> String {
    let host = url::Url::parse(api_base)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "github.com".into());
    if host == "api.github.com" {
        "github.com".into()
    } else {
        host
    }
}

pub async fn status(host: &str) -> GhStatus {
    let mut st = GhStatus {
        installed: false,
        path: None,
        host: host.to_string(),
        login: None,
        scopes: Vec::new(),
        missing_runners_scope: false,
        problem: None,
        fix: None,
    };
    let Some(gh) = find() else {
        st.problem = Some("The GitHub CLI (gh) isn't installed.".into());
        st.fix = Some("brew install gh && gh auth login".into());
        return st;
    };
    st.installed = true;
    st.path = Some(gh.display().to_string());
    let mut command = Command::new(&gh);
    command.args(["auth", "status", "--json", "hosts", "--hostname", host]);
    let out = match cli::output(command, CLI_TIMEOUT).await {
        Err(CliError::TimedOut(limit)) => {
            st.problem = Some(GhError::TimedOut(limit.as_secs()).to_string());
            return st;
        }
        out => out,
    };
    let parsed = out
        .ok()
        .and_then(|o| parse_status(&String::from_utf8_lossy(&o.stdout), host));
    match parsed {
        Some((login, scopes)) => {
            st.missing_runners_scope = !scopes.iter().any(|s| s == RUNNERS_SCOPE);
            if st.missing_runners_scope {
                st.problem = Some(format!(
                    "Your gh login ({login}) can't list organization runners yet: GitHub requires the {RUNNERS_SCOPE} scope for that."
                ));
                st.fix = Some(refresh_command(host));
            }
            st.login = Some(login);
            st.scopes = scopes;
        }
        None => {
            st.problem = Some(format!("gh isn't logged in to {host}."));
            st.fix = Some(format!("gh auth login --hostname {host}"));
        }
    }
    st
}

pub fn refresh_command(host: &str) -> String {
    format!("gh auth refresh --hostname {host} --scopes {RUNNERS_SCOPE}")
}

/// The active account's (login, scopes) from `gh auth status --json hosts`.
pub fn parse_status(json: &str, host: &str) -> Option<(String, Vec<String>)> {
    #[derive(Deserialize)]
    struct Root {
        hosts: std::collections::HashMap<String, Vec<Account>>,
    }
    #[derive(Deserialize)]
    struct Account {
        state: String,
        #[serde(default)]
        active: bool,
        login: Option<String>,
        #[serde(default)]
        scopes: String,
    }
    let root: Root = serde_json::from_str(json).ok()?;
    let accounts = root.hosts.get(host)?;
    let acct = accounts
        .iter()
        .find(|a| a.active && a.state == "success")
        .or_else(|| accounts.iter().find(|a| a.state == "success"))?;
    let scopes = acct
        .scopes
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Some((acct.login.clone()?, scopes))
}

/// `gh auth token` can wait on a macOS Keychain prompt, so it gets longer.
const TOKEN_TIMEOUT: Duration = Duration::from_secs(120);

/// The active account's token, straight from gh. Kept in memory by the caller.
pub async fn token(host: &str) -> Result<String, GhError> {
    let gh = find().ok_or(GhError::NotInstalled)?;
    let mut command = Command::new(gh);
    command.args(["auth", "token", "--hostname", host]);
    let out = cli::output(command, TOKEN_TIMEOUT).await?;
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || token.is_empty() {
        return Err(GhError::NotLoggedIn(host.to_string()));
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_active_account_and_scopes() {
        let json = r#"{"hosts":{"github.com":[
            {"state":"success","active":false,"host":"github.com","login":"other","tokenSource":"keyring","scopes":"repo"},
            {"state":"success","active":true,"host":"github.com","login":"example-builder","tokenSource":"keyring","scopes":"gist, read:org, repo, workflow","gitProtocol":"https"}
        ]}}"#;
        let (login, scopes) = parse_status(json, "github.com").unwrap();
        assert_eq!(login, "example-builder");
        assert_eq!(scopes, vec!["gist", "read:org", "repo", "workflow"]);
        assert!(!scopes.iter().any(|s| s == RUNNERS_SCOPE));
    }

    #[test]
    fn not_logged_in_or_other_host() {
        assert!(parse_status(r#"{"hosts":{}}"#, "github.com").is_none());
        assert!(
            parse_status(
                r#"{"hosts":{"github.com":[{"state":"error","active":true,"login":"x","scopes":""}]}}"#,
                "github.com"
            )
            .is_none()
        );
        assert!(parse_status("not json", "github.com").is_none());
    }

    #[test]
    fn api_base_to_host() {
        assert_eq!(host_for_api_base("https://api.github.com"), "github.com");
        assert_eq!(host_for_api_base("https://ghe.acme.dev/api/v3"), "ghe.acme.dev");
        assert_eq!(
            refresh_command("github.com"),
            "gh auth refresh --hostname github.com --scopes admin:org"
        );
    }
}
