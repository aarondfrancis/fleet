#![expect(clippy::unwrap_used, reason = "tests fail at the assertion site")]
//! SSH polling with fake Tailscale, inventory resolution, and machine reads.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use fleet::desired::remote::RemoteError;
use fleet::engine::desired::{DesiredIo, SshTransportIdentity};
use fleet::engine::{Engine, MemorySecrets};
use fleet::machine::{MachineReport, MachineSource, MachineTarget, parse_inspection};
use fleet::model::{MachineConnection, TailnetPeer, TailnetSnapshot};

const INVENTORY: &str = r#"
[organization]
name = "test"
url = "https://github.com/test"
runner_group = "test"
[[hosts]]
id = "mini"
platform = "darwin-arm64"
ssh = "mini-alias"
root = "~/gh-runners/test"
service_mode = "launchagent"
labels = ["fleet", "macos-build"]
runners = [{ name = "mini-01", directory = "runner-01" }]
"#;

#[derive(Default)]
struct FakeMachine {
    failures: Mutex<VecDeque<bool>>,
    calls: Mutex<Vec<String>>,
}

impl MachineSource for FakeMachine {
    fn inspect(&self, target: &MachineTarget) -> Result<MachineReport, RemoteError> {
        self.calls
            .lock()
            .unwrap()
            .push(target.host().unwrap().ssh_target.clone().unwrap());
        if self.failures.lock().unwrap().pop_front().unwrap_or(false) {
            return Err(RemoteError("SSH permission denied.".into()));
        }
        parse_inspection(
            target,
            "RFM\tos\tmacos\nRFM\tcpu\t42\nRFM\tmemory_total\t2000\nRFM\tmemory_available\t1000\nRFM_END\nRFM_RUNNERS\nRF1\tmini-01\t/Users/ci/gh-runners/test/runner-01\ttrue\tactive\t2.329.0\n",
        )
    }
}

fn fixture(source: Arc<dyn MachineSource>, inventory: &str) -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fleet.toml");
    std::fs::write(&path, inventory).unwrap();
    let engine = Engine::load(dir.path().join("config.json"), Arc::new(MemorySecrets::default()))
        .unwrap()
        .with_machine_source(source)
        .with_desired_io(DesiredIo {
            boundaries: Arc::new(|_| panic!("polling must not call runner operations or GitHub")),
            sleep: |_| panic!("no operation waits"),
            resolve_ssh_transport: Arc::new(|target| {
                (target == "mini-alias")
                    .then(|| SshTransportIdentity::from_config("hostname mini.demo.ts.net\nport 22\nuser fake-ci\n"))
                    .flatten()
            }),
        })
        .with_tailscale_source(|| async {
            Ok(TailnetSnapshot {
                peers: vec![TailnetPeer {
                    node_id: "nMini".into(),
                    hostname: "mini".into(),
                    dns_name: Some("mini.demo.ts.net".into()),
                    os: "macOS".into(),
                    ips: vec![],
                    online: true,
                    last_seen: None,
                    tags: vec![],
                    ssh_host_keys: vec![],
                }],
                ..Default::default()
            })
        });
    engine.set_inventory_path(Some(path.display().to_string())).unwrap();
    fleet::desired::inventory::load_inventory_from_env(&path).unwrap();
    (dir, engine)
}

#[tokio::test]
async fn success_failure_and_recovery_preserve_last_success_until_replaced() {
    let fake = Arc::new(FakeMachine::default());
    *fake.failures.lock().unwrap() = VecDeque::from([false, true, false]);
    let (_dir, engine) = fixture(fake.clone(), INVENTORY);
    engine.poll_tailscale().await;
    engine.poll_machines().await;
    let machine = engine.view().machines.remove(0);
    assert_eq!(machine.id, "ts:nMini");
    assert!(matches!(machine.connection, MachineConnection::Connected { .. }));
    let health = machine.health.unwrap();
    let diagnostics = machine.diagnostics.unwrap();
    assert_eq!(health.cpu_percent, Some(42.0));
    engine.poll_machines().await;
    let machine = engine.view().machines.remove(0);
    assert_eq!(
        machine.connection,
        MachineConnection::Stale {
            checked_at: health.as_of,
            reason: "SSH permission denied.".into()
        }
    );
    assert_eq!(
        machine.health.unwrap(),
        fleet::model::HealthView { stale: true, ..health }
    );
    assert_eq!(
        machine.diagnostics.unwrap(),
        fleet::machine_diagnostics::MachineDiagnostics {
            stale: true,
            ..diagnostics
        }
    );
    engine.poll_machines().await;
    let machine = engine.view().machines.remove(0);
    assert!(matches!(machine.connection, MachineConnection::Connected { .. }));
    assert!(!machine.health.unwrap().stale);
    assert!(!machine.diagnostics.unwrap().stale);
    assert_eq!(*fake.calls.lock().unwrap(), ["mini-alias", "mini-alias", "mini-alias"]);
}

