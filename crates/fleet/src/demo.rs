//! Demo dataset. Built as raw config + observations and passed through the
//! same `derive` as live data, so demo mode exercises the real pipeline.
//! Everything here is fictional (`acme-demo`, `*.demo.ts.net`); the view
//! is marked `DataSource::Demo` and the UI labels it as demo data.

use crate::desired::model::LocalRunner;
use crate::machine::{DiskStats, MachineReport, MachineTarget, OsFamily};
use chrono::{DateTime, Duration, Utc};

use crate::model::*;

const ORG: &str = "acme-demo";

pub fn dataset(now: DateTime<Utc>) -> (AppConfig, Observations) {
    let ago = |s: i64| now - Duration::seconds(s);
    let added = now - Duration::days(12);

    let config = AppConfig {
        desktop_notifications: false,
        orgs: vec![OrgConfig {
            login: ORG.into(),
            api_base: default_api_base(),
            repo_scope: RepoScope::Recent { max_repos: 20 },
            added_at: added,
            auth: OrgAuth::GhCli,
        }],
        ignored_peers: vec![IgnoredPeer {
            tailscale_node_id: "nNas".into(),
            name: "home-nas".into(),
            ignored_at: now - Duration::days(3),
        }],
        demo_mode: true,
        local_api: true,
        inventory_path: None,
        machine_preferences: Default::default(),
    };

    let peers = vec![
        peer("nStudio", "studio", "macOS", true, None),
        peer("nMini", "mac-mini-02", "macOS", true, None),
        peer("nLinux", "build-linux-01", "linux", true, None),
        peer("nWin", "win-builder", "windows", false, Some(ago(3 * 3600))),
        peer("nNew", "mac-mini-03", "macOS", true, None),
        peer("nSpare", "spare-ubuntu", "linux", true, None),
        peer("nLaptop", "dev-laptop", "macOS", true, None),
        peer("nNas", "home-nas", "linux", true, None),
        peer("nPhone", "iphone", "iOS", true, None),
    ];

    let gh_runners = vec![
        gh(101, "studio-1", true, &["self-hosted", "macOS", "ARM64", "xcode-16"]),
        gh(102, "studio-2", false, &["self-hosted", "macOS", "ARM64", "xcode-16"]),
        gh(201, "mini-02", true, &["self-hosted", "macOS", "ARM64"]),
        gh(301, "linux-01-a", false, &["self-hosted", "Linux", "X64", "docker"]),
        gh(302, "linux-01-b", false, &["self-hosted", "Linux", "X64", "docker"]),
        offline(gh(401, "win-builder", false, &["self-hosted", "Windows", "X64"])),
        gh(501, "cloud-gpu-1", false, &["self-hosted", "Linux", "X64", "gpu"]),
    ];

    let jobs = vec![
        job(
            9001,
            101,
            "build (macOS)",
            "Release",
            "acme-demo/ios-app",
            "in_progress",
            &["self-hosted", "macOS", "xcode-16"],
            Some(ago(47 * 60 + 12)),
            ago(48 * 60),
        ),
        job(
            9002,
            201,
            "test-unit",
            "CI",
            "acme-demo/web",
            "in_progress",
            &["self-hosted", "macOS"],
            Some(ago(3 * 60 + 5)),
            ago(4 * 60),
        ),
        job(
            9003,
            0,
            "ui-tests",
            "CI",
            "acme-demo/ios-app",
            "queued",
            &["self-hosted", "macOS", "xcode-16"],
            None,
            ago(2 * 60 + 30),
        ),
        job(
            9004,
            0,
            "train-small",
            "Nightly",
            "acme-demo/ml",
            "queued",
            &["self-hosted", "gpu", "a100"],
            None,
            ago(26 * 60),
        ),
        job(
            9005,
            0,
            "lint",
            "CI",
            "acme-demo/web",
            "queued",
            &["ubuntu-latest"],
            None,
            ago(20),
        ),
    ];

    let mut obs = Observations {
        tailscale: Some(Timed::new(
            ago(4),
            TailnetSnapshot {
                self_node_id: Some("nThisMac".into()),
                local_online: Some(true),
                tailnet_name: Some("demo.ts.net".into()),
                peers,
            },
        )),
        ..Default::default()
    };
    obs.github.insert(
        ORG.into(),
        OrgObservation {
            runners: Some(Timed::new(ago(9), gh_runners)),
            jobs: Some(Timed::new(
                ago(21),
                JobScan {
                    repos_scanned: vec!["ios-app".into(), "web".into(), "ml".into(), "infra".into()],
                    truncated: false,
                    jobs,
                },
            )),
            error: None,
            jobs_error: None,
            rate_limit: Some(RateLimit {
                limit: 5000,
                remaining: 4712,
                resets_at: Some(now + Duration::minutes(38)),
            }),
            missing_token: false,
        },
    );

    let inventory = crate::demo_desired::inventory();
    let snapshot = crate::demo_desired::snapshot();
    for (id, os, cpu, memory, disk, age, error) in [
        ("studio", OsFamily::Macos, 38.5, (64, 21), (994, 312), 6, None),
        ("mac-mini-02", OsFamily::Macos, 71.0, (16, 3), (245, 14), 4, None),
        (
            "build-linux-01",
            OsFamily::Linux,
            12.0,
            (128, 96),
            (1800, 1250),
            360,
            Some("SSH connection timed out."),
        ),
        (
            "win-builder",
            OsFamily::Windows,
            4.0,
            (32, 22),
            (512, 140),
            10800,
            Some("Host unreachable."),
        ),
    ] {
        let host = inventory.find_host(id).expect("demo host exists");
        let mut runners = std::collections::BTreeMap::new();
        for r in &host.runners {
            if let Some(local) = snapshot.local.get(&r.name) {
                runners.insert(r.name.clone(), local.clone());
            } else if id == "win-builder" {
                runners.insert(
                    r.name.clone(),
                    LocalRunner::new(&r.name, &host.runner_path(r), true, "running", Some("2.329.0")),
                );
            }
            if let Some(old) = &r.replaces
                && let Some(local) = snapshot.local.get(&old.name)
            {
                runners.insert(old.name.clone(), local.clone());
            }
        }
        obs.machines.insert(
            inventory_machine_key(id),
            MachineObservation {
                target: MachineTarget::Inventory(host.clone()),
                report: Some(Timed::new(ago(age), report(os, cpu, memory, disk, runners))),
                last_error: error.map(|message| Timed::new(ago(5), message.into())),
            },
        );
    }
    obs.machines.insert(
        inventory_machine_key("mac-mini-03"),
        MachineObservation {
            target: MachineTarget::Inventory(inventory.find_host("mac-mini-03").expect("demo host").clone()),
            report: None,
            last_error: Some(Timed::new(
                ago(5),
                "SSH permission denied. Check the host's SSH login and tailnet access.".into(),
            )),
        },
    );
    (config, obs)
}

