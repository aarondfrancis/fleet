//! GitHub sign-in through a *stub* `gh` (never the real CLI or a real login):
//! token fetched from gh, nothing stored in the secret store, scope problems
//! explained with the exact `gh auth refresh` command.

#![expect(clippy::unwrap_used, reason = "test fixtures fail the test loudly")]

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use fleet::engine::{Engine, MemorySecrets, SecretStore};
use fleet::gh;
use fleet::model::{AccountKind, AccountSetup, OrgAuth, RepoScope};
use fleet::sync::MutexExt;
use serde_json::json;

mod common;

fn stub_gh(dir: &std::path::Path, token: &str, scopes: &str) -> std::path::PathBuf {
    let path = dir.join("gh");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n  \"auth token\") echo {token} ;;\n  \"auth status\") echo '{{\"hosts\":{{\"github.com\":[{{\"state\":\"success\",\"active\":true,\"host\":\"github.com\",\"login\":\"stub-user\",\"tokenSource\":\"keyring\",\"scopes\":\"{scopes}\"}}]}}}}' ;;\n  *) exit 1 ;;\nesac\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

async fn mock_github(allowed: Arc<Mutex<bool>>, seen: Arc<Mutex<Vec<String>>>) -> String {
    let app = Router::new().route(
        "/orgs/{org}/actions/runners",
        get(move |headers: HeaderMap| {
            let allowed = allowed.clone();
            let seen = seen.clone();
            async move {
                seen.locked()
                    .push(headers["authorization"].to_str().unwrap().to_string());
                if *allowed.locked() {
                    (StatusCode::OK, Json(json!({"total_count": 0, "runners": []})))
                } else {
                    (
                        StatusCode::FORBIDDEN,
                        Json(json!({"message": "Must have admin rights to Repository."})),
                    )
                }
            }
        }),
    );
    common::serve(app).await
}

#[tokio::test]
async fn gh_login_is_used_and_never_stored() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: this file holds a single test, so this test binary runs it alone,
    // and the variable is set before the runtime or any client starts threads.
    unsafe {
        std::env::set_var(
            gh::GH_PATH_ENV,
            stub_gh(dir.path(), "gho_stubtoken", "gist, read:org, repo"),
        )
    };

    let st = gh::status("github.com").await;
    assert!(st.installed);
    assert_eq!(st.login.as_deref(), Some("stub-user"));
    assert!(st.missing_runners_scope);
    assert_eq!(
        st.fix.as_deref(),
        Some("gh auth refresh --hostname github.com --scopes admin:org")
    );

    let allowed = Arc::new(Mutex::new(false));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let base = mock_github(allowed.clone(), seen.clone()).await;
    let secrets = Arc::new(MemorySecrets::default());
    let engine = Engine::load(dir.path().join("config.json"), secrets.clone()).unwrap();

    // Missing scope: refused with the exact fix, nothing saved.
    let err = engine
        .add_org(
            "acme",
            OrgAuth::GhCli,
            None,
            AccountSetup {
                kind: AccountKind::Organization,
                repo_scope: RepoScope::None,
                runner_repos: Vec::new(),
            },
            Some(base.clone()),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("Run `gh auth refresh --hostname 127.0.0.1 --scopes admin:org` in Terminal."),
        "{err}"
    );
    assert!(engine.config().orgs.is_empty());

    // After the user refreshes the scope, it works and uses gh's token.
    *allowed.locked() = true;
    engine
        .add_org(
            "acme",
            OrgAuth::GhCli,
            None,
            AccountSetup {
                kind: AccountKind::Organization,
                repo_scope: RepoScope::None,
                runner_repos: Vec::new(),
            },
            Some(base),
        )
        .await
        .unwrap();
    assert_eq!(engine.config().orgs[0].auth, OrgAuth::GhCli);
    assert_eq!(seen.locked().last().unwrap(), "Bearer gho_stubtoken");
    assert_eq!(secrets.get("acme").unwrap(), None, "gh's token isn't copied");

    // Polling keeps working from gh.
    engine.poll_github_runners().await;
    let view = engine.view();
    assert!(view.orgs[0].error.is_none(), "{:?}", view.orgs[0].error);
    assert_eq!(view.orgs[0].auth, OrgAuth::GhCli);
}