#[tokio::test]
async fn demo_and_disabled_hosts_never_start_ssh() {
    let fake = Arc::new(FakeMachine::default());
    let (_dir, engine) = fixture(
        fake.clone(),
        &INVENTORY.replace("id = \"mini\"", "id = \"mini\"\nenabled = false"),
    );
    engine.poll_tailscale().await;
    engine.poll_machines().await;
    assert_eq!(engine.view().machines[0].connection, MachineConnection::Disabled);
    engine.set_demo_mode(true).unwrap();
    engine.poll_machines().await;
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn refreshes_coalesce_and_limit_concurrent_ssh_sessions() {
    use std::sync::Condvar;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct BlockingSource {
        active: AtomicUsize,
        peak: AtomicUsize,
        calls: AtomicUsize,
        started: tokio::sync::mpsc::Sender<()>,
        release: (Mutex<bool>, Condvar),
    }
    impl MachineSource for BlockingSource {
        fn inspect(&self, target: &MachineTarget) -> Result<MachineReport, RemoteError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            self.started.blocking_send(()).unwrap();
            let (lock, wake) = &self.release;
            let (released, _) = wake
                .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |released| !*released)
                .unwrap();
            self.active.fetch_sub(1, Ordering::SeqCst);
            assert!(*released, "test must release the SSH reads");
            let name = &target.host().unwrap().runners[0].name;
            parse_inspection(
                target,
                &format!("RFM\tos\tmacos\nRFM_END\nRFM_RUNNERS\nRF1\t{name}\t/home/ci/runner\tfalse\tmissing\t\n"),
            )
        }
    }
    let (started, mut receiver) = tokio::sync::mpsc::channel(8);
    let source = Arc::new(BlockingSource {
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
        started,
        release: (Mutex::new(false), Condvar::new()),
    });
    let (organization, host) = INVENTORY.split_once("[[hosts]]").unwrap();
    let mut inventory = organization.to_string();
    for i in 0..8 {
        inventory.push_str("[[hosts]]");
        inventory.push_str(&host.replace("mini", &format!("mini-{i}")));
    }
    let (_dir, engine) = fixture(source.clone(), &inventory);
    let engine = Arc::new(engine);
    let poll = tokio::spawn({
        let engine = engine.clone();
        async move { engine.poll_machines().await }
    });
    for _ in 0..4 {
        tokio::time::timeout(Duration::from_secs(3), receiver.recv())
            .await
            .unwrap()
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(1), engine.poll_machines())
        .await
        .unwrap();
    assert_eq!(source.calls.load(Ordering::SeqCst), 4);
    *source.release.0.lock().unwrap() = true;
    source.release.1.notify_all();
    poll.await.unwrap();
    assert_eq!(source.calls.load(Ordering::SeqCst), 8);
    assert_eq!(source.peak.load(Ordering::SeqCst), 4);
    assert!(
        engine
            .view()
            .machines
            .iter()
            .all(|m| matches!(m.connection, MachineConnection::Connected { .. }))
    );
}

#[tokio::test]
async fn personal_labels_persist_across_restart_and_tailnet_discovery_without_changing_hosts() {
    let fake = Arc::new(FakeMachine::default());
    let (dir, engine) = fixture(fake.clone(), INVENTORY);
    let original = engine.view().machines.remove(0);
    engine
        .set_machine_preferences(&original.id, "  Office Mini  ", "  Shelf by the router.\nGUI tests.  ")
        .unwrap();
    assert!(fake.calls.lock().unwrap().is_empty());
    engine.poll_tailscale().await;
    engine.poll_machines().await;
    let labelled = engine.view().machines.remove(0);
    assert_eq!(labelled.id, "ts:nMini");
    assert_eq!(labelled.display_name, "Office Mini");
    assert_eq!(
        labelled.description.as_deref(),
        Some("Shelf by the router.\nGUI tests.")
    );
    assert_eq!((labelled.host, labelled.desired), (original.host, original.desired));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("fleet.toml")).unwrap(),
        INVENTORY
    );
    let reloaded = Engine::load(dir.path().join("config.json"), Arc::new(MemorySecrets::default())).unwrap();
    let machine = reloaded.view().machines.remove(0);
    assert_eq!(machine.id, "inv:mini"); // No tailnet observation yet after restart.
    assert_eq!(machine.display_name, "Office Mini");
    assert_eq!(machine.description.as_deref(), Some("Shelf by the router.\nGUI tests."));
    reloaded
        .set_machine_preferences(&machine.id, "", "Only a note")
        .unwrap();
    let machine = reloaded.view().machines.remove(0);
    assert_eq!(
        (machine.display_name.as_str(), machine.description.as_deref()),
        ("mini", Some("Only a note"))
    );
    reloaded.set_machine_preferences(&machine.id, "", "").unwrap();
    let machine = reloaded.view().machines.remove(0);
    assert_eq!((machine.display_name.as_str(), machine.description), ("mini", None));
    assert!(reloaded.config().machine_preferences.is_empty());
}

