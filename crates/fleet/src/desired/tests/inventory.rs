use super::super::inventory::load_inventory;
use super::super::model::{Platform, ServiceMode};
use super::*;

#[test]
fn unknown_ssh_transport_is_rejected() {
    let e = load_err(&base().replace("ssh = \"test-host\"", "ssh = \"test-host\"\nssh_transport = \"shell\""));
    assert_eq!(e, "test: ssh_transport must be openssh or tailscale");
}

#[test]
fn example_inventory_has_safe_generated_paths() {
    let inventory = example_inventory();
    // Every runner lives in its own directory under its host's dedicated root.
    for host in &inventory.hosts {
        for runner in &host.runners {
            let sep = if host.is_windows() { "\\" } else { "/" };
            assert_eq!(
                host.runner_path(runner),
                format!("{}{sep}{}", host.root, runner.directory)
            );
        }
    }
    let studio = inventory.find_host("studio").unwrap();
    assert!(
        studio
            .runners
            .iter()
            .all(|r| studio.runner_path(r).starts_with("~/gh-runners/acme/runner-"))
    );
    let windows = inventory.find_host("windows").unwrap();
    assert_eq!(windows.ssh_target.as_deref(), Some("win-builder"));
    assert_eq!(windows.ssh_setting, "win-builder");
    assert_eq!(windows.platform, Platform::WindowsArm64);
    assert_eq!(windows.platform.runner_asset(), "win-arm64");
    assert_eq!(windows.service_account.as_deref(), Some(".\\builder"));
    assert!(windows.labels.contains(&"tauri".into()));
    let legacy = windows.runners[0].replaces.as_ref().unwrap();
    assert_eq!(legacy.name, "old-windows-01");
    assert_eq!(legacy.directory, "C:\\actions-runner");
    assert_eq!(legacy.repository.as_deref(), Some("acme/desktop"));
    let canary = inventory.find_host("linux-canary").unwrap();
    assert_eq!(canary.platform, Platform::LinuxX64);
    assert_eq!(canary.service_mode, ServiceMode::Systemd);
    assert_eq!(canary.runners[0].name, "linux-canary-01");
    assert!(canary.labels.contains(&"linux-experimental".into()));
    assert!(canary.labels.contains(&"arch".into()));
    assert!(!canary.labels.contains(&"linux-build".into()));
}

#[test]
fn environment_parameter_resolves_ssh_without_storing_secret() {
    let text = base().replace("ssh = \"test-host\"", "ssh = \"${TEST_SSH}\"");
    let inventory = parse_inventory(&text, &env(&[("TEST_SSH", "builder@100.1.2.3")])).unwrap();
    let host = inventory.find_host("test").unwrap();
    assert_eq!(host.ssh_target.as_deref(), Some("builder@100.1.2.3"));
    assert_eq!(host.ssh_setting, "${TEST_SSH}");
    let unset = load(&text).unwrap();
    assert_eq!(unset.find_host("test").unwrap().ssh_target, None);
}

#[test]
fn toml_errors_name_the_file_they_came_from() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fleet.toml");
    std::fs::write(&path, "[organization\n").unwrap();
    let parsed = load_err("[organization\n");
    let detail = parsed.strip_prefix("TOML: ").unwrap();
    assert_eq!(
        load_inventory(&path, &env(&[])).unwrap_err().0,
        format!("cannot load {}: {detail}", path.display())
    );
}

#[test]
fn duplicate_runner_name_is_rejected_across_hosts() {
    let text = format!(
        "{}\n[[hosts]]\nid = \"second\"\nplatform = \"linux-x64\"\nssh = \"second\"\nroot = \"~/gh-runners/acme\"\n\
         service_mode = \"systemd\"\nlabels = [\"fleet\", \"linux-build\"]\nrunners = [{{ name = \"test-01\", directory = \"runner-01\" }}]\n",
        base()
    );
    assert_eq!(load_err(&text), "duplicate runner name: test-01");
}

