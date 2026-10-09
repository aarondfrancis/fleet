//! Desired state through the engine: check, review, execute. GitHub and the
//! hosts are faked; nothing here contacts a real machine or organization.

#![expect(clippy::unwrap_used, reason = "test fixtures fail the test loudly")]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fleet::desired::github::ReleaseAsset;
use fleet::desired::model::{FleetPlanAction, GithubRunner, LocalRunner};
use fleet::desired::remote::{InstallRequest, Transient};
use fleet::desired::testing::{FakeFleet, no_sleep};
use fleet::engine::desired::{ChangeSecrets, DesiredIo, SshTransportIdentity};
use fleet::engine::{Engine, MemorySecrets};
use fleet::fleet_plan::{ChangeRequest, MachineDesired, OperationStatus, OperationView};
use fleet::model::{DataSource, MachineConnection, MachineView};
use fleet::sync::MutexExt;

const EXAMPLE_INVENTORY: &str = include_str!("../../../inventory/fleet.example.toml");

const INVENTORY: &str = r#"
[organization]
name = "acme"
url = "https://github.com/acme"
runner_group = "trusted"

[[hosts]]
id = "mini"
platform = "darwin-arm64"
ssh = "mini"
root = "~/gh-runners/acme"
service_mode = "launchagent"
labels = ["fleet", "macos-build"]
runners = [
  { name = "mini-01", directory = "runner-01" },
  { name = "mini-02", directory = "runner-02", replaces = { name = "Old-Mini", directory = "~/actions-runner" } },
]

[[hosts]]
id = "linux"
platform = "linux-x64"
ssh = "builder@linux.example.ts.net"
root = "~/gh-runners/acme"
service_mode = "systemd"
labels = ["fleet", "linux-build"]
runners = [{ name = "linux-01", directory = "runner-01", decommission = true }]
"#;

/// What `ssh -G` reports for each target.
#[derive(Default)]
struct SshConfig {
    /// Target → the hostname it resolves to (default: `mini` is the Mac mini).
    hosts: BTreeMap<String, String>,
    /// Target → its complete effective configuration, overriding `hosts`.
    configs: BTreeMap<String, String>,
}

struct Fixture {
    _tmp: tempfile::TempDir,
    engine: Arc<Engine>,
    fleet: FakeFleet,
    ssh: Arc<Mutex<SshConfig>>,
}

fn fixture(inventory: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("fleet.toml");
    std::fs::write(&path, inventory).unwrap();
    let fleet = FakeFleet::default();
    let ssh: Arc<Mutex<SshConfig>> = Arc::default();
    let boundaries = fleet.clone();
    let resolver = ssh.clone();
    let engine = Engine::load(tmp.path().join("config.json"), Arc::new(MemorySecrets::default()))
        .unwrap()
        .with_desired_io(DesiredIo {
            boundaries: Arc::new(move |_| (Arc::new(boundaries.clone()) as _, Arc::new(boundaries.clone()) as _)),
            sleep: no_sleep,
            resolve_ssh_transport: Arc::new(move |target: &str| {
                let ssh = resolver.locked();
                if let Some(config) = ssh.configs.get(target) {
                    return SshTransportIdentity::from_config(config);
                }
                let hostname = ssh.hosts.get(target).map(String::as_str).unwrap_or_else(|| {
                    if target == "mini" {
                        "mac-mini.example.ts.net"
                    } else {
                        target
                    }
                });
                SshTransportIdentity::from_config(&format!(
                    "hostname {hostname}\nport 22\nuser fake-ci\nproxycommand none\nproxyjump none\n"
                ))
            }),
        });
    engine.set_inventory_path(Some(path.display().to_string())).unwrap();
    Fixture {
        _tmp: tmp,
        engine: Arc::new(engine),
        fleet,
        ssh,
    }
}

fn healthy(fleet: &FakeFleet, name: &str, labels: &[&str]) {
    let mut world = fleet.world();
    world.github.insert(
        name.into(),
        GithubRunner::new(7, name, "online", false, labels, Some(true)),
    );
    world.local.insert(
        name.into(),
        LocalRunner::new(name, "p", true, "active", Some("2.330.0")),
    );
}

fn no_secrets() -> ChangeSecrets {
    ChangeSecrets::default()
}

/// The machine for inventory host `id`.
fn machine(engine: &Engine, id: &str) -> MachineView {
    engine
        .view()
        .machines
        .into_iter()
        .find(|m| m.desired.as_ref().is_some_and(|d| d.host.id == id))
        .unwrap_or_else(|| panic!("no machine for host {id}"))
}

