use super::super::model::{FleetPlanAction, PlanItem};
use super::super::parity;
use super::super::plan::generate_plan;
use super::super::state::build_status_rows;
use super::*;

fn as_strs(labels: &[String]) -> Vec<&str> {
    labels.iter().map(String::as_str).collect()
}

#[test]
fn missing_desired_runner_plans_install_without_retiring_legacy() {
    let inventory = example_inventory();
    let (_, runner) = studio(&inventory);
    let old = runner.replaces.as_ref().unwrap();
    let snap = snapshot(
        vec![gr(&old.name, "online", false, &[], Some(true))],
        vec![LocalRunner::new(
            &old.name,
            &old.directory,
            true,
            "active",
            Some("2.336.0"),
        )],
    );
    let plan = generate_plan(&inventory, &snap, Some("studio"), Some(&runner.name)).unwrap();
    assert_eq!(plan[0].action, FleetPlanAction::Install);
    assert_eq!(plan[1].action, FleetPlanAction::PreserveLegacy);
    assert!(plan[1].blocked);
}

#[test]
fn healthy_replacement_makes_exact_legacy_retirement_ready() {
    let inventory = example_inventory();
    let (host, runner) = studio(&inventory);
    let old = runner.replaces.as_ref().unwrap();
    let labels = host.labels_for(runner);
    let snap = snapshot(
        vec![
            gr(&runner.name, "online", false, &as_strs(&labels), Some(true)),
            gr(&old.name, "online", false, &[], Some(true)),
        ],
        vec![
            LocalRunner::new(&runner.name, &host.runner_path(runner), true, "active", Some("2.336.0")),
            LocalRunner::new(&old.name, &old.directory, true, "active", Some("2.336.0")),
        ],
    );
    let plan = generate_plan(&inventory, &snap, Some("studio"), Some(&runner.name)).unwrap();
    let actions: Vec<&str> = plan.iter().map(|i| i.action.as_str()).collect();
    assert_eq!(actions, ["no-change", "retire-legacy"]);
    assert!(plan[1].reason.contains(&old.directory));
    assert_eq!(
        plan[1].command.as_deref(),
        Some("bin/fleet retire studio --runner studio-01")
    );
}

#[test]
fn label_and_group_drift_are_separate_actions() {
    let inventory = example_inventory();
    let (host, runner) = studio(&inventory);
    let snap = snapshot(
        vec![gr(&runner.name, "online", false, &["self-hosted"], Some(false))],
        vec![LocalRunner::new(
            &runner.name,
            &host.runner_path(runner),
            true,
            "active",
            Some("2.336.0"),
        )],
    );
    let plan = generate_plan(&inventory, &snap, Some("studio"), Some(&runner.name)).unwrap();
    let actions: Vec<&str> = plan.iter().map(|i| i.action.as_str()).collect();
    assert_eq!(actions[..2], ["reconcile-labels", "assign-group"]);
}

#[test]
fn generated_drift_classifies_apply_steps_and_busy_service_preservation() {
    let inventory = load(&base()).unwrap();
    let mut snap = snapshot(
        vec![gr("test-01", "online", false, &["self-hosted"], Some(false))],
        vec![LocalRunner::new(
            "test-01",
            "~/gh-runners/acme/runner-01",
            true,
            "stopped",
            None,
        )],
    );
    let classify = |snapshot: &FleetSnapshot| {
        generate_plan(&inventory, snapshot, Some("test"), None)
            .unwrap()
            .into_iter()
            .map(|item| (item.action, item.apply, item.blocked))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        classify(&snap),
        [
            (FleetPlanAction::ReconcileLabels, true, false),
            (FleetPlanAction::AssignGroup, true, false),
            (FleetPlanAction::StartService, true, false),
        ],
    );
    snap.github.get_mut("test-01").unwrap().busy = true;
    assert_eq!(
        classify(&snap),
        [
            (FleetPlanAction::ReconcileLabels, true, false),
            (FleetPlanAction::AssignGroup, true, false),
            (FleetPlanAction::PreserveService, false, true),
        ],
    );
}

#[test]
fn unavailable_boundaries_produce_blocked_inspection() {
    let inventory = example_inventory();
    let (_, runner) = studio(&inventory);
    let snap = FleetSnapshot {
        github_error: Some("authentication expired".into()),
        host_errors: [("studio".to_string(), "ssh timeout".to_string())].into(),
        ..Default::default()
    };
    let item = &generate_plan(&inventory, &snap, Some("studio"), Some(&runner.name)).unwrap()[0];
    assert_eq!(item.action, FleetPlanAction::Inspect);
    assert!(item.blocked);
    assert_eq!(
        item.reason,
        "GitHub unavailable: authentication expired; host unavailable: ssh timeout"
    );
}

#[test]
fn online_service_with_offline_github_state_requires_inspection() {
    let inventory = example_inventory();
    let (host, runner) = studio(&inventory);
    let labels = host.labels_for(runner);
    let snap = snapshot(
        vec![gr(&runner.name, "offline", false, &as_strs(&labels), Some(true))],
        vec![LocalRunner::new(
            &runner.name,
            &host.runner_path(runner),
            true,
            "active",
            Some("2.336.0"),
        )],
    );
    let plan = generate_plan(&inventory, &snap, Some("studio"), Some(&runner.name)).unwrap();
    assert_eq!(plan[0].action, FleetPlanAction::InspectOffline);
    assert!(plan[0].blocked);
}

