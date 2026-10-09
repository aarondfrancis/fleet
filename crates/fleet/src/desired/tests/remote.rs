use super::super::github::ReleaseAsset;
use super::super::model::{CheckStatus, RunnerRole, SshTransport};
use super::super::remote::{
    FleetRemote, InstallRequest, parse_doctor, parse_posix_probe, parse_windows_probe, posix_doctor, probe_script,
    windows_doctor,
};
use super::*;

fn asset(platform: &str, version: &str, extension: &str) -> ReleaseAsset {
    let name = format!("actions-runner-{platform}-{version}.{extension}");
    ReleaseAsset {
        url: format!("https://github.com/actions/runner/releases/download/v{version}/{name}"),
        name,
        version: version.into(),
        sha256: Some("a".repeat(64)),
    }
}

#[test]
fn tailscale_transport_keeps_payload_on_stdin() {
    let mut host = example_inventory().find_host("linux-canary").unwrap().clone();
    host.ssh_transport = SshTransport::Tailscale;
    let (command, input) = recording_remote().1.script_command(&host, "secret-payload").unwrap();
    assert_eq!(command, ["tailscale", "ssh", "linux-canary", "bash -s"]);
    assert_eq!(input, "secret-payload");
    assert!(!command.join(" ").contains("secret-payload"));
}

#[test]
fn openssh_transport_is_batch_and_strict() {
    let host = example_inventory().find_host("studio").unwrap().clone();
    let (command, _) = recording_remote().1.script_command(&host, "x").unwrap();
    let joined = command.join(" ");
    for option in [
        "BatchMode=yes",
        "ConnectTimeout=15",
        "StrictHostKeyChecking=yes",
        "ForwardAgent=no",
    ] {
        assert!(joined.contains(option), "{joined}");
    }
    assert_eq!(&command[command.len() - 3..], ["--", "studio", "bash -s"]);
}

#[test]
fn posix_probe_parser_ignores_login_banner() {
    let out = "Welcome\nRF1\tmac-01\t/Users/a/gh-runners/r1\ttrue\tactive\t2.336.0\n";
    let runner = &parse_posix_probe(out).unwrap()["mac-01"];
    assert!(runner.configured);
    assert_eq!(runner.service_state, "active");
    assert_eq!(runner.version.as_deref(), Some("2.336.0"));
}

#[test]
fn windows_probe_parser_handles_single_object() {
    let out = r#"{"name":"windows-01","path":"C:\\gh-runners\\runner-01","configured":true,"service":"Running","version":"2.336.0"}"#;
    assert_eq!(parse_windows_probe(out).unwrap()["windows-01"].service_state, "running");
    let list = r#"[{"name":"a","path":"p","configured":false,"service":"missing","version":null}]"#;
    assert_eq!(parse_windows_probe(list).unwrap()["a"].version, None);
}

#[test]
fn windows_probe_values_are_truthy_the_python_way() {
    let rows = r#"[
        {"name":"a","path":"p","configured":[],"service":"missing","version":0},
        {"name":"b","path":"p","configured":{},"service":"missing","version":[]},
        {"name":"c","path":"p","configured":[1],"service":"running","version":2}
    ]"#;
    let runners = parse_windows_probe(rows).unwrap();
    let configured: Vec<(bool, Option<&str>)> =
        runners.values().map(|r| (r.configured, r.version.as_deref())).collect();
    assert_eq!(configured, [(false, None), (false, None), (true, Some("2"))]);
}

#[test]
fn malformed_probe_is_rejected() {
    assert_eq!(
        parse_posix_probe("unrelated output").unwrap_err().0,
        "host returned no recognizable runner probe data"
    );
    assert_eq!(
        parse_posix_probe("RF1\tonly\tthree").unwrap_err().0,
        "malformed runner probe row: 'RF1\\tonly\\tthree'"
    );
    assert_eq!(
        parse_windows_probe("[]").unwrap_err().0,
        "host returned no runner probe data"
    );
}

#[test]
fn doctor_and_probe_scripts_are_read_only() {
    let inventories = [example_inventory(), load(&super::windows_logon::logon_toml()).unwrap()];
    for host in inventories.iter().flat_map(|i| &i.hosts) {
        let scripts = [
            probe_script(host),
            if host.is_windows() {
                windows_doctor(host)
            } else {
                posix_doctor(host)
            },
        ];
        for script in scripts {
            let lowered = script.to_lowercase();
            for mutation in [
                "svc.sh",
                "config.sh",
                "config.cmd",
                "rm ",
                "start-service",
                "stop-service",
                "systemctl start",
                "systemctl stop",
                "launchctl load",
                "launchctl bootstrap",
                "sudo -s",
                "enable-scheduledtask",
                "start-scheduledtask",
                "stop-scheduledtask",
                "register-scheduledtask",
                "/setacvalueindex",
                "/setdcvalueindex",
                "/setactive",
                "set-itemproperty",
                "new-itemproperty",
                "reg add",
            ] {
                assert!(
                    !lowered.contains(mutation),
                    "{} read-only script contains {mutation}",
                    host.id
                );
            }
        }
    }
}