fn desired(engine: &Engine, id: &str) -> MachineDesired {
    machine(engine, id).desired.unwrap()
}

async fn wait_done(engine: &Engine) -> OperationView {
    for _ in 0..200 {
        let plan = engine.view().plan;
        if let Some(op) = plan.operation.filter(|o| o.status != OperationStatus::Running)
            && !plan.refreshing
        {
            return op;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("operation didn't finish");
}

#[tokio::test]
async fn check_puts_plan_and_rows_on_machines_and_runners() {
    let f = fixture(INVENTORY);
    healthy(&f.fleet, "mini-01", &["fleet", "macos-build"]);
    let before = desired(&f.engine, "mini");
    assert!(f.engine.view().plan.checked_at.is_none());
    assert!(before.plan.is_empty(), "nothing is invented before the first check");
    // Declared runners are listed from the inventory alone, state unknown.
    let names: Vec<String> = machine(&f.engine, "mini").runners.into_iter().map(|r| r.name).collect();
    assert_eq!(names, ["mini-01", "mini-02"]);

    f.engine.desired_refresh(None).await.unwrap();
    let view = f.engine.view();
    assert_eq!(view.plan.organization.unwrap().name, "acme");
    let mini = desired(&f.engine, "mini");
    let actions: Vec<(&str, &str)> = mini
        .plan
        .iter()
        .map(|i| (i.runner.as_str(), i.action.as_str()))
        .collect();
    assert_eq!(actions, [("mini-01", "no-change"), ("mini-02", "install")]);
    assert_eq!(mini.pending_changes, 1);
    // Each runner carries its own row and plan items.
    let runners = machine(&f.engine, "mini").runners;
    let mini_02 = runners
        .iter()
        .find(|r| r.name == "mini-02")
        .unwrap()
        .desired
        .clone()
        .unwrap();
    assert_eq!(mini_02.status.unwrap().drift, "install");
    assert_eq!(mini_02.desired_labels, ["fleet", "macos-build"]);
    // Runner checks do not invent a successful machine-health sample.
    assert!(matches!(
        machine(&f.engine, "mini").connection,
        MachineConnection::Pending
    ));
}

#[tokio::test]
async fn reviewed_apply_installs_then_reconciles_and_reports() {
    let f = fixture(INVENTORY);
    healthy(&f.fleet, "mini-01", &["fleet", "macos-build"]);
    let review = f
        .engine
        .desired_review(ChangeRequest::Apply {
            host: "mini".into(),
            runner: Some("mini-02".into()),
        })
        .await
        .unwrap();
    assert!(review.ready, "{:?}", review.blockers);
    assert_eq!(review.items[0].action, FleetPlanAction::Install);
    assert!(f.fleet.world().tokens.is_empty(), "reviewing never mints a token");

    f.engine.desired_execute(&review.id, no_secrets(), None).await.unwrap();
    let op = wait_done(&f.engine).await;
    assert_eq!(op.status, OperationStatus::Succeeded, "{:?}", op.error);
    assert!(op.messages[0].starts_with("installed mini-02 v2.999.0"));
    let calls = f.fleet.world().calls.clone();
    assert_eq!(
        calls,
        [
            "install mini mini-02 token=short-lived-registration",
            "labels 44 fleet,macos-build",
            "group 5 44",
            "start mini mini-02"
        ]
    );
    // The check after the change shows the new state.
    let mini = desired(&f.engine, "mini");
    assert!(
        mini.plan
            .iter()
            .any(|i| i.runner == "mini-02" && i.action == FleetPlanAction::NoChange)
    );

    // A review is used once.
    let again = f.engine.desired_execute(&review.id, no_secrets(), None).await;
    assert!(again.unwrap_err().to_string().contains("review the change again"));
}

#[tokio::test]
async fn execute_refuses_when_the_host_changed_since_review() {
    let f = fixture(INVENTORY);
    healthy(&f.fleet, "mini-01", &["fleet", "macos-build"]);
    let review = f
        .engine
        .desired_review(ChangeRequest::Apply {
            host: "mini".into(),
            runner: Some("mini-02".into()),
        })
        .await
        .unwrap();
    // Someone registers the runner by hand in the meantime.
    f.fleet.world().github.insert(
        "mini-02".into(),
        GithubRunner::new(8, "mini-02", "online", false, &[], Some(true)),
    );
    let e = f
        .engine
        .desired_execute(&review.id, no_secrets(), None)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("changed since you reviewed it"), "{e}");
    assert!(f.fleet.world().calls.is_empty());
}

#[tokio::test]
async fn migrating_host_needs_one_runner_at_a_time() {
    let f = fixture(INVENTORY);
    let review = f
        .engine
        .desired_review(ChangeRequest::Apply {
            host: "mini".into(),
            runner: None,
        })
        .await
        .unwrap();
    assert!(!review.ready);
    assert_eq!(
        review.blockers,
        ["mini has a capacity-preserving migration; select exactly one --runner"]
    );
}

#[tokio::test]
async fn nothing_to_change_is_not_runnable() {
    let f = fixture(INVENTORY);
    healthy(&f.fleet, "mini-01", &["fleet", "macos-build"]);
    let review = f
        .engine
        .desired_review(ChangeRequest::Apply {
            host: "mini".into(),
            runner: Some("mini-01".into()),
        })
        .await
        .unwrap();
    assert!(!review.ready);
    assert!(review.blockers[0].contains("Nothing to change"));
}

#[tokio::test]
async fn retire_needs_a_healthy_replacement_and_the_exact_phrase() {
    let f = fixture(INVENTORY);
    healthy(&f.fleet, "Old-Mini", &[]);
    let review = f
        .engine
        .desired_review(ChangeRequest::Retire {
            host: "mini".into(),
            runner: "mini-02".into(),
        })
        .await
        .unwrap();
    assert!(!review.ready, "replacement isn't installed yet");
    assert!(review.blockers[0].contains("keep capacity until mini-02"));

    healthy(&f.fleet, "mini-02", &["fleet", "macos-build"]);
    let review = f
        .engine
        .desired_review(ChangeRequest::Retire {
            host: "mini".into(),
            runner: "mini-02".into(),
        })
        .await
        .unwrap();
    assert!(review.ready, "{:?}", review.blockers);
    assert_eq!(review.confirm_phrase.as_deref(), Some("retire Old-Mini"));
    let none = no_secrets();
    let e = f
        .engine
        .desired_execute(&review.id, none.clone(), Some("retire old-mini".into()))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("exactly"));

    let review = f
        .engine
        .desired_review(ChangeRequest::Retire {
            host: "mini".into(),
            runner: "mini-02".into(),
        })
        .await
        .unwrap();
    f.engine
        .desired_execute(&review.id, none, Some("retire Old-Mini".into()))
        .await
        .unwrap();
    let op = wait_done(&f.engine).await;
    assert_eq!(op.status, OperationStatus::Succeeded, "{:?}", op.error);
    assert_eq!(
        op.messages,
        ["retired Old-Mini; preserved ~/actions-runner for recovery"]
    );
    assert_eq!(f.fleet.world().tokens, ["removal"]);
}