#[test]
fn runner_directory_cannot_escape_root() {
    let e = load_err(&base().replace("directory = \"runner-01\"", "directory = \"../runner-01\""));
    assert_eq!(
        e,
        "test: runner directory must be one safe path component: '../runner-01'"
    );
}

#[test]
fn runner_directory_rejects_shell_metacharacters() {
    let e = load_err(&base().replace("directory = \"runner-01\"", "directory = \"runner-$(id)\""));
    assert_eq!(e, "invalid test runner directory: 'runner-$(id)'");
}

#[test]
fn service_mode_must_match_platform() {
    let e = load_err(&base().replace("service_mode = \"launchagent\"", "service_mode = \"systemd\""));
    assert_eq!(e, "test: darwin-arm64 requires service_mode='launchagent'");
}

#[test]
fn unknown_platforms_and_service_modes_are_inventory_errors() {
    let e = load_err(&base().replace("platform = \"darwin-arm64\"", "platform = \"darwin-x64\""));
    assert_eq!(e, "test: unsupported platform 'darwin-x64'");
    let e = load_err(&base().replace("service_mode = \"launchagent\"", "service_mode = \"cron\""));
    assert_eq!(e, "test: darwin-arm64 requires service_mode='launchagent'");
}

#[test]
fn linux_host_requires_exactly_one_workload_class() {
    let linux = base()
        .replace("platform = \"darwin-arm64\"", "platform = \"linux-x64\"")
        .replace("service_mode = \"launchagent\"", "service_mode = \"systemd\"")
        .replace("labels = [\"fleet\", \"macos-build\"]", "labels = [\"fleet\"]");
    let expected = "test: include exactly one platform capability label: linux-build, linux-experimental";
    assert_eq!(load_err(&linux), expected);
    let ambiguous = linux.replace(
        "labels = [\"fleet\"]",
        "labels = [\"fleet\", \"linux-build\", \"linux-experimental\"]",
    );
    assert_eq!(load_err(&ambiguous), expected);
}

#[test]
fn more_inventory_rules_match_runner_fleet() {
    let base = base();
    let replaces = |legacy: &str| {
        base.replace(
            "directory = \"runner-01\" }",
            &format!("directory = \"runner-01\", replaces = {{ {legacy} }} }}"),
        )
    };
    let cases = [
        (
            base.replace("https://github.com/acme", "https://github.com/other"),
            "organization url must be 'https://github.com/acme'",
        ),
        (
            base.replace("~/gh-runners/acme", "~/elsewhere"),
            "test: root must be beneath ~/gh-runners",
        ),
        (
            base.replace("~/gh-runners/acme", "~/gh-runners/../x"),
            "test: root may not contain '..'",
        ),
        (
            base.replace("[\"fleet\", \"macos-build\"]", "[\"macos-build\"]"),
            "test: missing required labels: fleet",
        ),
        (
            base.replace("id = \"test\"", "id = \"bad id\""),
            "invalid host id: 'bad id'",
        ),
        (
            base.replace("ssh = \"test-host\"", "ssh = \"test-host\"\nservice_account = \"x\""),
            "test: service_account is supported only on Windows",
        ),
        (
            base.replace("ssh = \"test-host\"", "ssh = \"test-host\"\nenabled = \"no\""),
            "enabled must be true or false",
        ),
        (
            base.replace("[organization]", "[org]"),
            "invalid inventory structure: 'organization'",
        ),
        (
            base.replace("{ name = \"test-01\", directory = \"runner-01\" },", ""),
            "test: define at least one runner",
        ),
        (
            replaces("name = \"old\", directory = \"/\""),
            "test: legacy path is too broad",
        ),
        (
            replaces("name = \"old\", directory = \"actions\""),
            "test: legacy path must be an exact absolute or ~/ path",
        ),
        (
            replaces("name = \"old\", directory = \"~/a\", repository = \"other/desktop\""),
            "legacy repository must belong to the inventory organization",
        ),
        (
            replaces("name = \"test-01\", directory = \"~/a\""),
            "duplicate runner name: test-01",
        ),
        (
            replaces("name = \"old\", directory = \"~/gh-runners/acme/runner-01\""),
            "duplicate legacy runner path on test: ~/gh-runners/acme/runner-01",
        ),
    ];
    for (text, expected) in cases {
        assert_eq!(load_err(&text), expected);
    }
}

