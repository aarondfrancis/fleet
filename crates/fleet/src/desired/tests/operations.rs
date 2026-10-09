use super::super::model::{CheckStatus, DoctorCheck};
use super::super::operations::doctor;
use super::super::parity;
use super::super::testing::REMOVAL_TOKEN;
use super::*;

/// GitHub and host facts under which studio's legacy runner may be retired.
fn retirable(inventory: &Inventory) -> (Vec<GithubRunner>, Vec<LocalRunner>) {
    let (host, runner) = studio(inventory);
    let old = runner.replaces.as_ref().unwrap();
    let labels = host.labels_for(runner);
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    (
        vec![
            gr(&runner.name, "online", false, &labels, Some(true)),
            gr(&old.name, "online", false, &[], Some(true)),
        ],
        vec![
            LocalRunner::new(&runner.name, &host.runner_path(runner), true, "active", None),
            LocalRunner::new(&old.name, &old.directory, true, "active", None),
        ],
    )
}

fn github_runner<'a>(runners: &'a mut [GithubRunner], name: &str) -> &'a mut GithubRunner {
    runners.iter_mut().find(|r| r.name == name).unwrap()
}

fn local_runner<'a>(runners: &'a mut [LocalRunner], name: &str) -> &'a mut LocalRunner {
    runners.iter_mut().find(|r| r.name == name).unwrap()
}

#[test]
fn retirement_with_a_healthy_replacement_deregisters_the_legacy_runner() {
    let inventory = example_inventory();
    let (github, local) = retirable(&inventory);
    let fleet = FakeFleet::new(github, local);
    let message = ops(&inventory, &fleet, Secrets::default())
        .retire_legacy("studio", "studio-01")
        .unwrap();
    assert_eq!(
        message,
        "retired Legacy-Studio; preserved ~/actions-runner for recovery"
    );
    let world = fleet.world();
    assert_eq!(world.tokens, ["removal"]);
    assert_eq!(world.calls, ["retire studio Legacy-Studio"]);
}

#[test]
fn retirement_guards_refuse_before_minting_a_removal_token() {
    type Change = fn(&mut Vec<GithubRunner>, &mut Vec<LocalRunner>);
    let cases: [(&str, Change); 9] = [
        ("replacement studio-01 must be online and idle", |github, _| {
            github.retain(|r| r.name != "studio-01");
        }),
        ("replacement studio-01 must be online and idle", |github, _| {
            github_runner(github, "studio-01").busy = true;
        }),
        (
            "replacement studio-01 must belong to Acme trusted builders",
            |github, _| {
                github_runner(github, "studio-01").in_desired_group = Some(false);
            },
        ),
        ("replacement studio-01 has label drift: missing xcode", |github, _| {
            github_runner(github, "studio-01").labels.remove("xcode");
        }),
        ("replacement studio-01 has label drift: unexpected gpu", |github, _| {
            github_runner(github, "studio-01").custom_labels.insert("gpu".into());
        }),
        (
            "replacement studio-01 must be configured with an active service",
            |_, local| {
                local_runner(local, "studio-01").service_state = "inactive".into();
            },
        ),
        ("legacy runner Legacy-Studio does not exist on GitHub", |github, _| {
            github.retain(|r| r.name != "Legacy-Studio");
        }),
        ("legacy runner Legacy-Studio is busy", |github, _| {
            github_runner(github, "Legacy-Studio").busy = true;
        }),
        (
            "legacy exact directory ~/actions-runner is not configured",
            |_, local| {
                local_runner(local, "Legacy-Studio").configured = false;
            },
        ),
    ];
    let inventory = example_inventory();
    for (expected, change) in cases {
        let (mut github, mut local) = retirable(&inventory);
        change(&mut github, &mut local);
        let fleet = FakeFleet::new(github, local);
        let error = ops(&inventory, &fleet, Secrets::default())
            .retire_legacy("studio", "studio-01")
            .unwrap_err();
        assert_eq!(error.0, expected);
        assert!(fleet.world().tokens.is_empty(), "{expected}");
    }
}

#[test]
fn apply_uses_fake_boundaries_and_transient_token() {
    let inventory = example_inventory();
    let fleet = FakeFleet::default();
    let messages = ops(&inventory, &fleet, Secrets::default())
        .apply_host("mac-build", Some("mac-build-01"))
        .unwrap();
    assert_eq!(
        fleet.world().calls,
        [
            "install mac-build mac-build-01 token=short-lived-registration",
            "labels 44 fleet,macos-build,xcode,host-mac-build",
            "group 5 44",
            "start mac-build mac-build-01",
        ]
    );
    assert!(
        messages[0].starts_with("installed mac-build-01 v2.999.0"),
        "{messages:?}"
    );
}

#[test]
fn windows_registers_directly_into_trusted_group() {
    let inventory = example_inventory();
    let fleet = FakeFleet::default();
    let secrets = Secrets {
        windows_service_password: secret("transient-password"),
        linux_sudo_password: secret("unused-sudo"),
    };
    ops(&inventory, &fleet, secrets)
        .apply_host("windows", Some("windows-01"))
        .unwrap();
    assert_eq!(
        fleet.world().calls[0],
        "install windows windows-01 token=short-lived-registration service-password=transient-password"
    );
}

#[test]
fn windows_install_requires_the_service_account_password() {
    let inventory = example_inventory();
    let fleet = FakeFleet::default();
    let e = ops(&inventory, &fleet, Secrets::default())
        .apply_host("windows", Some("windows-01"))
        .unwrap_err();
    assert_eq!(
        e.0,
        "the password for .\\builder is required to install a runner on windows"
    );
    assert!(fleet.world().calls.is_empty());
}