#[tokio::test]
async fn decommission_passes_the_sudo_password_once_and_keeps_it_out_of_views() {
    let f = fixture(INVENTORY);
    healthy(&f.fleet, "linux-01", &[]);
    let review = f
        .engine
        .desired_review(ChangeRequest::Decommission {
            host: "linux".into(),
            runner: "linux-01".into(),
        })
        .await
        .unwrap();
    assert!(review.ready, "{:?}", review.blockers);
    assert!(review.accepts_sudo_password);
    let secrets = ChangeSecrets {
        service_password: None,
        sudo_password: Some("hunter2".into()),
    };
    f.engine
        .desired_execute(&review.id, secrets, Some("decommission linux-01".into()))
        .await
        .unwrap();
    let op = wait_done(&f.engine).await;
    assert_eq!(op.status, OperationStatus::Succeeded, "{:?}", op.error);
    assert_eq!(
        f.fleet.world().calls,
        ["decommission linux linux-01 token=short-lived-removal sudo-password=hunter2"]
    );
    let view = serde_json::to_string(&f.engine.view()).unwrap();
    assert!(!view.contains("hunter2"));
    let config = std::fs::read_to_string(f._tmp.path().join("config.json")).unwrap();
    assert!(!config.contains("hunter2"));
}

#[tokio::test]
async fn restart_refuses_a_busy_runner() {
    let f = fixture(INVENTORY);
    healthy(&f.fleet, "mini-01", &["fleet", "macos-build"]);
    f.fleet.world().github.get_mut("mini-01").unwrap().busy = true;
    let review = f
        .engine
        .desired_review(ChangeRequest::Restart {
            runner: "mini-01".into(),
        })
        .await
        .unwrap();
    assert!(!review.ready);
    assert_eq!(review.blockers, ["mini-01 is busy; refusing restart"]);
}