#[test]
fn windows_paths_are_validated_case_insensitively() {
    let windows = inventory_toml(
        r#"
[[hosts]]
id = "win"
platform = "windows-x64"
ssh = "win"
root = "c:/GH-RUNNERS/acme"
service_mode = "windows-service"
labels = ["fleet", "windows-build"]
runners = [
  { name = "w1", directory = "runner-01", replaces = { name = "old", directory = "C:\\actions-runner" } },
  { name = "w2", directory = "RUNNER-01" },
]
"#,
    )
    .replace("https://github.com/acme", "https://github.com/acme/");
    assert_eq!(
        load_err(&windows),
        "duplicate runner path on win: c:\\gh-runners\\acme\\runner-01"
    );
    let outside = windows.replace("c:/GH-RUNNERS/acme", "D:/gh-runners/x");
    assert_eq!(load_err(&outside), "win: Windows root must be beneath C:\\gh-runners");
    let broad = windows
        .replace("C:\\\\actions-runner", "C:\\\\")
        .replace("RUNNER-01", "runner-02");
    assert_eq!(load_err(&broad), "win: legacy Windows path is too broad");
}

#[test]
fn exclude_labels_remove_a_host_label_from_one_runner() {
    let inventory = load(&base().replace(
        "{ name = \"test-01\", directory = \"runner-01\" },",
        "{ name = \"test-01\", directory = \"runner-01\", labels = [\"macos-e2e\"], exclude_labels = [\"macos-build\"] },\n  { name = \"test-02\", directory = \"runner-02\" },",
    ))
    .unwrap();
    let host = inventory.find_host("test").unwrap();
    assert_eq!(host.labels_for(&host.runners[0]), ["fleet", "macos-e2e"]);
    assert_eq!(host.labels_for(&host.runners[1]), ["fleet", "macos-build"]);
}

#[test]
fn exclude_labels_must_name_a_host_label() {
    let excluding = |label: &str| {
        base().replace(
            "{ name = \"test-01\", directory = \"runner-01\" },",
            &format!("{{ name = \"test-01\", directory = \"runner-01\", exclude_labels = [\"{label}\"] }},"),
        )
    };
    assert_eq!(
        load_err(&excluding("xcode")),
        "test-01: exclude_labels may only remove host labels; 'xcode' is not one"
    );
    assert_eq!(
        load_err(&excluding("fleet")),
        "test-01: the fleet label cannot be excluded"
    );
}

#[test]
fn example_inventory_keeps_the_gui_slots_off_builds() {
    // A GUI test lane shares one desktop, so a runner that takes GUI jobs never
    // also takes builds (builds would occupy the desktop).
    let inventory = example_inventory();
    let mut gui = 0;
    for host in &inventory.hosts {
        for runner in host.desired_runners() {
            let labels = host.labels_for(runner);
            if labels.iter().any(|l| l == "macos-e2e") {
                gui += 1;
                assert!(
                    !labels.iter().any(|l| l == "macos-build"),
                    "{} takes GUI jobs and builds",
                    runner.name
                );
            }
        }
    }
    assert!(gui >= 1, "some runner must take macOS GUI jobs");
    // The Studio still has build capacity.
    let studio = inventory.find_host("studio").unwrap();
    assert!(
        studio
            .desired_runners()
            .any(|r| studio.labels_for(r).iter().any(|l| l == "macos-build"))
    );
}
