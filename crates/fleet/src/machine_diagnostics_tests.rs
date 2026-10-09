use super::*;
use crate::desired::model::SshTransport;
use crate::machine::{DiscoveredMachine, MachineTarget, parse_inspection};

/// A machine whose reported OS is accepted as is.
fn any_os() -> MachineTarget {
    MachineTarget::Discovered(DiscoveredMachine {
        id: "ts:test".into(),
        address: "test".into(),
        os: OsFamily::Other,
        ssh_transport: SshTransport::Openssh,
    })
}

fn report(os: &str, facts: &[(&str, &str)]) -> MachineReport {
    let mut report = parse_inspection(&any_os(), &format!("RFM\tos\t{os}\nRFM_END\nRFM_RUNNERS\n")).unwrap();
    report.diagnostics = facts.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    report
}

fn check<'a>(checks: &'a [MachineDiagnostic], id: &str) -> &'a MachineDiagnostic {
    checks.iter().find(|c| c.id == id).unwrap()
}

#[test]
fn sleep_ssh_and_startup_distinguish_unknown_disabled_and_unsupported() {
    let mac = evaluate(
        &report(
            "macos",
            &[
                ("tailscale_install", "mac-app-store"),
                ("tailscale_ssh", "false"),
                ("ac_sleep", "0"),
            ],
        ),
        None,
    );
    assert_eq!(
        (
            check(&mac, "tailscale-ssh").status,
            check(&mac, "tailscale-ssh").value.as_str()
        ),
        (DiagnosticStatus::Info, "Not supported")
    );
    assert_eq!(check(&mac, "power-sleep").status, DiagnosticStatus::Pass);
    let unknown = evaluate(&report("macos", &[("ac_sleep", "oops")]), None);
    for id in ["tailscale-install", "tailscale-ssh", "power-sleep"] {
        assert_eq!(check(&unknown, id).status, DiagnosticStatus::Unknown);
    }
    let windows = evaluate(
        &report(
            "windows",
            &[
                ("tailscale_unattended", "false"),
                ("ac_sleep", "900"),
                ("ac_displaysleep", "60"),
            ],
        ),
        None,
    );
    assert_eq!(check(&windows, "tailscale-ssh").value, "Not supported");
    assert_eq!(check(&windows, "tailscale-unattended").status, DiagnosticStatus::Warn);
    assert_eq!(check(&windows, "power-sleep").status, DiagnosticStatus::Warn);
    assert_eq!(check(&windows, "power-displaysleep").status, DiagnosticStatus::Info);
}

#[test]
fn unmasked_linux_targets_do_not_claim_the_machine_will_sleep() {
    let linux = evaluate(
        &report("linux", &[("target_sleep", "static"), ("idle_action", "ignore")]),
        None,
    );
    assert_eq!(check(&linux, "power-sleep").status, DiagnosticStatus::Unknown);
    assert_eq!(check(&linux, "power-idle").status, DiagnosticStatus::Pass);
    let masked = evaluate(
        &report("linux", &[("target_sleep", "masked"), ("idle_action", "suspend")]),
        None,
    );
    assert_eq!(check(&masked, "power-sleep").status, DiagnosticStatus::Pass);
    assert_eq!(check(&masked, "power-idle").status, DiagnosticStatus::Info);
}

#[test]
fn runner_checks_use_actual_services_and_exclude_decommissioned_runners() {
    let inventory = crate::demo_desired::inventory();
    let mut host = inventory.find_host("mac-mini-02").unwrap().clone();
    let mut report = report("macos", &[]);
    report.runners.insert(
        host.runners[0].name.clone(),
        crate::desired::model::LocalRunner::new(&host.runners[0].name, "/runners/a", true, "inactive", Some("2.329.0")),
    );
    let checks = evaluate(&report, Some(&host));
    assert_eq!(
        check(&checks, &format!("runner:{}", host.runners[0].name)).status,
        DiagnosticStatus::Warn
    );
    host.runners[0].decommission = true;
    assert!(
        evaluate(&report, Some(&host))
            .iter()
            .all(|c| c.group != DiagnosticGroup::Runners)
    );
    assert!(
        evaluate(&report, None)
            .iter()
            .all(|c| c.group != DiagnosticGroup::Runners)
    );
}

#[test]
fn incomplete_diagnostics_do_not_replace_unknown_with_partial_success() {
    for (end, expected) in [("", false), ("RFM\tdiag:complete\t1\n", true)] {
        let report = parse_inspection(
            &any_os(),
            &format!("RFM\tos\tlinux\nRFM_END\nRFM\tdiag:tailscale_ssh\ttrue\n{end}RFM_RUNNERS\n"),
        )
        .unwrap();
        assert_eq!(report.diagnostics.contains_key("tailscale_ssh"), expected);
    }
}

#[cfg(unix)]
#[test]
fn posix_collection_reads_fake_macos_and_linux_without_emitting_private_preferences() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let fake = r#"#!/bin/sh
case "${0##*/}" in
  uname) if [ "$1" = -s ]; then printf '%s\n' "$TEST_OS"; else echo arm64; fi ;;
  id) echo builder ;;
  ps) echo /usr/local/bin/tailscaled ;;
  stat) echo builder ;;
  pmset) printf 'Battery Power:\n sleep 5\nAC Power:\n sleep 0\n displaysleep 10\n autorestart 1\n womp 0\n' ;;
  systemctl) case "$2" in tailscaled.service) echo enabled ;; *) echo masked ;; esac ;;
  systemd-analyze) printf '[Login]\nIdleAction=ignore\n# override\nIdleAction=suspend\n' ;;
  tailscale) if [ "$1" = debug ]; then
    printf '{\n  "RunSSH": true,\n  "Persist": {"PrivateNodeKey": "DO_NOT_EXPORT"}\n}\n'
  else
    printf '{\n  "Version": "1.90.0",\n  "BackendState": "Running"\n}\n'
  fi ;;
esac
"#;
    for name in [
        "uname",
        "id",
        "ps",
        "stat",
        "pmset",
        "systemctl",
        "systemd-analyze",
        "tailscale",
    ] {
        let path = dir.path().join(name);
        std::fs::write(&path, fake).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    for os in ["Darwin", "Linux"] {
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(include_str!("../scripts/machine-diagnostics.sh"))
            .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
            .env("TEST_OS", os)
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = String::from_utf8(output.stdout).unwrap();
        assert!(!output.contains("DO_NOT_EXPORT"));
        let report = parse_inspection(&any_os(), &format!("RFM\tos\tother\nRFM_END\n{output}RFM_RUNNERS\n")).unwrap();
        assert_eq!(report.diagnostics["tailscale_ssh"], "true");
        assert_eq!(report.diagnostics["tailscale_version"], "1.90.0");
        assert_eq!(report.diagnostics["identity"], "builder");
        if os == "Darwin" {
            assert_eq!(report.diagnostics["ac_sleep"], "0");
            assert_eq!(report.diagnostics["battery_sleep"], "5");
        } else {
            assert_eq!(report.diagnostics["target_sleep"], "masked");
            assert_eq!(report.diagnostics["idle_action"], "suspend");
        }
    }
}