#[test]
fn unknown_runner_selection_is_an_error() {
    let inventory = example_inventory();
    let e = generate_plan(&inventory, &FleetSnapshot::default(), Some("studio"), Some("nope")).unwrap_err();
    assert_eq!(e.0, "desired runner 'nope' is not selected");
    let e = generate_plan(&inventory, &FleetSnapshot::default(), Some("nope"), None).unwrap_err();
    assert_eq!(e.0, "unknown or disabled host: nope");
}

#[test]
fn an_empty_runner_selection_selects_every_runner() {
    let inventory = example_inventory();
    let snap = FleetSnapshot::default();
    assert_eq!(
        generate_plan(&inventory, &snap, Some("studio"), Some("")).unwrap(),
        generate_plan(&inventory, &snap, Some("studio"), None).unwrap()
    );
}

#[test]
fn generated_plan_serializes_apply_classification_without_changing_cli_parity() {
    let inventory = load(&base()).unwrap();
    let mut snap = FleetSnapshot::default();
    let installs = generate_plan(&inventory, &snap, Some("test"), None).unwrap();
    assert_eq!(
        serde_json::to_value(&installs[0]).unwrap(),
        serde_json::json!({
            "action": "install", "apply": true, "host": "test", "runner": "test-01",
            "reason": "register in ~/gh-runners/acme/runner-01 and start launchagent",
            "command": "bin/fleet apply test --runner test-01", "blocked": false,
        }),
    );
    assert_eq!(
        parity::plan(&installs),
        serde_json::json!([{
            "action": "install", "host": "test", "runner": "test-01",
            "reason": "register in ~/gh-runners/acme/runner-01 and start launchagent",
            "command": "bin/fleet apply test --runner test-01", "blocked": false,
        }]),
    );

    snap.github_error = Some("unavailable".into());
    let inspection = generate_plan(&inventory, &snap, Some("test"), None).unwrap();
    assert_eq!(
        serde_json::to_value(&inspection[0]).unwrap(),
        serde_json::json!({
            "action": "inspect", "apply": false, "host": "test", "runner": "test-01",
            "reason": "GitHub unavailable: unavailable", "command": null, "blocked": true,
        }),
    );
}

#[test]
fn plan_json_reclassifies_actions_and_rejects_unknown_vocabulary() {
    let mut json = serde_json::json!({
        "action": "start-service", "apply": false, "host": "test", "runner": "test-01",
        "reason": "service stopped", "command": "bin/fleet apply test --runner test-01", "blocked": true,
    });
    let expected = PlanItem::new(FleetPlanAction::StartService, "test", "test-01", "service stopped")
        .command("bin/fleet apply test --runner test-01".into())
        .blocked();
    assert_eq!(serde_json::from_value::<PlanItem>(json.clone()).unwrap(), expected);
    assert_eq!(serde_json::to_value(&expected).unwrap()["apply"], true);

    json["action"] = serde_json::json!("retire-legacy");
    json["apply"] = serde_json::json!(true);
    let retired: PlanItem = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(retired.action, FleetPlanAction::RetireLegacy);
    assert!(!retired.apply);
    json.as_object_mut().unwrap().remove("apply");
    assert_eq!(serde_json::from_value::<PlanItem>(json.clone()).unwrap(), retired);

    json["action"] = serde_json::json!("install-unreviewed");
    let e = serde_json::from_value::<PlanItem>(json).unwrap_err();
    assert!(e.to_string().starts_with("unknown variant `install-unreviewed`"), "{e}");
}

#[test]
fn plan_action_json_and_display_preserve_existing_vocabulary() {
    for name in [
        "install",
        "reconcile-labels",
        "assign-group",
        "start-service",
        "retire-legacy",
        "decommission",
        "no-change",
        "restart",
        "inspect",
        "inspect-offline",
        "inspect-group",
        "preserve-legacy",
        "preserve-service",
        "preserve-decommission",
        "repair-registration",
        "recover-local",
        "harden-group",
    ] {
        let action: FleetPlanAction = serde_json::from_value(serde_json::json!(name)).unwrap();
        assert_eq!(serde_json::to_value(action).unwrap(), name);
        assert_eq!(action.to_string(), name);
    }
}

#[test]
fn unavailable_github_and_host_degrade_to_unknown() {
    let inventory = example_inventory();
    let snap = FleetSnapshot {
        github_error: Some("token expired".into()),
        host_errors: [("mac-build".to_string(), "asleep".to_string())].into(),
        ..Default::default()
    };
    let row = &build_status_rows(&inventory, &snap, Some("mac-build")).unwrap()[0];
    assert_eq!(row.github_state, "unknown");
    assert_eq!(row.service_state.as_deref(), Some("unknown"));
    assert_eq!(row.drift, "GitHub unavailable; host unavailable");
}

#[test]
fn unmanaged_github_runner_is_visible_in_full_status() {
    let inventory = example_inventory();
    let snap = snapshot(
        vec![GithubRunner::new(
            99,
            "mystery",
            "offline",
            false,
            &["self-hosted"],
            Some(false),
        )],
        vec![],
    );
    let rows = build_status_rows(&inventory, &snap, None).unwrap();
    let unmanaged = rows.iter().find(|r| r.runner == "mystery").unwrap();
    assert_eq!(unmanaged.drift, "unmanaged");
    assert_eq!(unmanaged.host, None);
}
