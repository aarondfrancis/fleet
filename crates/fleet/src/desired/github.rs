//! The `gh` boundary for desired-state operations, ported from Runner Fleet's
//! `github.py`. Worker hosts never receive permanent credentials: only
//! short-lived registration/removal tokens, passed over SSH stdin.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Map, Value};

use super::model::GithubRunner;
use super::process::{CommandRunner, NonZeroExit};
use super::py::{py_str, repr, splitlines, truthy};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct GithubError(pub String);

type Result<T> = std::result::Result<T, GithubError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub version: String,
    pub name: String,
    pub url: String,
    pub sha256: Option<String>,
}

/// What operations need from GitHub. `GithubClient` is the real one.
pub trait FleetGithub: Send + Sync {
    fn auth_status(&self) -> Result<String>;
    fn list_runners(&self, desired_group: &str) -> Result<BTreeMap<String, GithubRunner>>;
    fn list_repository_runners(&self, repository: &str) -> Result<BTreeMap<String, GithubRunner>>;
    /// The group's id when it exists and its access policy is safe.
    fn validate_runner_group_policy(&self, name: &str) -> Result<i64>;
    fn latest_asset(&self, platform: &str) -> Result<ReleaseAsset>;
    fn registration_token(&self) -> Result<String>;
    fn removal_token(&self, repository: Option<&str>) -> Result<String>;
    fn set_labels(&self, runner_id: i64, labels: &[String]) -> Result<()>;
    fn assign_group(&self, group_id: i64, runner_id: i64) -> Result<()>;
    fn delete_runner(&self, runner_id: i64) -> Result<()>;
}

pub struct GithubClient {
    runner: Arc<dyn CommandRunner>,
    organization: String,
    gh: String,
    timeout: Duration,
}