#[tokio::test]
async fn demo_mode_and_bad_inventory_are_reported_not_hidden() {
    let f = fixture(INVENTORY);
    f.engine.set_demo_mode(true).unwrap();
    assert_eq!(
        f.engine.desired_refresh(None).await.unwrap_err().to_string(),
        "Turn off demo data to check or change real machines."
    );
    assert_eq!(f.engine.view().source, DataSource::Demo);
    f.engine.set_demo_mode(false).unwrap();

    let g = fixture(&INVENTORY.replace("\"fleet\", \"macos-build\"", "\"macos-build\""));
    let view = g.engine.view();
    assert_eq!(
        view.plan.inventory_error.as_deref(),
        Some("mini: missing required labels: fleet")
    );
    assert_eq!(
        g.engine.desired_refresh(None).await.unwrap_err().to_string(),
        "mini: missing required labels: fleet"
    );
}

#[tokio::test]
async fn logs_are_read_only() {
    let f = fixture(INVENTORY);
    let text = f.engine.desired_logs("mini-01".into(), 50).await.unwrap();
    assert_eq!(text, "==> mini-01 last 50 <==\n");
    assert_eq!(
        f.engine
            .desired_logs("mini-01".into(), 0)
            .await
            .unwrap_err()
            .to_string(),
        "--lines must be between 1 and 10000"
    );
    assert!(f.fleet.world().calls.is_empty());
}

#[test]
fn inventory_hosts_match_tailnet_machines_by_their_ssh_target() {
    use fleet::fleet_plan::peer_for_host;
    use fleet::model::TailnetPeer;
    let peer = |id: &str, host: &str, dns: &str| TailnetPeer {
        node_id: id.into(),
        hostname: host.into(),
        dns_name: Some(dns.into()),
        os: "linux".into(),
        ips: vec![format!("100.64.0.{}", id.len())],
        online: true,
        last_seen: None,
        tags: vec![],
        ssh_host_keys: vec![],
    };
    let peers = vec![
        peer("n1", "Build Studio", "studio.example.ts.net"),
        peer("n2", "linux", "linux.example.ts.net"),
        peer("n3", "win-builder", "win-builder.example.ts.net"),
        peer("n4", "Build Mac Mini", "mac-mini.example.ts.net"),
    ];
    // The shared example covers short aliases, user@MagicDNS and a Tailscale host name.
    let inventory = fleet::desired::inventory::parse_inventory(EXAMPLE_INVENTORY, &Default::default()).unwrap();
    let matched = |id: &str| peer_for_host(inventory.find_host(id).unwrap(), None, &peers).map(|p| p.node_id.clone());
    assert_eq!(
        matched("studio").as_deref(),
        Some("n1"),
        "short alias matches the MagicDNS name"
    );
    assert_eq!(matched("linux").as_deref(), Some("n2"), "user@fqdn matches");
    assert_eq!(matched("windows").as_deref(), Some("n3"), "alias win-builder matches");
    assert_eq!(matched("linux-canary"), None);
    assert_eq!(matched("mini").as_deref(), Some("n4"), "user@MagicDNS name matches");
    // An alias like `ssh = "mini"` only names the machine once SSH resolves it.
    let mut alias = inventory.find_host("mini").unwrap().clone();
    alias.ssh_target = Some("mini".into());
    assert_eq!(peer_for_host(&alias, None, &peers), None);
    let resolved = peer_for_host(&alias, Some("mac-mini.example.ts.net"), &peers);
    assert_eq!(resolved.map(|p| p.node_id.as_str()), Some("n4"));
}

#[test]
fn debug_output_never_shows_secrets() {
    use fleet::engine::SecretStore;
    let asset = ReleaseAsset {
        version: "2.9".into(),
        name: "r".into(),
        url: "https://x".into(),
        sha256: None,
    };
    let password = Transient::new(Some("hunter2".into()));
    let request = InstallRequest {
        asset: &asset,
        token: "AAAREGTOKEN",
        organization_url: "o",
        runner_group: "g",
        service_password: &password,
        sudo_password: &password,
    };
    let secrets = MemorySecrets::default();
    secrets.set("acme", "ghp_secret").unwrap();
    let client = fleet::github::GithubClient::new("https://api.github.com", "ghp_secret");
    let change = ChangeSecrets {
        service_password: Some("service-secret".into()),
        sudo_password: Some("sudo-secret".into()),
    };
    let shown = format!("{request:?} {secrets:?} {client:?} {change:?}");
    for secret in ["hunter2", "AAAREGTOKEN", "ghp_secret", "service-secret", "sudo-secret"] {
        assert!(!shown.contains(secret), "{shown}");
    }
}

