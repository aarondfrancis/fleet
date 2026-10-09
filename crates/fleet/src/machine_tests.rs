use super::*;
use crate::desired::process::{CommandError, CommandResult, CommandRunner, NonZeroExit};
use std::sync::Mutex;

fn inventory_host(id: &str) -> Host {
    crate::demo_desired::inventory().find_host(id).unwrap().clone()
}

fn host(id: &str) -> MachineTarget {
    MachineTarget::Inventory(inventory_host(id))
}

const MAC_HEALTH: &str = "RFM\tos\tmacos\nRFM\tos_version\t15.6\nRFM\tcores\t12\nRFM\tcpu\t23.50\nRFM\tload\t1.5 2.0 0.8\nRFM\tmemory_total\t24000000000\nRFM\tmemory_available\t8000000000\nRFM\tuptime\t86400\nRFM\tdisk\t/\t500000000000\t300000000000\nRFM_END\nRFM_RUNNERS\n";
const MAC_RUNNERS: &str = "RF1\tmini-02\t/Users/ci/gh-runners/acme-demo/runner-01\ttrue\tactive\t2.329.0\n";

#[test]
fn parses_health_and_exact_runner_facts() {
    let report = parse_inspection(
        &host("mac-mini-02"),
        &format!("login banner\n{MAC_HEALTH}{MAC_RUNNERS}"),
    )
    .unwrap();
    assert_eq!(
        report,
        MachineReport {
            os: OsFamily::Macos,
            os_version: Some("15.6".into()),
            cpu_percent: Some(23.5),
            load_average: Some([1.5, 2.0, 0.8]),
            logical_cores: Some(12),
            memory_total_bytes: Some(24_000_000_000),
            memory_available_bytes: Some(8_000_000_000),
            uptime_secs: Some(86400),
            boot_id: None,
            disks: vec![DiskStats {
                mount_point: "/".into(),
                total_bytes: 500_000_000_000,
                available_bytes: 300_000_000_000
            }],
            runners: [(
                "mini-02".into(),
                LocalRunner::new(
                    "mini-02",
                    "/Users/ci/gh-runners/acme-demo/runner-01",
                    true,
                    "active",
                    Some("2.329.0")
                )
            )]
            .into(),
            warnings: vec![],
            diagnostics: BTreeMap::new(),
        }
    );
}

#[test]
fn unavailable_metrics_stay_unknown_but_runner_data_survives() {
    let output = format!("RFM\tos\tmacos\nRFM\tcpu\t\nRFM_END\nRFM_RUNNERS\n{MAC_RUNNERS}");
    let report = parse_inspection(&host("mac-mini-02"), &output).unwrap();
    assert_eq!(
        (
            report.cpu_percent,
            report.memory_total_bytes,
            report.memory_available_bytes
        ),
        (None, None, None)
    );
    assert!(report.runners["mini-02"].configured);
    assert!(!report.warnings.is_empty());
}

#[test]
fn boot_session_identity_survives_health_protocol_parsing() {
    for boot_id in ["boot-session-uuid", "2026-10-08T11:00:00.0000000Z"] {
        let output = format!("RFM\tboot_id\t{boot_id}\n{MAC_HEALTH}{MAC_RUNNERS}");
        let report = parse_inspection(&host("mac-mini-02"), &output).unwrap();
        assert_eq!(report.boot_id.as_deref(), Some(boot_id));
    }
}

#[test]
fn machines_without_inventory_runners_can_report_health() {
    for (id, os, runner_output) in [("mac-mini-02", "macos", ""), ("win-builder", "windows", "[]")] {
        let mut host = inventory_host(id);
        host.runners.clear();
        let report = parse_inspection(
            &MachineTarget::Inventory(host),
            &format!("RFM\tos\t{os}\nRFM_END\nRFM_RUNNERS\n{runner_output}"),
        )
        .unwrap();
        assert!(report.runners.is_empty());
    }
}

#[test]
fn windows_crlf_json_and_unicode_are_preserved() {
    let output = "RFM\tos\twindows\r\nRFM\tos_version\tWindows 11 Pro\r\nRFM\tcpu\t12\r\nRFM\tmemory_total\t32000000000\r\nRFM\tmemory_available\t16000000000\r\nRFM\tdisk\tC:\\\t512000000000\t140000000000\r\nRFM_END\r\nRFM_RUNNERS\r\n[{\"name\":\"win-builder\",\"path\":\"C:\\\\runners\\\\équipe\",\"configured\":true,\"service\":\"running\",\"version\":\"2.329.0\"}]\r\n";
    let report = parse_inspection(&host("win-builder"), output).unwrap();
    assert_eq!(report.os, OsFamily::Windows);
    assert_eq!(report.runners["win-builder"].path, "C:\\runners\\équipe");
    assert_eq!(report.disks[0].mount_point, "C:\\");
}

#[test]
fn linux_and_missing_runner_configuration_are_readable() {
    let mut linux = inventory_host("build-linux-01");
    linux.runners.truncate(1);
    let linux = MachineTarget::Inventory(linux);
    let output = format!(
        "{}RF1\tlinux-01-a\t/home/builder/runners/a\tfalse\tmissing\t\n",
        MAC_HEALTH.replace("macos", "linux")
    );
    let report = parse_inspection(&linux, &output).unwrap();
    assert_eq!(
        report.runners["linux-01-a"],
        LocalRunner::new("linux-01-a", "/home/builder/runners/a", false, "missing", None)
    );
}

#[test]
fn partial_or_invalid_responses_are_errors_instead_of_healthy_zeros() {
    let complete = format!("{MAC_HEALTH}{MAC_RUNNERS}");
    for output in [
        complete.replace("RFM_END\n", ""),
        complete.replace("RFM_RUNNERS\n", ""),
        complete.replace("23.50", "NaN"),
        complete.replace("23.50", "101"),
        complete.replace("8000000000", "25000000000"),
        complete.replace("1.5 2.0 0.8", "1.5"),
        complete.replace("macos", "windows"),
        complete.replace(MAC_RUNNERS, ""),
        complete.replace("mini-02", "other-runner"),
    ] {
        assert!(
            parse_inspection(&host("mac-mini-02"), &output).is_err(),
            "accepted {output:?}"
        );
    }
}

#[derive(Default)]
struct FakeCommands(Mutex<Vec<(Vec<String>, String)>>);

impl CommandRunner for FakeCommands {
    fn run(
        &self,
        command: &[String],
        input: Option<&str>,
        _: Duration,
        _: NonZeroExit,
    ) -> Result<CommandResult, CommandError> {
        self.0.lock().unwrap().push((command.to_vec(), input.unwrap().into()));
        Ok(CommandResult::ok(format!("{MAC_HEALTH}{MAC_RUNNERS}")))
    }
}

#[test]
fn one_ssh_session_delivers_health_and_runner_checks_on_stdin() {
    let fake = Arc::new(FakeCommands::default());
    let remote = RemoteClient::new(fake.clone(), Duration::from_secs(30), Programs::default());
    let host = host("mac-mini-02");
    let report = remote.inspect(&host).unwrap();
    assert!(report.runners["mini-02"].configured);
    let calls = fake.0.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let (argv, stdin) = &calls[0];
    assert_eq!(argv.last().unwrap(), "bash -s");
    assert!(argv.contains(&"StrictHostKeyChecking=yes".into()));
    assert!(argv.contains(&"ForwardAgent=no".into()));
    assert!(stdin.contains("/proc/meminfo"));
    assert!(stdin.contains("$HOME/gh-runners/acme-demo/runner-01") || stdin.contains("gh-runners/acme-demo/runner-01"));
}