impl std::fmt::Debug for GithubClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubClient")
            .field("organization", &self.organization)
            .field("gh", &self.gh)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl GithubClient {
    pub fn new(runner: Arc<dyn CommandRunner>, organization: &str, gh: &str, timeout: Duration) -> Self {
        Self {
            runner,
            organization: organization.into(),
            gh: gh.into(),
            timeout,
        }
    }

    fn cmd(&self, args: &[&str]) -> Vec<String> {
        std::iter::once(self.gh.clone())
            .chain(args.iter().map(ToString::to_string))
            .collect()
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        self.runner
            .run(&self.cmd(args), None, self.timeout, NonZeroExit::Error)
            .map(|r| r.stdout)
            .map_err(|e| GithubError(e.to_string()))
    }

    fn paginated(&self, endpoint: &str, jq: &str) -> Result<Vec<Map<String, Value>>> {
        let stdout = self.run(&["api", "--paginate", endpoint, "--jq", jq])?;
        let mut rows = Vec::new();
        for line in splitlines(&stdout) {
            if line.trim().is_empty() {
                continue;
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(line.trim())
                .map_err(|e| parse_error(&e.to_string()))?;
            let text = String::from_utf8(bytes).map_err(|e| parse_error(&e.to_string()))?;
            match serde_json::from_str(&text).map_err(|e| parse_error(&e.to_string()))? {
                Value::Object(map) => rows.push(map),
                _ => return Err(parse_error("expected an object")),
            }
        }
        Ok(rows)
    }

    fn json(&self, args: &[&str]) -> Result<Map<String, Value>> {
        let stdout = self.run(args)?;
        match serde_json::from_str(&stdout).map_err(|e| parse_error(&e.to_string()))? {
            Value::Object(map) => Ok(map),
            _ => Err(parse_error("expected an object")),
        }
    }

    fn find_runner_group(&self, name: &str) -> Result<Option<Map<String, Value>>> {
        let groups = self.paginated(
            &format!("orgs/{}/actions/runner-groups?per_page=100", self.organization),
            ".runner_groups[] | @base64",
        )?;
        Ok(groups
            .into_iter()
            .find(|g| g.get("name").and_then(Value::as_str) == Some(name)))
    }
}

fn parse_error(detail: &str) -> GithubError {
    GithubError(format!("could not parse GitHub API response: {detail}"))
}

fn as_id(value: Option<&Value>) -> Result<i64> {
    match value {
        Some(Value::Number(n)) => n.as_i64().ok_or_else(|| parse_error("runner id is not an integer")),
        Some(Value::String(s)) => s.trim().parse().map_err(|_| parse_error("runner id is not an integer")),
        _ => Err(parse_error("missing id")),
    }
}

fn label_names(item: &Map<String, Value>, keep: impl Fn(&Map<String, Value>) -> bool) -> BTreeSet<String> {
    item.get("labels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter(|l| keep(l))
        .filter_map(|l| l.get("name").map(py_str))
        .collect()
}

fn runner_records(
    items: Vec<Map<String, Value>>,
    group_members: Option<&HashSet<i64>>,
) -> Result<BTreeMap<String, GithubRunner>> {
    let mut runners = BTreeMap::new();
    for item in items {
        let id = as_id(item.get("id"))?;
        let name = item
            .get("name")
            .map(py_str)
            .ok_or_else(|| parse_error("missing name"))?;
        let runner = GithubRunner {
            id,
            name: name.clone(),
            status: item.get("status").map(py_str).unwrap_or_else(|| "unknown".into()),
            busy: item.get("busy").is_some_and(truthy),
            labels: label_names(&item, |_| true),
            in_desired_group: group_members.map(|m| m.contains(&id)),
            custom_labels: label_names(&item, |l| l.get("type").and_then(Value::as_str) == Some("custom")),
        };
        runners.insert(name, runner);
    }
    Ok(runners)
}

/// GitHub represents "all repositories, public disabled" as
/// `visibility = all` plus `allows_public_repositories = false`.
fn group_policy_issues(group: &Map<String, Value>) -> Vec<&'static str> {
    let mut issues = Vec::new();
    if group.get("visibility").and_then(Value::as_str) != Some("all") {
        issues.push("repository access must include all organization repositories");
    }
    if group.get("allows_public_repositories") != Some(&Value::Bool(false)) {
        issues.push("public repositories must be disabled");
    }
    if group.get("restricted_to_workflows") != Some(&Value::Bool(false)) {
        issues.push("workflow access must include all workflows in private repositories");
    }
    issues
}

fn token(data: &Map<String, Value>) -> Result<String> {
    match data.get("token") {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        _ => Err(GithubError("GitHub did not return a runner token".into())),
    }
}

fn missing_group(name: &str) -> GithubError {
    GithubError(format!(
        "runner group {} does not exist; create and restrict it in GitHub first",
        repr(name)
    ))
}

impl FleetGithub for GithubClient {
    fn auth_status(&self) -> Result<String> {
        let result = self
            .runner
            .run(
                &self.cmd(&["auth", "status", "-h", "github.com"]),
                None,
                self.timeout,
                NonZeroExit::Return,
            )
            .map_err(|e| GithubError(e.to_string()))?;
        let output = format!("{}{}", result.stdout, result.stderr).trim().to_string();
        if result.returncode != 0 {
            return Err(GithubError(if output.is_empty() {
                "gh is not authenticated".into()
            } else {
                output
            }));
        }
        Ok(output)
    }

    fn list_runners(&self, desired_group: &str) -> Result<BTreeMap<String, GithubRunner>> {
        let members = match self.find_runner_group(desired_group)? {
            Some(group) => {
                let id = as_id(group.get("id"))?;
                let rows = self.paginated(
                    &format!(
                        "orgs/{}/actions/runner-groups/{id}/runners?per_page=100",
                        self.organization
                    ),
                    ".runners[] | @base64",
                )?;
                Some(
                    rows.iter()
                        .map(|r| as_id(r.get("id")))
                        .collect::<Result<HashSet<i64>>>()?,
                )
            }
            None => None,
        };
        let rows = self.paginated(
            &format!("orgs/{}/actions/runners?per_page=100", self.organization),
            ".runners[] | @base64",
        )?;
        runner_records(rows, members.as_ref())
    }

    fn list_repository_runners(&self, repository: &str) -> Result<BTreeMap<String, GithubRunner>> {
        let rows = self.paginated(
            &format!("repos/{repository}/actions/runners?per_page=100"),
            ".runners[] | @base64",
        )?;
        runner_records(rows, None)
    }

    fn validate_runner_group_policy(&self, name: &str) -> Result<i64> {
        let group = self.find_runner_group(name)?.ok_or_else(|| missing_group(name))?;
        let issues = group_policy_issues(&group);
        if !issues.is_empty() {
            return Err(GithubError(format!(
                "runner group {} is unsafe: {}",
                repr(name),
                issues.join("; ")
            )));
        }
        as_id(group.get("id"))
    }

    fn latest_asset(&self, platform: &str) -> Result<ReleaseAsset> {
        let data = self.json(&["api", "repos/actions/runner/releases/latest"])?;
        let tag = data.get("tag_name").map(py_str).unwrap_or_default();
        let version = tag.strip_prefix('v').unwrap_or(&tag).to_string();
        let suffix = if platform.starts_with("win-") {
            ".zip"
        } else {
            ".tar.gz"
        };
        let expected = format!("actions-runner-{platform}-{version}{suffix}");
        let asset = data
            .get("assets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .find(|a| a.get("name").and_then(Value::as_str) == Some(expected.as_str()));
        let (true, Some(asset)) = (!version.is_empty(), asset) else {
            return Err(GithubError(format!(
                "latest runner release does not contain {expected}"
            )));
        };
        let sha256 = asset
            .get("digest")
            .and_then(Value::as_str)
            .and_then(|d| d.strip_prefix("sha256:"))
            .filter(|c| c.len() == 64)
            .map(str::to_string);
        let url = asset
            .get("browser_download_url")
            .map(py_str)
            .ok_or_else(|| parse_error("missing browser_download_url"))?;
        Ok(ReleaseAsset {
            version,
            name: expected,
            url,
            sha256,
        })
    }

    fn registration_token(&self) -> Result<String> {
        let endpoint = format!("orgs/{}/actions/runners/registration-token", self.organization);
        token(&self.json(&["api", "-X", "POST", &endpoint])?)
    }

    fn removal_token(&self, repository: Option<&str>) -> Result<String> {
        let endpoint = match repository {
            Some(repo) => format!("repos/{repo}/actions/runners/remove-token"),
            None => format!("orgs/{}/actions/runners/remove-token", self.organization),
        };
        token(&self.json(&["api", "-X", "POST", &endpoint])?)
    }

    fn set_labels(&self, runner_id: i64, labels: &[String]) -> Result<()> {
        let endpoint = format!("orgs/{}/actions/runners/{runner_id}/labels", self.organization);
        let fields: Vec<String> = labels.iter().map(|l| format!("labels[]={l}")).collect();
        let mut args = vec!["api", "-X", "PUT", endpoint.as_str()];
        for field in &fields {
            args.push("-f");
            args.push(field);
        }
        self.run(&args).map(drop)
    }

    fn assign_group(&self, group_id: i64, runner_id: i64) -> Result<()> {
        let endpoint = format!(
            "orgs/{}/actions/runner-groups/{group_id}/runners/{runner_id}",
            self.organization
        );
        self.run(&["api", "-X", "PUT", &endpoint]).map(drop)
    }

    fn delete_runner(&self, runner_id: i64) -> Result<()> {
        let endpoint = format!("orgs/{}/actions/runners/{runner_id}", self.organization);
        self.run(&["api", "-X", "DELETE", &endpoint]).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desired::testing::RecordingRunner;
    use serde_json::json;

    fn encoded(items: &[Value]) -> String {
        items
            .iter()
            .map(|i| base64::engine::general_purpose::STANDARD.encode(i.to_string()))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    /// Canned `gh api` answers; `unsafe_group` opens the trusted group to selected repositories only.
    fn fake_gh(unsafe_group: bool) -> Arc<RecordingRunner> {
        RecordingRunner::new(move |command| {
            let joined = command.join(" ");
            if joined.contains("runner-groups?") && unsafe_group {
                encoded(&[json!({"id": 7, "name": "trusted", "visibility": "selected",
                    "allows_public_repositories": false, "restricted_to_workflows": true, "selected_workflows": []})])
            } else if joined.contains("runner-groups?") {
                encoded(&[
                    json!({"id": 1, "name": "Default", "default": true, "visibility": "selected",
                        "allows_public_repositories": false, "restricted_to_workflows": true, "selected_workflows": []}),
                    json!({"id": 7, "name": "trusted", "default": false, "visibility": "all",
                        "allows_public_repositories": false, "restricted_to_workflows": false, "selected_workflows": []}),
                ])
            } else if joined.contains("runner-groups/7/runners") {
                encoded(&[json!({"id": 1, "name": "mac-01"})])
            } else if joined.contains("repos/acme/desktop/actions/runners?") {
                encoded(&[
                    json!({"id": 8, "name": "old-windows-01", "status": "online", "busy": false,
                    "labels": [{"name": "self-hosted", "type": "read-only"}, {"name": "windows-build", "type": "custom"}]}),
                ])
            } else if joined.contains("repos/acme/desktop/actions/runners/remove-token") {
                json!({"token": "repository-removal-token"}).to_string()
            } else if joined.contains("actions/runners?") {
                encoded(&[json!({"id": 1, "name": "mac-01", "status": "online", "busy": false,
                    "labels": [{"name": "self-hosted", "type": "read-only"}, {"name": "fleet", "type": "custom"}]})])
            } else if joined.contains("releases/latest") {
                json!({"tag_name": "v2.999.0", "assets": [{"name": "actions-runner-osx-arm64-2.999.0.tar.gz",
                    "browser_download_url": "https://example.invalid/runner.tar.gz", "digest": format!("sha256:{}", "a".repeat(64))}]})
                .to_string()
            } else {
                panic!("unexpected command {command:?}")
            }
        })
    }

    fn client(commands: Arc<RecordingRunner>) -> GithubClient {
        GithubClient::new(commands, "acme", "gh", Duration::from_secs(30))
    }

    #[test]
    fn runner_group_membership_and_labels_are_parsed() {
        let runners = client(fake_gh(false)).list_runners("trusted").unwrap();
        let runner = &runners["mac-01"];
        assert_eq!(runner.in_desired_group, Some(true));
        assert_eq!(runner.labels, ["self-hosted", "fleet"].map(String::from).into());
        assert_eq!(runner.custom_labels, ["fleet"].map(String::from).into());
    }

    #[test]
    fn repository_scoped_legacy_runner_and_removal_token() {
        let c = client(fake_gh(false));
        let runner = &c.list_repository_runners("acme/desktop").unwrap()["old-windows-01"];
        assert_eq!(runner.status, "online");
        assert_eq!(runner.in_desired_group, None);
        assert_eq!(
            c.removal_token(Some("acme/desktop")).unwrap(),
            "repository-removal-token"
        );
    }

    #[test]
    fn latest_asset_uses_release_metadata_digest() {
        let asset = client(fake_gh(false)).latest_asset("osx-arm64").unwrap();
        assert_eq!(asset.version, "2.999.0");
        assert_eq!(asset.sha256, Some("a".repeat(64)));
    }

    #[test]
    fn runner_group_security_policy_is_enforced() {
        assert_eq!(
            client(fake_gh(false)).validate_runner_group_policy("trusted").unwrap(),
            7
        );
    }

    #[test]
    fn unsafe_runner_group_is_rejected_before_mutation() {
        let e = client(fake_gh(true))
            .validate_runner_group_policy("trusted")
            .unwrap_err();
        assert!(e.0.contains("all organization repositories"), "{e}");
    }

    #[test]
    fn missing_group_leaves_membership_unknown() {
        let runners = client(fake_gh(false)).list_runners("nope").unwrap();
        assert_eq!(runners["mac-01"].in_desired_group, None);
    }

    #[test]
    fn reads_use_gh_api_and_never_post() {
        let commands = fake_gh(false);
        client(commands.clone()).list_runners("trusted").unwrap();
        for run in commands.runs() {
            assert_eq!(&run.command[..3], ["gh", "api", "--paginate"]);
            assert!(!run.command.iter().any(|a| a == "-X"));
        }
    }
}