#[tokio::test]
async fn ssh_aliases_resolve_to_their_tailnet_machine() {
    use fleet::model::{TailnetPeer, TailnetSnapshot};
    let inventory = INVENTORY;
    let f = fixture(inventory);
    let engine = Arc::try_unwrap(f.engine).unwrap().with_tailscale_source(|| async {
        Ok(TailnetSnapshot {
            self_node_id: None,
            local_online: Some(true),
            tailnet_name: None,
            peers: vec![TailnetPeer {
                node_id: "nMini".into(),
                hostname: "Build Mac Mini".into(),
                dns_name: Some("mac-mini.example.ts.net".into()),
                os: "macOS".into(),
                ips: vec!["100.64.0.9".into()],
                online: true,
                last_seen: None,
                tags: vec![],
                ssh_host_keys: vec![],
            }],
        })
    });
    let engine = Arc::new(engine);
    engine.poll_tailscale().await;
    // Before SSH resolves the alias, the host and the tailnet machine are separate.
    assert_eq!(machine(&engine, "mini").id, "inv:mini", "before SSH is resolved");
    assert_eq!(engine.view().other_machines.len(), 1);
    engine.desired_refresh(None).await.unwrap();
    // After: one machine, keyed by its tailnet node.
    let mini = machine(&engine, "mini");
    assert_eq!(mini.id, "ts:nMini");
    assert_eq!(mini.tailnet.unwrap().node_id, "nMini");
    assert!(engine.view().other_machines.is_empty());
}

async fn review_restart(f: &Fixture) -> fleet::engine::desired::ReviewView {
    healthy(&f.fleet, "mini-01", &["fleet", "macos-build"]);
    let review = f
        .engine
        .desired_review(ChangeRequest::Restart {
            runner: "mini-01".into(),
        })
        .await
        .unwrap();
    assert!(review.ready, "{:?}", review.blockers);
    review
}

fn assert_no_changes(f: &Fixture) {
    let world = f.fleet.world();
    assert!(world.calls.is_empty(), "{:?}", world.calls);
    assert!(world.tokens.is_empty(), "{:?}", world.tokens);
}

#[tokio::test]
async fn a_second_execute_waits_its_turn_and_keeps_its_review() {
    let f = fixture(INVENTORY);
    let first = review_restart(&f).await;
    let second = review_restart(&f).await;
    let (a, b) = tokio::join!(
        f.engine.desired_execute(&first.id, no_secrets(), None),
        f.engine.desired_execute(&second.id, no_secrets(), None),
    );
    let (refused, loser) = match (a, b) {
        (Ok(()), Err(e)) => (e, &second),
        (Err(e), Ok(())) => (e, &first),
        other => panic!("exactly one execute should start: {other:?}"),
    };
    assert!(refused.to_string().contains("Another change is running"), "{refused}");
    assert_eq!(wait_done(&f.engine).await.status, OperationStatus::Succeeded);

    f.engine.desired_execute(&loser.id, no_secrets(), None).await.unwrap();
    assert_eq!(wait_done(&f.engine).await.status, OperationStatus::Succeeded);
}

#[tokio::test]
async fn execution_rejects_inventory_identity_changes_without_an_intervening_view() {
    for (old, new) in [
        ("ssh = \"mini\"", "ssh = \"different-machine\""),
        ("darwin-arm64", "linux-x64"),
        ("~/gh-runners/acme", "~/gh-runners/different-root"),
        ("directory = \"runner-01\"", "directory = \"different-runner\""),
        ("name = \"acme\"", "name = \"different-org\""),
        ("https://github.com/acme", "https://github.com/different-org"),
        ("runner_group = \"trusted\"", "runner_group = \"different-group\""),
    ] {
        let f = fixture(INVENTORY);
        let review = review_restart(&f).await;
        let mut changed = INVENTORY.replace(old, new);
        if old == "darwin-arm64" {
            changed = changed
                .replace("launchagent", "systemd")
                .replace("macos-build", "linux-build");
        }
        if old == "name = \"acme\"" {
            changed = changed.replace("https://github.com/acme", "https://github.com/different-org");
        } else if old == "https://github.com/acme" {
            changed = changed.replace("name = \"acme\"", "name = \"different-org\"");
        }
        fleet::desired::inventory::parse_inventory(&changed, &Default::default()).unwrap();
        std::fs::write(f.engine.inventory_path(), changed).unwrap();
        let error = f
            .engine
            .desired_execute(&review.id, no_secrets(), None)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("changed since you reviewed it"),
            "{old}: {error}"
        );
        assert_no_changes(&f);
    }
}

