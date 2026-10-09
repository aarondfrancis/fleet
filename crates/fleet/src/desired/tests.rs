//! Runner Fleet's test suite, ported. Every boundary is faked: these tests
//! never contact a real host or the GitHub organization.

mod decommission;
mod inventory;
mod operations;
mod plan;
mod remote;
mod windows_logon;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;

use super::inventory::{InventoryError, parse_inventory};
use super::model::{FleetSnapshot, GithubRunner, Host, Inventory, LocalRunner, Runner};
use super::operations::{Ops, Secrets};
use super::remote::{Programs, RemoteClient, Transient};
use super::testing::{FakeFleet, RecordingRunner, no_sleep};

const EXAMPLE: &str = include_str!("../../../../inventory/fleet.example.toml");

/// The organization every inventory written for these tests belongs to.
const ORGANIZATION: &str = r#"
[organization]
name = "acme"
url = "https://github.com/acme"
runner_group = "trusted"
"#;

/// An inventory with `hosts` (`[[hosts]]` tables) in the test organization.
fn inventory_toml(hosts: &str) -> String {
    format!("{ORGANIZATION}{hosts}")
}

/// One macOS host, `test`, with one runner, `test-01`.
fn base() -> String {
    inventory_toml(
        r#"
[[hosts]]
id = "test"
platform = "darwin-arm64"
ssh = "test-host"
root = "~/gh-runners/acme"
service_mode = "launchagent"
labels = ["fleet", "macos-build"]
runners = [
  { name = "test-01", directory = "runner-01" },
]
"#,
    )
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn example_inventory() -> Inventory {
    parse_inventory(EXAMPLE, &env(&[])).unwrap()
}

fn load(text: &str) -> Result<Inventory, InventoryError> {
    parse_inventory(text, &env(&[]))
}

fn load_err(text: &str) -> String {
    load(text).unwrap_err().0
}

fn gr(name: &str, status: &str, busy: bool, labels: &[&str], group: Option<bool>) -> GithubRunner {
    GithubRunner::new(11, name, status, busy, labels, group)
}

fn snapshot(github: Vec<GithubRunner>, local: Vec<LocalRunner>) -> FleetSnapshot {
    FleetSnapshot {
        github: github.into_iter().map(|g| (g.name.clone(), g)).collect(),
        local: local.into_iter().map(|l| (l.name.clone(), l)).collect(),
        ..Default::default()
    }
}

fn ops<'a>(inventory: &'a Inventory, fleet: &'a FakeFleet, secrets: Secrets) -> Ops<'a> {
    Ops {
        inventory,
        github: fleet,
        remote: fleet,
        secrets,
        sleep: &no_sleep,
    }
}

fn secret(value: &str) -> Transient {
    Transient::new(Some(value.into()))
}

/// The example inventory's `studio` host and its first runner.
fn studio(inventory: &Inventory) -> (&Host, &Runner) {
    let host = inventory.find_host("studio").unwrap();
    (host, &host.runners[0])
}

fn recording_remote() -> (Arc<RecordingRunner>, RemoteClient) {
    let commands = RecordingRunner::silent();
    let client = RemoteClient::new(commands.clone(), Duration::from_secs(20), Programs::default());
    (commands, client)
}

/// The last script sent, as given on stdin.
fn sent(commands: &RecordingRunner) -> String {
    commands.last().unwrap().input.unwrap_or_default()
}

/// The last script sent to a Windows host, decoded from its base64 line.
fn decoded(commands: &RecordingRunner) -> String {
    let input = sent(commands);
    String::from_utf8(base64::engine::general_purpose::STANDARD.decode(input.trim()).unwrap()).unwrap()
}
