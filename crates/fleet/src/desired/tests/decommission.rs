use super::super::model::{FleetPlanAction, PlanItem};
use super::super::operations::OperationError;
use super::super::plan::generate_plan;
use super::super::remote::FleetRemote;
use super::super::state::build_status_rows;
use super::*;

/// A Windows host with a desired and a decommissioned runner, and a Linux
/// host whose only runner is decommissioned.
pub(super) fn decom() -> Inventory {
    load(&inventory_toml(
        r#"
[[hosts]]
id = "win"
platform = "windows-arm64"
ssh = "win-host"
root = "C:\\gh-runners\\acme"
service_mode = "windows-service"
labels = ["fleet", "windows-build"]
runners = [
  { name = "win-01", directory = "runner-01" },
  { name = "win-02", directory = "runner-02", decommission = true },
]

[[hosts]]
id = "linux"
platform = "linux-x64"
ssh = "linux-host"
root = "~/gh-runners/acme"
service_mode = "systemd"
labels = ["fleet", "linux-build"]
runners = [
  { name = "linux-01", directory = "runner-01", decommission = true },
]
"#,
    ))
    .unwrap()
}

/// A GitHub runner in the trusted group.
pub(super) fn dg(name: &str, status: &str, busy: bool, id: i64) -> GithubRunner {
    GithubRunner::new(id, name, status, busy, &[], Some(true))
}

/// A Windows runner directory named after its runner.
pub(super) fn dl(name: &str, configured: bool, service: &str) -> LocalRunner {
    LocalRunner::new(
        name,
        &format!("C:\\gh-runners\\acme\\{name}"),
        configured,
        service,
        Some("2.337.0"),
    )
}

#[test]
fn decommission_flag_is_parsed_and_excluded_from_desired_runners() {
    let inventory = decom();
    let host = inventory.find_host("win").unwrap();
    let desired: Vec<&str> = host.desired_runners().map(|r| r.name.as_str()).collect();
    assert_eq!(desired, ["win-01"]);
    assert!(host.runners[1].decommission);
}

#[test]
fn non_boolean_decommission_is_rejected() {
    let text = inventory_toml(
        r#"
[[hosts]]
id = "linux"
platform = "linux-x64"
ssh = "linux-host"
root = "~/gh-runners/acme"
service_mode = "systemd"
labels = ["fleet", "linux-build"]
runners = [{ name = "linux-01", directory = "runner-01", decommission = "yes" }]
"#,
    );
    assert_eq!(load_err(&text), "decommission must be true or false");
}

#[test]
fn decommissioned_runner_cannot_replace_a_legacy_runner() {
    let text = inventory_toml(
        r#"
[[hosts]]
id = "linux"
platform = "linux-x64"
ssh = "linux-host"
root = "~/gh-runners/acme"
service_mode = "systemd"
labels = ["fleet", "linux-build"]
runners = [{ name = "linux-01", directory = "runner-01", decommission = true, replaces = { name = "old", directory = "~/actions-runner" } }]
"#,
    );
    assert_eq!(
        load_err(&text),
        "linux-01: a decommissioned runner cannot declare replaces"
    );
}

fn decom_plan(github: Vec<GithubRunner>, local: Vec<LocalRunner>) -> PlanItem {
    generate_plan(&decom(), &snapshot(github, local), Some("win"), Some("win-02"))
        .unwrap()
        .remove(0)
}

#[test]
fn registered_runner_plans_guarded_decommission() {
    let item = decom_plan(
        vec![dg("win-02", "online", false, 7)],
        vec![dl("win-02", true, "running")],
    );
    assert_eq!(item.action, FleetPlanAction::Decommission);
    assert_eq!(
        item.command.as_deref(),
        Some("bin/fleet decommission win --runner win-02")
    );
    assert!(!item.blocked);
}

#[test]
fn removed_runner_is_satisfied() {
    let item = decom_plan(vec![], vec![dl("win-02", false, "missing")]);
    assert_eq!(item.action, FleetPlanAction::NoChange);
    assert_eq!(
        item.reason,
        "decommissioned; exact directory C:\\gh-runners\\acme\\runner-02 is preserved"
    );
}

#[test]
fn busy_runner_blocks_decommission() {
    let item = decom_plan(
        vec![dg("win-02", "online", true, 7)],
        vec![dl("win-02", true, "running")],
    );
    assert_eq!(item.action, FleetPlanAction::PreserveDecommission);
    assert!(item.blocked);
}

