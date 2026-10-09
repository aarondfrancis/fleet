use super::super::model::RunnerRole;
use super::super::py::repr;
use super::super::remote::{FleetRemote, InstallRequest, windows_doctor};
use super::super::windows_logon::doctor_script;
use super::*;

/// A Windows host, `gui`, whose runner runs in `.\builder`'s desktop session.
pub(super) fn logon_toml() -> String {
    inventory_toml(
        r#"
[[hosts]]
id = "gui"
platform = "windows-x64"
ssh = "gui-host"
root = "C:\\gh-runners\\acme"
service_mode = "windows-logon"
service_account = ".\\builder"
labels = ["fleet", "windows-build"]
runners = [{ name = "gui-01", directory = "runner-01", labels = ["windows-e2e-x64"] }]
"#,
    )
}

fn logon() -> Inventory {
    load(&logon_toml()).unwrap()
}

#[test]
fn windows_logon_is_a_valid_windows_mode() {
    let inventory = logon();
    let host = inventory.find_host("gui").unwrap();
    assert!(host.is_windows_logon());
    assert!(!host.needs_service_password());
    assert_eq!(host.service_account.as_deref(), Some(".\\builder"));
}

fn with_service_account(account: &str) -> String {
    logon_toml().replace(
        r#"service_account = ".\\builder""#,
        &format!("service_account = {}", toml::Value::String(account.into())),
    )
}

#[test]
fn service_accounts_are_plain_windows_account_names() {
    for account in [
        r".\builder",
        r"BUILD\ci-runner_2",
        "ci.runner@example.com",
        "Runner Account",
    ] {
        assert!(load(&with_service_account(account)).is_ok(), "rejected {account}");
    }
    for account in [r"x'; Remove-Item C:\ #", "x\u{2019}y", "$env:PATH", "a`b", "a\"b"] {
        assert_eq!(
            load_err(&with_service_account(account)),
            format!("gui: invalid service_account: {}", repr(account))
        );
    }
}

#[test]
fn windows_logon_requires_the_auto_logon_account() {
    let e = load_err(&logon_toml().replace("service_account = \".\\\\builder\"\n", ""));
    assert_eq!(e, "gui: windows-logon requires service_account (the auto-logon user)");
}

#[test]
fn windows_logon_is_rejected_off_windows() {
    let e = load_err(
        &logon_toml()
            .replace("platform = \"windows-x64\"", "platform = \"darwin-arm64\"")
            .replace(
                "labels = [\"fleet\", \"windows-build\"]",
                "labels = [\"fleet\", \"macos-build\"]",
            ),
    );
    assert_eq!(e, "gui: darwin-arm64 requires service_mode='launchagent'");
    let e = load_err(&logon_toml().replace("service_mode = \"windows-logon\"", "service_mode = \"launchagent\""));
    assert_eq!(
        e,
        "gui: windows-x64 requires service_mode='windows-service' or 'windows-logon'"
    );
}

#[test]
fn logon_mode_installs_without_a_service_password() {
    let inventory = logon();
    let fleet = FakeFleet::default();
    let secrets = Secrets {
        windows_service_password: secret("unused-service"),
        linux_sudo_password: secret("unused-sudo"),
    };
    ops(&inventory, &fleet, secrets)
        .apply_host("gui", Some("gui-01"))
        .unwrap();
    let world = fleet.world();
    assert_eq!(world.calls[0], "install gui gui-01 token=short-lived-registration");
    assert_eq!(world.calls.last().unwrap(), "start gui gui-01");
}

#[test]
fn install_registers_the_logon_mode_and_account() {
    let inventory = logon();
    let host = inventory.find_host("gui").unwrap();
    let (commands, remote) = recording_remote();
    remote
        .install(
            host,
            &host.runners[0],
            &InstallRequest {
                asset: &super::super::github::ReleaseAsset {
                    version: "2.999.0".into(),
                    name: "actions-runner-win-x64-2.999.0.zip".into(),
                    url: "https://github.com/actions/runner/releases/download/v2.999.0/actions-runner-win-x64-2.999.0.zip"
                        .into(),
                    sha256: Some("a".repeat(64)),
                },
                token: "tok",
                organization_url: "https://github.com/acme",
                runner_group: "trusted",
                service_password: &Transient::NONE,
                sudo_password: &Transient::NONE,
            },
        )
        .unwrap();
    let script = decoded(&commands);
    for expected in [
        "$env:RF_SERVICE_MODE = 'windows-logon'",
        "$env:RF_SERVICE_ACCOUNT = '.\\builder'",
        "$env:RF_SERVICE_PASSWORD = ''",
    ] {
        assert!(script.contains(expected), "{expected}");
    }
}

#[test]
fn start_enables_and_starts_the_exact_logon_task() {
    let inventory = logon();
    let host = inventory.find_host("gui").unwrap();
    let (commands, remote) = recording_remote();
    remote.start(host, &host.runners[0], &Transient::NONE).unwrap();
    let script = decoded(&commands);
    assert!(script.contains("$task = 'runner-fleet-gui-01'"));
    assert!(script.contains("$path = 'C:\\gh-runners\\acme\\runner-01'"));
    assert!(script.contains("Start-ScheduledTask"));
    assert!(!script.contains("Start-Service"));
    assert!(!script.contains("Stop-ScheduledTask"));
}

#[test]
fn restart_stops_the_listener_before_starting_again() {
    let inventory = logon();
    let host = inventory.find_host("gui").unwrap();
    let (commands, remote) = recording_remote();
    remote
        .restart(host, &host.runners[0], RunnerRole::Declared, &Transient::NONE)
        .unwrap();
    let script = decoded(&commands);
    assert!(script.find("Stop-ScheduledTask").unwrap() < script.find("Start-ScheduledTask").unwrap());
    assert!(!script.contains("Restart-Service"));
}

#[test]
fn doctor_checks_auto_logon_and_session_instead_of_the_service() {
    let script = windows_doctor(logon().find_host("gui").unwrap());
    assert!(script.ends_with(&doctor_script(".\\builder")));
    assert!(!script.contains("Windows service starts without interactive login"));
}
