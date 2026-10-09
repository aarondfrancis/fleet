//! Demo desired state: a fictional inventory and snapshot for the same
//! `acme-demo` machines as `demo.rs`, run through the real status and plan
//! code. Shown only when demo data is on.

use std::collections::HashMap;

use crate::desired::inventory::parse_inventory;
use chrono::{DateTime, Utc};

use crate::desired::model::{FleetSnapshot, GithubRunner, Inventory, LocalRunner};
use crate::fleet_plan::FleetPlanInputs;

const INVENTORY: &str = r#"
[organization]
name = "acme-demo"
url = "https://github.com/acme-demo"
runner_group = "Acme trusted builders"

[[hosts]]
id = "studio"
platform = "darwin-arm64"
ssh = "studio"
root = "~/gh-runners/acme-demo"
service_mode = "launchagent"
labels = ["fleet", "macos-build", "xcode"]
runners = [
  { name = "studio-1", directory = "runner-01", labels = ["macos-e2e"] },
  { name = "studio-2", directory = "runner-02", replaces = { name = "Studio-Old-2", directory = "~/actions-runner-2" } },
]

[[hosts]]
id = "mac-mini-02"
platform = "darwin-arm64"
ssh = "mac-mini-02"
root = "~/gh-runners/acme-demo"
service_mode = "launchagent"
labels = ["fleet", "macos-build"]
runners = [{ name = "mini-02", directory = "runner-01" }]

[[hosts]]
id = "build-linux-01"
platform = "linux-x64"
ssh = "builder@build-linux-01.demo.ts.net"
ssh_transport = "tailscale"
root = "~/gh-runners/acme-demo"
service_mode = "systemd"
labels = ["fleet", "linux-build", "ubuntu"]
runners = [
  { name = "linux-01-a", directory = "runner-01" },
  { name = "linux-01-b", directory = "runner-02", decommission = true },
]

[[hosts]]
id = "win-builder"
platform = "windows-x64"
ssh = "win-builder"
root = "C:\\gh-runners\\acme-demo"
service_mode = "windows-service"
service_account = ".\\builder"
labels = ["fleet", "windows-build"]
runners = [{ name = "win-builder", directory = "runner-01" }]

[[hosts]]
id = "mac-mini-03"
platform = "darwin-arm64"
ssh = "mac-mini-03"
root = "~/gh-runners/acme-demo"
service_mode = "launchagent"
labels = ["fleet", "macos-build"]
runners = [{ name = "mini-03-01", directory = "runner-01" }]
"#;

pub fn inventory() -> Inventory {
    parse_inventory(INVENTORY, &HashMap::new()).expect("demo inventory is valid")
}

pub fn snapshot() -> FleetSnapshot {
    let mac = ["self-hosted", "macOS", "ARM64", "fleet", "macos-build", "xcode"];
    let mut github = vec![
        group(GithubRunner::new(
            101,
            "studio-1",
            "online",
            true,
            &[&mac[..], &["macos-e2e"]].concat(),
            Some(true),
        )),
        group(GithubRunner::new(102, "studio-2", "online", false, &mac, Some(true))),
        group(GithubRunner::new(
            90,
            "Studio-Old-2",
            "online",
            false,
            &["self-hosted", "macOS"],
            Some(true),
        )),
        // Labels drifted: `macos-build` missing, a stray custom label present.
        group(GithubRunner::new(
            103,
            "mini-02",
            "online",
            false,
            &["self-hosted", "macOS", "fleet", "old-label"],
            Some(true),
        )),
        group(GithubRunner::new(
            104,
            "linux-01-a",
            "online",
            false,
            &["self-hosted", "Linux", "fleet", "linux-build", "ubuntu"],
            Some(true),
        )),
        group(GithubRunner::new(
            105,
            "linux-01-b",
            "online",
            false,
            &["self-hosted", "Linux", "fleet"],
            Some(true),
        )),
    ];
    for (id, name) in (200..).zip(["cloud-gpu-1", "hosted-pool-b7", "hosted-pool-c3"]) {
        github.push(GithubRunner::new(
            id,
            name,
            "online",
            id == 200,
            &["self-hosted"],
            Some(false),
        ));
    }
    let local = vec![
        LocalRunner::new(
            "studio-1",
            "~/gh-runners/acme-demo/runner-01",
            true,
            "active",
            Some("2.329.0"),
        ),
        LocalRunner::new(
            "studio-2",
            "~/gh-runners/acme-demo/runner-02",
            true,
            "active",
            Some("2.329.0"),
        ),
        LocalRunner::new("Studio-Old-2", "~/actions-runner-2", true, "active", Some("2.321.0")),
        LocalRunner::new(
            "mini-02",
            "~/gh-runners/acme-demo/runner-01",
            true,
            "active",
            Some("2.329.0"),
        ),
        LocalRunner::new(
            "linux-01-a",
            "~/gh-runners/acme-demo/runner-01",
            true,
            "active",
            Some("2.329.0"),
        ),
        LocalRunner::new(
            "linux-01-b",
            "~/gh-runners/acme-demo/runner-02",
            true,
            "active",
            Some("2.329.0"),
        ),
        LocalRunner::new("mini-03-01", "~/gh-runners/acme-demo/runner-01", false, "missing", None),
    ];
    FleetSnapshot {
        github: github.into_iter().map(|g| (g.name.clone(), g)).collect(),
        local: local.into_iter().map(|l| (l.name.clone(), l)).collect(),
        github_error: None,
        host_errors: [(
            "win-builder".to_string(),
            "ssh: connect to host win-builder port 22: Operation timed out".to_string(),
        )]
        .into(),
        policy_errors: Vec::new(),
    }
}

/// Desired-state inputs for demo mode: checked four minutes before `now`.
pub fn plan_inputs(now: DateTime<Utc>) -> FleetPlanInputs {
    let inventory = inventory();
    let checked = now - chrono::TimeDelta::minutes(4);
    FleetPlanInputs {
        inventory_path: "demo: inventory/fleet.toml".into(),
        host_checked_at: inventory.hosts.iter().map(|h| (h.id.clone(), checked)).collect(),
        inventory: Some(Ok(inventory)),
        snapshot: Some(snapshot()),
        checked_at: Some(checked),
        ..Default::default()
    }
}

/// GitHub marks fleet labels custom; the OS/arch ones are read-only.
fn group(mut runner: GithubRunner) -> GithubRunner {
    let builtin = ["self-hosted", "macOS", "Linux", "Windows", "ARM64", "X64"];
    runner.custom_labels = runner
        .labels
        .iter()
        .filter(|l| !builtin.contains(&l.as_str()))
        .cloned()
        .collect();
    runner
}