#[tokio::test]
async fn execution_rejects_changed_runner_registration_and_observed_path() {
    for registration in [true, false] {
        let f = fixture(INVENTORY);
        let review = review_restart(&f).await;
        {
            let mut world = f.fleet.world();
            if registration {
                world.github.get_mut("mini-01").unwrap().id = 999;
            } else {
                world.local.get_mut("mini-01").unwrap().path = "different-path".into();
            }
        }
        let comparison = fixture(INVENTORY);
        {
            let world = f.fleet.world();
            let mut other = comparison.fleet.world();
            other.github = world.github.clone();
            other.local = world.local.clone();
        }
        let current = comparison.engine.desired_review(review.request.clone()).await.unwrap();
        assert_eq!(
            current.items, review.items,
            "the visible plan does not reveal this identity change"
        );
        let error = f
            .engine
            .desired_execute(&review.id, no_secrets(), None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("changed since you reviewed it"), "{error}");
        assert_no_changes(&f);
    }
}

#[tokio::test]
async fn execution_rejects_changed_ssh_alias_resolution() {
    let f = fixture(INVENTORY);
    let review = review_restart(&f).await;
    f.ssh.locked().hosts.insert("mini".into(), "different-machine".into());
    let error = f
        .engine
        .desired_execute(&review.id, no_secrets(), None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("changed since you reviewed it"), "{error}");
    assert_no_changes(&f);
}

#[tokio::test]
async fn execution_rejects_changed_ssh_transport_with_unchanged_hostname() {
    let original = "hostname mac-mini.example.ts.net\nport 22\nuser fake-ci\nproxycommand none\nproxyjump none\nidentityfile /fake/key\n";
    for (option, changed) in [
        ("port 22", "port 2222"),
        ("user fake-ci", "user different-ci"),
        ("proxycommand none", "proxycommand ssh fake-proxy -W %h:%p"),
        ("proxyjump none", "proxyjump fake-bastion"),
        ("identityfile /fake/key", "identityfile /fake/other-key"),
    ] {
        let f = fixture(INVENTORY);
        f.ssh.locked().configs.insert("mini".into(), original.into());
        let review = review_restart(&f).await;
        let configuration = original.replace(option, changed);
        f.ssh.locked().configs.insert("mini".into(), configuration);
        let error = f
            .engine
            .desired_execute(&review.id, no_secrets(), None)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("changed since you reviewed it"),
            "{option}: {error}"
        );
        assert_no_changes(&f);
    }
}

#[tokio::test]
async fn unresolved_ssh_transport_blocks_review_and_execution() {
    let f = fixture(INVENTORY);
    let review = review_restart(&f).await;
    // Failed resolution must not turn the previously approved configuration into a fallback.
    f.ssh.locked().configs.insert("mini".into(), String::new());
    let error = f
        .engine
        .desired_execute(&review.id, no_secrets(), None)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("SSH transport changed since you reviewed it"),
        "{error}"
    );
    assert_no_changes(&f);
    let blocked = f.engine.desired_review(review.request).await.unwrap();
    assert!(!blocked.ready);
    assert!(
        blocked
            .blockers
            .iter()
            .any(|b| b.contains("Couldn't resolve the effective SSH configuration"))
    );
    assert_no_changes(&f);
}

#[tokio::test]
async fn unchanged_ssh_transport_keeps_review_executable() {
    let f = fixture(INVENTORY);
    let review = review_restart(&f).await;
    f.engine.desired_execute(&review.id, no_secrets(), None).await.unwrap();
    assert_eq!(wait_done(&f.engine).await.status, OperationStatus::Succeeded);
    assert_eq!(f.fleet.world().calls, vec!["restart mini mini-01"]);
}

#[tokio::test]
async fn execution_rejects_new_host_blockers() {
    let f = fixture(INVENTORY);
    let review = review_restart(&f).await;
    f.fleet
        .world()
        .host_errors
        .insert("mini".into(), "identity check failed".into());
    let error = f
        .engine
        .desired_execute(&review.id, no_secrets(), None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("changed since you reviewed it"), "{error}");
    assert_no_changes(&f);
}