#[test]
fn offline_orphan_registration_plans_deletion() {
    let item = decom_plan(vec![dg("win-02", "offline", false, 7)], vec![]);
    assert_eq!(item.action, FleetPlanAction::Decommission);
    assert_eq!(
        item.reason,
        "delete the offline GitHub registration; no local directory is configured"
    );
}

#[test]
fn online_registration_without_local_directory_is_blocked() {
    let item = decom_plan(vec![dg("win-02", "online", false, 7)], vec![]);
    assert_eq!(item.action, FleetPlanAction::Inspect);
    assert!(item.blocked);
}

#[test]
fn status_reports_decommission_drift_as_not_desired() {
    let inventory = decom();
    let rows = build_status_rows(
        &inventory,
        &snapshot(
            vec![dg("win-02", "online", false, 7)],
            vec![dl("win-02", true, "running")],
        ),
        Some("win"),
    )
    .unwrap();
    let row = rows.iter().find(|r| r.runner == "win-02").unwrap();
    assert_eq!(row.drift, "decommission pending");
    assert!(!row.desired);
    let gone = build_status_rows(&inventory, &FleetSnapshot::default(), Some("win")).unwrap();
    assert_eq!(
        gone.iter().find(|r| r.runner == "win-02").unwrap().drift,
        "decommissioned"
    );
}

#[test]
fn host_apply_never_installs_a_decommissioned_runner() {
    let inventory = decom();
    let fleet = FakeFleet::new(
        vec![dg("win-01", "online", false, 7)],
        vec![dl("win-01", true, "running")],
    );
    ops(&inventory, &fleet, Secrets::default())
        .apply_host("win", None)
        .unwrap();
    assert!(!fleet.world().calls.iter().any(|c| c.starts_with("install ")));
}

#[test]
fn apply_refuses_to_select_a_decommissioned_runner() {
    let inventory = decom();
    let fleet = FakeFleet::default();
    let e = ops(&inventory, &fleet, Secrets::default())
        .apply_host("win", Some("win-02"))
        .unwrap_err();
    assert_eq!(
        e.0,
        "win-02 is declared for decommission; use bin/fleet decommission win --runner win-02"
    );
}

#[test]
fn restart_refuses_a_decommissioned_runner() {
    let inventory = decom();
    let fleet = FakeFleet::new(vec![dg("win-02", "online", false, 7)], vec![]);
    let e = ops(&inventory, &fleet, Secrets::default())
        .restart_runner("win-02")
        .unwrap_err();
    assert_eq!(e.0, "win-02 is declared for decommission; refusing restart");
}

#[test]
fn restart_refuses_busy_or_unverifiable_runners() {
    let inventory = decom();
    let fleet = FakeFleet::new(vec![dg("win-01", "online", true, 7)], vec![]);
    assert_eq!(
        ops(&inventory, &fleet, Secrets::default())
            .restart_runner("win-01")
            .unwrap_err()
            .0,
        "win-01 is busy; refusing restart"
    );
    let fleet = FakeFleet::default();
    assert_eq!(
        ops(&inventory, &fleet, Secrets::default())
            .restart_runner("win-01")
            .unwrap_err()
            .0,
        "win-01 is missing on GitHub; cannot verify it is idle"
    );
    assert!(fleet.world().calls.is_empty());
}

fn run_decommission(fleet: &FakeFleet, name: &str) -> Result<String, OperationError> {
    let inventory = decom();
    ops(&inventory, fleet, Secrets::default()).decommission_runner("win", name)
}

#[test]
fn configured_runner_is_deregistered_with_a_transient_removal_token() {
    let fleet = FakeFleet::new(
        vec![dg("win-02", "online", false, 7)],
        vec![dl("win-02", true, "running")],
    );
    let message = run_decommission(&fleet, "win-02").unwrap();
    assert_eq!(
        fleet.world().calls,
        ["decommission win win-02 token=short-lived-removal"]
    );
    assert_eq!(
        message,
        "decommissioned win-02; preserved C:\\gh-runners\\acme\\runner-02"
    );
}

#[test]
fn runner_must_be_declared_in_inventory_first() {
    let fleet = FakeFleet::new(
        vec![dg("win-01", "online", false, 7)],
        vec![dl("win-01", true, "running")],
    );
    assert_eq!(
        run_decommission(&fleet, "win-01").unwrap_err().0,
        "win-01 is still desired; set decommission = true in inventory first"
    );
    assert!(fleet.world().calls.is_empty());
}