fn peer(id: &str, host: &str, os: &str, online: bool, last_seen: Option<DateTime<Utc>>) -> TailnetPeer {
    TailnetPeer {
        node_id: id.into(),
        hostname: host.into(),
        dns_name: Some(format!("{host}.demo.ts.net")),
        os: os.into(),
        ips: vec!["100.64.0.0".into()],
        online,
        last_seen,
        tags: vec![],
        ssh_host_keys: if os == "linux" {
            vec!["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDemoHostKeyForPreviewOnly".into()]
        } else {
            vec![]
        },
    }
}

fn gh(id: u64, name: &str, busy: bool, labels: &[&str]) -> GhRunner {
    GhRunner {
        id,
        name: name.into(),
        os: if labels.contains(&"macOS") {
            "macOS".into()
        } else if labels.contains(&"Windows") {
            "Windows".into()
        } else {
            "Linux".into()
        },
        status: "online".into(),
        busy,
        labels: labels.iter().map(ToString::to_string).collect(),
        ephemeral: Some(false),
        version: Some("2.329.0".into()),
        runner_group_id: Some(1),
    }
}

fn offline(mut r: GhRunner) -> GhRunner {
    r.status = "offline".into();
    r
}

#[expect(
    clippy::too_many_arguments,
    reason = "demo fixture builder; a struct would only rename the arguments"
)]
fn job(
    id: u64,
    runner: u64,
    name: &str,
    workflow: &str,
    repo: &str,
    status: &str,
    labels: &[&str],
    started: Option<DateTime<Utc>>,
    created: DateTime<Utc>,
) -> GhJob {
    GhJob {
        id,
        run_id: id * 10,
        repo: repo.into(),
        name: name.into(),
        workflow_name: Some(workflow.into()),
        status: status.into(),
        labels: labels.iter().map(ToString::to_string).collect(),
        runner_id: (runner != 0).then_some(runner),
        runner_name: None,
        created_at: Some(created),
        started_at: started,
        completed_at: None,
        conclusion: None,
        html_url: Some(format!("https://github.com/{repo}/actions/runs/{}/job/{id}", id * 10)),
        head_branch: Some("main".into()),
    }
}