#[tokio::test]
async fn scoped_refresh_advances_github_freshness_without_freshening_other_hosts() {
    let f = fixture(INVENTORY);
    healthy(&f.fleet, "mini-01", &["fleet", "macos-build"]);
    healthy(&f.fleet, "linux-01", &[]);
    f.engine.desired_refresh(None).await.unwrap();
    let old_github = f.engine.view().plan.checked_at.unwrap();
    let old_linux = desired(&f.engine, "linux").checked_at.unwrap();
    let old_mini = desired(&f.engine, "mini").checked_at.unwrap();
    {
        let mut world = f.fleet.world();
        world.github.get_mut("linux-01").unwrap().busy = true;
        world.local.get_mut("linux-01").unwrap().service_state = "inactive".into();
    }
    f.engine.desired_refresh(Some("mini".into())).await.unwrap();
    assert!(f.engine.view().plan.checked_at.unwrap() > old_github);
    assert!(desired(&f.engine, "mini").checked_at.unwrap() > old_mini);
    assert_eq!(desired(&f.engine, "linux").checked_at, Some(old_linux));
    let status = machine(&f.engine, "linux")
        .runners
        .into_iter()
        .find(|runner| runner.name == "linux-01")
        .unwrap()
        .desired
        .unwrap()
        .status
        .unwrap();
    assert_eq!(
        status.service_state.as_deref(),
        Some("active"),
        "unchecked local state remains unchanged"
    );
    assert_eq!(status.busy, Some(true), "GitHub was checked for all runners");
}

#[tokio::test]
async fn first_scoped_refresh_does_not_claim_other_hosts_were_probed() {
    let f = fixture(INVENTORY);
    f.engine.desired_refresh(Some("mini".into())).await.unwrap();
    assert!(f.engine.view().plan.checked_at.is_some());
    assert!(desired(&f.engine, "mini").checked_at.is_some());
    assert!(desired(&f.engine, "linux").checked_at.is_none());
}

#[tokio::test]
async fn review_rejects_shared_ssh_destinations_without_tailnet_data() {
    let inventory = INVENTORY.replace("builder@linux.example.ts.net", "builder@MAC-MINI.EXAMPLE.TS.NET.");
    let f = fixture(&inventory);
    healthy(&f.fleet, "mini-01", &["fleet", "macos-build"]);
    let review = f
        .engine
        .desired_review(ChangeRequest::Restart {
            runner: "mini-01".into(),
        })
        .await
        .unwrap();
    assert!(!review.ready);
    assert!(
        review
            .blockers
            .iter()
            .any(|blocker| blocker.contains("share the same machine")),
        "{:?}",
        review.blockers
    );
    let error = f
        .engine
        .desired_execute(&review.id, no_secrets(), None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("blockers"), "{error}");
    assert_no_changes(&f);
}

#[tokio::test]
async fn applying_a_replacement_preserves_legacy_capacity_until_a_separate_retirement() {
    let f = fixture(INVENTORY);
    healthy(&f.fleet, "Old-Mini", &[]);
    let review = f
        .engine
        .desired_review(ChangeRequest::Apply {
            host: "mini".into(),
            runner: Some("mini-02".into()),
        })
        .await
        .unwrap();
    assert!(review.ready, "{:?}", review.blockers);
    assert!(
        review
            .items
            .iter()
            .any(|item| item.action == FleetPlanAction::PreserveLegacy && item.blocked)
    );
    f.engine.desired_execute(&review.id, no_secrets(), None).await.unwrap();
    let operation = wait_done(&f.engine).await;
    assert_eq!(operation.status, OperationStatus::Succeeded, "{:?}", operation.error);
    let world = f.fleet.world();
    assert!(world.github.contains_key("Old-Mini"));
    assert!(world.local.contains_key("Old-Mini"));
    assert!(world.calls.iter().all(|call| !call.starts_with("retire ")));
    assert_eq!(world.tokens, ["registration"]);
}

#[tokio::test]
async fn execution_rejects_a_new_peer_ownership_blocker_even_when_plan_items_are_unchanged() {
    use fleet::model::{TailnetPeer, TailnetSnapshot};
    let mut f = fixture(INVENTORY);
    let peers: Arc<Mutex<Vec<TailnetPeer>>> = Arc::default();
    let source_peers = peers.clone();
    let engine = Arc::try_unwrap(f.engine).unwrap().with_tailscale_source(move || {
        let peers = source_peers.locked().clone();
        async move {
            Ok(TailnetSnapshot {
                self_node_id: None,
                local_online: Some(true),
                tailnet_name: None,
                peers,
            })
        }
    });
    f.engine = Arc::new(engine);
    f.engine.poll_tailscale().await;
    let review = review_restart(&f).await;
    peers.locked().push(TailnetPeer {
        node_id: "shared-node".into(),
        hostname: "linux".into(),
        dns_name: Some("mac-mini.example.ts.net".into()),
        os: "macOS".into(),
        ips: vec![],
        online: true,
        last_seen: None,
        tags: vec![],
        ssh_host_keys: vec![],
    });
    f.engine.poll_tailscale().await;
    // Host facts and GitHub facts have not changed; only ownership became ambiguous.
    let error = f
        .engine
        .desired_execute(&review.id, no_secrets(), None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("changed since you reviewed it"), "{error}");
    assert_no_changes(&f);
}