#[tokio::test]
async fn personal_labels_follow_other_peers_and_reject_invalid_edits() {
    let fake = Arc::new(FakeMachine::default());
    let (_dir, engine) = fixture(fake, &INVENTORY.replace("mini", "different"));
    engine.poll_tailscale().await;
    assert_eq!(engine.view().other_machines.len(), 1);
    engine
        .set_machine_preferences("ts:nMini", "Guest room", "Bottom shelf")
        .unwrap();
    let machine = engine.view().other_machines.remove(0);
    assert_eq!(
        (machine.display_name.as_str(), machine.description.as_deref()),
        ("Guest room", Some("Bottom shelf"))
    );
    let saved = engine.config();
    assert!(engine.set_machine_preferences("ts:gone", "Name", "").is_err());
    assert!(
        engine
            .set_machine_preferences("ts:nMini", "name\non two lines", "")
            .is_err()
    );
    assert_eq!(engine.config(), saved);
}

#[test]
fn a_failed_settings_write_does_not_apply_the_personal_label() {
    let (dir, engine) = fixture(Arc::new(FakeMachine::default()), INVENTORY);
    let before = engine.config();
    let path = dir.path().join("config.json");
    // A directory at the destination makes the atomic rename fail on every platform.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        engine
            .set_machine_preferences("inv:mini", "Unsaved name", "Unsaved note")
            .is_err()
    );
    assert_eq!(engine.config(), before);
    let machine = engine.view().machines.remove(0);
    assert_eq!((machine.display_name.as_str(), machine.description), ("mini", None));
}

#[tokio::test]
async fn http_refresh_waits_for_polling_then_collects_its_own_fresh_sample() {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use std::sync::Condvar;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tower::ServiceExt;

    struct Gated {
        count: AtomicUsize,
        started: tokio::sync::mpsc::Sender<usize>,
        released: (Mutex<usize>, Condvar),
    }
    impl MachineSource for Gated {
        fn inspect(&self, target: &MachineTarget) -> Result<MachineReport, RemoteError> {
            let call = self.count.fetch_add(1, Ordering::SeqCst) + 1;
            self.started.blocking_send(call).unwrap();
            let (lock, ready) = &self.released;
            let (release, timeout) = ready
                .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |r| *r < call)
                .unwrap();
            drop(release);
            assert!(!timeout.timed_out(), "test failed to release SSH sample {call}");
            parse_inspection(
                target,
                "RFM\tos\tmacos\nRFM\tcpu\t42\nRFM_END\nRFM_RUNNERS\nRF1\tmini-01\t/home/ci/runner\ttrue\tactive\t2.330.0\n",
            )
        }
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let source = Arc::new(Gated {
        count: AtomicUsize::new(0),
        started: tx,
        released: (Mutex::new(0), Condvar::new()),
    });
    let (_dir, engine) = fixture(source.clone(), INVENTORY);
    let engine = Arc::new(engine);
    engine.poll_tailscale().await;
    let poll_engine = engine.clone();
    let poller = tokio::spawn(async move { poll_engine.poll_machines().await });
    assert_eq!(rx.recv().await, Some(1));
    let app = fleet::local_api::router(engine.clone());
    let request = || {
        Request::post("/v1/checks")
            .header("host", "localhost")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "kind": "refresh", "machine": "inv:mini" }).to_string(),
            ))
            .unwrap()
    };
    let response = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let location = response.headers()["location"].to_str().unwrap().to_owned();
    let accepted: Value = serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(accepted["check"]["status"], "running");
    let busy = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(busy.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(busy.headers()["retry-after"], "3");
    let status = || async {
        let response = app
            .clone()
            .oneshot(
                Request::get(&location)
                    .header("host", "localhost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        serde_json::from_slice::<Value>(&to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap()["check"]
            .clone()
    };
    assert_eq!(status().await["status"], "running");
    *source.released.0.lock().unwrap() = 1;
    source.released.1.notify_all();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.unwrap(),
        Some(2)
    );
    assert_eq!(status().await["status"], "running");
    *source.released.0.lock().unwrap() = 2;
    source.released.1.notify_all();
    poller.await.unwrap();
    let finished = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let check = status().await;
            if check["status"] != "running" {
                break check;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(finished["status"], "completed", "{finished}");
    assert_eq!(finished["result"]["errors"], json!([]));
    assert_eq!(source.count.load(Ordering::SeqCst), 2);
    assert!(matches!(
        engine.view().machines[0].connection,
        MachineConnection::Connected { .. }
    ));
}