fn report(
    os: OsFamily,
    cpu: f32,
    mem_gb: (u64, u64),
    disk_gb: (u64, u64),
    runners: std::collections::BTreeMap<String, LocalRunner>,
) -> MachineReport {
    const GB: u64 = 1_000_000_000;
    MachineReport {
        os,
        os_version: Some(match os {
            OsFamily::Macos => "macOS 15.6".into(),
            OsFamily::Linux => "Ubuntu 24.04".into(),
            OsFamily::Windows => "Windows Server 2022".into(),
            OsFamily::Other => "Unknown".into(),
        }),
        uptime_secs: Some(9 * 86400 + 4 * 3600),
        boot_id: Some("demo-boot-session".into()),
        cpu_percent: Some(cpu),
        logical_cores: Some(12),
        load_average: Some([f64::from(cpu) / 10.0, f64::from(cpu) / 12.0, f64::from(cpu) / 14.0]),
        memory_total_bytes: Some(mem_gb.0 * GB),
        memory_available_bytes: Some(mem_gb.1 * GB),
        disks: vec![DiskStats {
            mount_point: if os == OsFamily::Windows {
                "C:\\".into()
            } else {
                "/".into()
            },
            total_bytes: disk_gb.0 * GB,
            available_bytes: disk_gb.1 * GB,
        }],
        runners,
        warnings: vec![],
        diagnostics: [
            ("identity", "ci"),
            ("architecture", "arm64"),
            (
                "tailscale_cli",
                if os == OsFamily::Windows {
                    "C:\\Program Files\\Tailscale\\tailscale.exe"
                } else {
                    "/usr/local/bin/tailscale"
                },
            ),
            (
                "tailscale_install",
                if os == OsFamily::Windows {
                    "windows"
                } else {
                    "cli-daemon"
                },
            ),
            ("tailscale_version", "1.90.0"),
            ("tailscale_backend", "Running"),
            ("tailscale_ssh", if os == OsFamily::Windows { "false" } else { "true" }),
            ("tailscale_unattended", "true"),
            (
                "tailscale_startup",
                if os == OsFamily::Windows { "Auto" } else { "enabled" },
            ),
            ("ac_sleep", "0"),
            ("ac_displaysleep", "10"),
            ("ac_autorestart", "1"),
            ("ac_womp", "1"),
            ("console_user", "ci"),
            ("target_sleep", "masked"),
            ("idle_action", "ignore"),
            ("ac_hibernate", "0"),
            ("ac_lid", "0"),
            ("ac_standby_network", "2"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect(),
    }
}