#[test]
fn linux_sudo_password_is_transient_install_input() {
    let inventory = example_inventory();
    let fleet = FakeFleet::default();
    let secrets = Secrets {
        linux_sudo_password: secret("transient-password"),
        windows_service_password: secret("unused-service"),
    };
    ops(&inventory, &fleet, secrets)
        .apply_host("linux-canary", Some("linux-canary-01"))
        .unwrap();
    assert_eq!(
        fleet.world().calls[0],
        "install linux-canary linux-canary-01 token=short-lived-registration sudo-password=transient-password"
    );
}

#[test]
fn migrating_host_requires_one_runner_at_a_time() {
    let inventory = example_inventory();
    let fleet = FakeFleet::default();
    let e = ops(&inventory, &fleet, Secrets::default())
        .apply_host("studio", None)
        .unwrap_err();
    assert_eq!(
        e.0,
        "studio has a capacity-preserving migration; select exactly one --runner"
    );
}

#[test]
fn apply_refuses_name_collisions_and_orphans_without_mutation() {
    let inventory = example_inventory();
    let path = "~/gh-runners/acme/runner-01";
    let cases = [
        // GitHub has the name, the exact directory isn't configured.
        (
            vec![gr("mac-build-01", "online", false, &[], Some(true))],
            vec![],
            "mac-build-01: GitHub name exists but exact local directory is missing; refusing name collision",
        ),
        // Configured locally, missing on GitHub.
        (
            vec![],
            vec![LocalRunner::new("mac-build-01", path, true, "active", None)],
            "mac-build-01: exact local directory is configured but GitHub runner is missing; refusing replacement",
        ),
        // Busy with a stopped service.
        (
            vec![gr("mac-build-01", "online", true, &[], Some(true))],
            vec![LocalRunner::new("mac-build-01", path, true, "inactive", None)],
            "mac-build-01 is busy; refusing to restart its service",
        ),
        // Offline on GitHub while the service is active.
        (
            vec![gr("mac-build-01", "offline", false, &[], Some(true))],
            vec![LocalRunner::new("mac-build-01", path, true, "active", None)],
            "mac-build-01: GitHub reports offline while the service is active; inspect logs before mutation",
        ),
    ];
    for (github, local, expected) in cases {
        let fleet = FakeFleet::new(github, local);
        let e = ops(&inventory, &fleet, Secrets::default())
            .apply_host("mac-build", Some("mac-build-01"))
            .unwrap_err();
        assert_eq!(e.0, expected);
        let world = fleet.world();
        assert!(world.calls.is_empty() && world.tokens.is_empty(), "{expected}");
    }
}

#[test]
fn apply_reconciles_labels_with_the_exact_desired_set() {
    let inventory = example_inventory();
    let fleet = FakeFleet::new(
        vec![gr("mac-build-01", "online", false, &["fleet"], Some(true))],
        vec![LocalRunner::new("mac-build-01", "p", true, "active", None)],
    );
    let messages = ops(&inventory, &fleet, Secrets::default())
        .apply_host("mac-build", Some("mac-build-01"))
        .unwrap();
    assert_eq!(messages, ["reconciled labels for mac-build-01"]);
    assert_eq!(
        fleet.world().calls,
        ["labels 11 fleet,macos-build,xcode,host-mac-build"]
    );
}

#[test]
fn decommission_deregisters_with_a_removal_token_minted_for_it() {
    let inventory = super::decommission::decom();
    let fleet = FakeFleet::new(
        vec![super::decommission::dg("linux-01", "online", false, 7)],
        vec![LocalRunner::new(
            "linux-01",
            "~/gh-runners/acme/runner-01",
            true,
            "active",
            None,
        )],
    );
    let secrets = Secrets {
        linux_sudo_password: secret("typed-sudo"),
        ..Default::default()
    };
    ops(&inventory, &fleet, secrets)
        .decommission_runner("linux", "linux-01")
        .unwrap();
    assert_eq!(
        fleet.world().calls,
        [format!(
            "decommission linux linux-01 token={REMOVAL_TOKEN} sudo-password=typed-sudo"
        )]
    );
}

#[test]
fn doctor_reports_runner_and_decommission_state() {
    let inventory = super::decommission::decom();
    let fleet = FakeFleet::new(
        vec![],
        vec![
            super::decommission::dl("win-01", true, "stopped"),
            super::decommission::dl("win-02", true, "running"),
        ],
    );
    let checks = doctor(&inventory, &fleet, &fleet, Some("win")).unwrap();
    let find = |name: &str| checks.iter().find(|c| c.check == name).unwrap();
    assert_eq!(find("github-auth").status, CheckStatus::Pass);
    assert_eq!(find("runner:win-01").status, CheckStatus::Fail);
    assert_eq!(find("decommission:win-02").status, CheckStatus::Warn);
    assert_eq!(
        find("decommission:win-02").detail,
        "still configured at C:\\gh-runners\\acme\\runner-02; run: bin/fleet decommission win --runner win-02"
    );
}

#[test]
fn doctor_checks_keep_the_original_json_keys() {
    let checks = [DoctorCheck::new("win", "ssh", CheckStatus::Warn, "slow")];
    assert_eq!(
        parity::doctor(&checks),
        serde_json::json!([{ "host": "win", "check": "ssh", "status": "warn", "detail": "slow" }])
    );
}