#[test]
fn busy_runner_is_refused_without_mutation() {
    let fleet = FakeFleet::new(
        vec![dg("win-02", "online", true, 7)],
        vec![dl("win-02", true, "running")],
    );
    assert_eq!(
        run_decommission(&fleet, "win-02").unwrap_err().0,
        "win-02 is busy; refusing to decommission"
    );
    let world = fleet.world();
    assert!(world.calls.is_empty() && world.tokens.is_empty());
}

#[test]
fn already_removed_runner_is_a_no_op() {
    let fleet = FakeFleet::new(vec![], vec![dl("win-02", false, "missing")]);
    assert_eq!(
        run_decommission(&fleet, "win-02").unwrap(),
        "win-02 is already decommissioned; C:\\gh-runners\\acme\\runner-02 is preserved"
    );
    assert!(fleet.world().calls.is_empty());
}

#[test]
fn offline_orphan_registration_is_deleted_through_the_api() {
    let fleet = FakeFleet::new(vec![dg("win-02", "offline", false, 42)], vec![]);
    assert_eq!(
        run_decommission(&fleet, "win-02").unwrap(),
        "deleted offline GitHub registration for win-02"
    );
    assert_eq!(fleet.world().calls, ["delete 42"]);
}

#[test]
fn online_registration_without_local_directory_is_refused() {
    let fleet = FakeFleet::new(vec![dg("win-02", "online", false, 7)], vec![]);
    assert_eq!(
        run_decommission(&fleet, "win-02").unwrap_err().0,
        "win-02: GitHub reports it online but C:\\gh-runners\\acme\\runner-02 is not configured; refusing to delete"
    );
    assert!(fleet.world().calls.is_empty());
}

#[test]
fn local_configuration_without_github_runner_needs_inspection() {
    let fleet = FakeFleet::new(vec![], vec![dl("win-02", true, "running")]);
    assert_eq!(
        run_decommission(&fleet, "win-02").unwrap_err().0,
        "win-02: exact local directory is configured but GitHub runner is missing; inspect manually"
    );
}

#[test]
fn registration_that_survives_removal_is_reported() {
    let fleet = FakeFleet::new(
        vec![dg("win-02", "online", false, 7)],
        vec![dl("win-02", true, "running")],
    );
    fleet.world().survives_removal = true;
    assert_eq!(
        run_decommission(&fleet, "win-02").unwrap_err().0,
        "win-02 still appears in GitHub 20s after deregistration; inspect status before retrying"
    );
}

#[test]
fn windows_payload_targets_the_exact_inventory_runner() {
    let inventory = decom();
    let host = inventory.find_host("win").unwrap();
    let (commands, remote) = recording_remote();
    remote
        .decommission(host, &host.runners[1], "tok", &Transient::NONE)
        .unwrap();
    let script = decoded(&commands);
    assert!(script.contains("$env:RF_ROOT = 'C:\\gh-runners\\acme\\runner-02'"));
    assert!(script.contains("$env:RF_NAME = 'win-02'"));
    assert!(script.contains("config.cmd remove"));
}

#[test]
fn posix_payload_carries_nixos_helper_and_resolves_home() {
    let inventory = decom();
    let host = inventory.find_host("linux").unwrap();
    let (commands, remote) = recording_remote();
    remote
        .decommission(host, &host.runners[0], "tok", &Transient::NONE)
        .unwrap();
    let input = sent(&commands);
    assert!(input.starts_with("RF_ROOT=\"$HOME\"/gh-runners/acme/runner-01\n"));
    assert!(input.contains("\nRF_NAME=linux-01\n"));
    assert!(input.contains("\nRF_TOKEN=tok\n"));
    assert!(input.contains("\nRF_NIXOS_SERVICE_HELPER="));
    assert!(input.contains("uninstall"));
    // Shell variables, not exported: programs the script starts don't inherit them.
    assert!(!input.contains("export RF_"), "{input}");
    assert_eq!(input.matches("run_as_root() {").count(), 1);
}

#[test]
fn desired_runner_cannot_be_sent_for_decommission() {
    let inventory = decom();
    let host = inventory.find_host("win").unwrap();
    let e = recording_remote()
        .1
        .decommission(host, &host.runners[0], "tok", &Transient::NONE)
        .unwrap_err();
    assert_eq!(e.0, "win-01 is not declared decommission = true");
}