#[tokio::test]
async fn concurrent_reviews_have_distinct_one_use_handles() {
    let f = fixture(INVENTORY);
    healthy(&f.fleet, "mini-01", &["fleet", "macos-build"]);
    let request = ChangeRequest::Restart {
        runner: "mini-01".into(),
    };
    let (first, second) = tokio::join!(
        f.engine.desired_review(request.clone()),
        f.engine.desired_review(request)
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_ne!(first.id, second.id);
    f.engine.desired_execute(&first.id, no_secrets(), None).await.unwrap();
    assert_eq!(wait_done(&f.engine).await.status, OperationStatus::Succeeded);
    f.engine.desired_execute(&second.id, no_secrets(), None).await.unwrap();
    assert_eq!(wait_done(&f.engine).await.status, OperationStatus::Succeeded);
    assert_eq!(f.fleet.world().calls, ["restart mini mini-01", "restart mini mini-01"]);
}

async fn api_call(
    app: &axum::Router,
    request: axum::http::Request<axum::body::Body>,
) -> (axum::http::StatusCode, serde_json::Value) {
    use tower::ServiceExt;
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 4 << 20).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

fn api_get(path: &str) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::get(path)
        .header("host", "localhost")
        .body(axum::body::Body::empty())
        .unwrap()
}

async fn api_check(app: &axum::Router, body: serde_json::Value) -> serde_json::Value {
    let request = axum::http::Request::post("/v1/checks")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let (status, accepted) = api_call(app, request).await;
    assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{accepted}");
    let id = accepted["check"]["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (status, response) = api_call(app, api_get(&format!("/v1/checks/{id}"))).await;
            assert_eq!(status, axum::http::StatusCode::OK);
            if response["check"]["status"] != "running" {
                return response["check"].clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn http_inventory_doctor_and_logs_use_read_only_boundaries_and_report_failures() {
    use serde_json::json;
    let f = fixture(INVENTORY);
    let app = fleet::local_api::router(f.engine.clone());
    f.fleet
        .world()
        .host_errors
        .insert("mini".into(), "Synthetic SSH failure".into());
    let check = api_check(&app, json!({ "kind": "inventory", "machine": "inv:mini" })).await;
    assert_eq!(check["status"], "completed");
    assert!(
        check["result"]["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("Synthetic SSH failure"))
    );
    assert_eq!(f.fleet.world().reads, ["probe mini"]);
    f.fleet.world().host_errors.clear();
    let check = api_check(&app, json!({ "kind": "doctor", "machine": "mini" })).await;
    assert_eq!(check["status"], "completed");
    let (_, desired) = api_call(&app, api_get("/v1/desired?machine=inv:mini")).await;
    assert!(
        desired["hosts"][0]["desired"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["check"] == "ssh")
    );
    assert!(desired["hosts"][0]["desired"]["checkedAt"].is_string());
    let (status, logs) = api_call(&app, api_get("/v1/logs?runner=mini-01&lines=17")).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{logs}");
    assert_eq!(logs["text"], "==> mini-01 last 17 <==\n");
    assert_eq!(logs["machineKey"], "inv:mini");
    assert_eq!(logs["truncated"], false);
    f.fleet.world().log_output = Some(format!("{}\nlast line", "😸".repeat(300000)));
    let (_, logs) = api_call(&app, api_get("/v1/logs?runner=mini-01")).await;
    assert_eq!(logs["truncated"], true);
    assert!(logs["text"].as_str().unwrap().len() <= 1048576);
    assert!(logs["text"].as_str().unwrap().ends_with("last line"));
    let calls = f.fleet.world().reads.len();
    for path in [
        "/v1/logs?runner=mini-01&lines=0",
        "/v1/logs?runner=mini-01&lines=10001",
        "/v1/logs?runner=../../elsewhere",
        "/v1/logs?runner=mini-01&path=/tmp/other",
    ] {
        assert!(api_call(&app, api_get(path)).await.0.is_client_error());
    }
    assert_eq!(f.fleet.world().reads.len(), calls);
    assert!(f.fleet.world().calls.is_empty());
    assert!(f.fleet.world().tokens.is_empty());
    f.engine
        .set_inventory_path(Some(f._tmp.path().join("missing.toml").display().to_string()))
        .unwrap();
    let check = api_check(&app, json!({ "kind": "inventory" })).await;
    assert_eq!(check["status"], "failed");
    assert_eq!(check["error"]["code"], "check_failed");
    assert!(check["result"].is_null());
}