#[test]
fn windows_doctor_power_checks_parse_as_tagged_rows() {
    let out = "RFD\tpower:ac-display\tpass\tAC display off never happens\nRFD\tpower:connected-standby\twarn\tModern Standby disconnects networking on AC\n";
    let checks = parse_doctor("windows", out).unwrap();
    let names: Vec<&str> = checks.iter().map(|c| c.check.as_str()).collect();
    assert_eq!(names, ["power:ac-display", "power:connected-standby"]);
    assert_eq!(checks[1].status, CheckStatus::Warn);
}

#[test]
fn doctor_parser_is_non_mutating_tagged_data() {
    let checks = parse_doctor("studio", "RFDoops\nRFD\tssh\tpass\tconnected\n").unwrap();
    assert_eq!(checks[0].detail, "connected");
}

#[test]
fn windows_transport_uses_line_framed_stdin_without_exposing_token() {
    let inventory = example_inventory();
    let host = inventory.find_host("windows").unwrap();
    let (commands, remote) = recording_remote();
    remote
        .install(
            host,
            &host.runners[0],
            &InstallRequest {
                asset: &asset("win-arm64", "2.999.0", "zip"),
                token: "short-lived-secret",
                organization_url: &inventory.organization.url,
                runner_group: &inventory.organization.runner_group,
                service_password: &secret("transient-password"),
                sudo_password: &Transient::NONE,
            },
        )
        .unwrap();
    let command = commands.last().unwrap().command.join(" ");
    let input = sent(&commands);
    assert!(command.contains("powershell.exe"));
    assert!(command.contains("[Console]::In.ReadLine()"));
    for secret in ["short-lived-secret", "transient-password"] {
        assert!(!command.contains(secret));
        assert!(!input.contains(secret));
    }
    assert!(input.ends_with('\n') && input.trim().lines().count() == 1);
    let decoded = decoded(&commands);
    assert!(decoded.contains("$env:RF_TOKEN = 'short-lived-secret'"));
    assert!(decoded.contains("$env:RF_SERVICE_PASSWORD = 'transient-password'"));
    assert!(decoded.contains("$env:RF_SUDO_PASSWORD = ''"));
    assert!(decoded.contains("$env:RF_LABELS = 'fleet,windows-build,tauri,host-windows,windows-e2e'"));
}

#[test]
fn posix_install_runs_every_service_change_through_run_as_root() {
    let inventory = example_inventory();
    let host = inventory.find_host("linux-canary").unwrap();
    let (commands, remote) = recording_remote();
    let mut asset = asset("linux-x64", "2.330.0", "tar.gz");
    asset.sha256 = None;
    remote
        .install(
            host,
            &host.runners[0],
            &InstallRequest {
                asset: &asset,
                token: "registration-token",
                organization_url: "https://github.com/acme",
                runner_group: "trusted",
                service_password: &Transient::NONE,
                sudo_password: &secret("typed-sudo"),
            },
        )
        .unwrap();
    let input = sent(&commands);
    assert!(input.contains("\nRF_SUDO_PASSWORD=typed-sudo\n"));
    assert!(input.contains("\nRF_SERVICE_PASSWORD=''\n"));
    assert!(!input.contains("export RF_"), "{input}");
    assert!(!input.contains("sudo -n ./svc.sh"), "{input}");
    assert_eq!(input.matches("run_as_root() {").count(), 1);
    let definition = input.find("run_as_root() {").unwrap();
    assert!(definition < input.find("run_as_root ./svc.sh install").unwrap());
}

#[test]
fn legacy_restart_and_logs_target_the_legacy_directory() {
    let inventory = example_inventory();
    let (host, runner) = studio(&inventory);
    let legacy = runner.replaces.as_ref().unwrap();
    let (commands, remote) = recording_remote();
    remote
        .restart(host, runner, RunnerRole::Legacy(legacy), &Transient::NONE)
        .unwrap();
    let script = sent(&commands);
    assert!(script.contains("\nroot=\"$HOME\"/actions-runner\n"), "{script}");
    assert!(script.ends_with("echo \"restarted Legacy-Studio\"\n"), "{script}");
    remote.logs(host, runner, RunnerRole::Legacy(legacy), 20).unwrap();
    assert!(sent(&commands).contains("\nroot=\"$HOME\"/actions-runner\n"));
    remote.logs(host, runner, RunnerRole::Declared, 20).unwrap();
    assert!(sent(&commands).contains("\nroot=\"$HOME\"/gh-runners/acme/runner-01\n"));
}
