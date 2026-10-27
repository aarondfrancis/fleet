//! Pure view behavior: data freshness, source authority, discovery and runner joins.

use crate::derive::*;
use crate::model::*;
use chrono::{DateTime, Duration, TimeZone, Utc};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 3, 12, 0, 0).unwrap()
}

fn ago(s: i64) -> DateTime<Utc> {
    now() - Duration::seconds(s)
}

struct Fixture {
    config: AppConfig,
    obs: Observations,
    plan: crate::fleet_plan::FleetPlanInputs,
}

impl Fixture {
    fn new() -> Self {
        let mut f = Fixture {
            config: AppConfig {
                orgs: vec![OrgConfig {
                    login: "Acme".into(),
                    api_base: default_api_base(),
                    repo_scope: RepoScope::Recent { max_repos: 10 },
                    added_at: ago(9999),
                    auth: OrgAuth::GhCli,
                    kind: AccountKind::Organization,
                    runner_repos: Vec::new(),
                }],
                ..Default::default()
            },
            obs: Observations::default(),
            plan: crate::fleet_plan::FleetPlanInputs::default(),
        };
        f.obs.tailscale = Some(Timed::new(
            ago(5),
            TailnetSnapshot {
                self_node_id: Some("nSelf".into()),
                local_online: Some(true),
                tailnet_name: None,
                peers: vec![peer("nSelf", "my-mac", "macOS", true)],
            },
        ));
        f
    }

    fn gh_runners(&mut self, at: DateTime<Utc>, runners: Vec<GhRunner>) -> &mut Self {
        self.obs.github.entry("acme".into()).or_default().runners = Some(Timed::new(at, runners));
        self
    }

    fn jobs(&mut self, at: DateTime<Utc>, jobs: Vec<GhJob>) -> &mut Self {
        self.obs.github.entry("acme".into()).or_default().jobs = Some(Timed::new(
            at,
            JobScan {
                repos_scanned: vec!["web".into()],
                truncated: false,
                jobs,
            },
        ));
        self
    }

    fn view(&self) -> FleetView {
        derive(&self.config, &self.obs, &self.plan, now(), DataSource::Live)
    }
}

fn peer(id: &str, host: &str, os: &str, online: bool) -> TailnetPeer {
    TailnetPeer {
        node_id: id.into(),
        hostname: host.into(),
        dns_name: Some(format!("{host}.ts.net")),
        os: os.into(),
        ips: vec![],
        online,
        last_seen: (!online).then(|| ago(3600)),
        tags: vec![],
        ssh_host_keys: vec![],
    }
}

fn gh(id: u64, name: &str, status: &str, busy: bool, labels: &[&str]) -> GhRunner {
    GhRunner {
        id,
        name: name.into(),
        os: "macOS".into(),
        status: status.into(),
        busy,
        labels: labels.iter().map(ToString::to_string).collect(),
        ephemeral: None,
        version: None,
        runner_group_id: None,
    }
}

fn job(id: u64, runner: Option<u64>, status: &str, labels: &[&str], started: Option<DateTime<Utc>>) -> GhJob {
    GhJob {
        id,
        run_id: 1,
        repo: "acme/web".into(),
        name: format!("job-{id}"),
        workflow_name: Some("CI".into()),
        status: status.into(),
        labels: labels.iter().map(ToString::to_string).collect(),
        runner_id: runner,
        runner_name: None,
        created_at: Some(ago(600 + i64::try_from(id).unwrap())),
        started_at: started,
        html_url: None,
        head_branch: None,
    }
}

const MAC: &[&str] = &["self-hosted", "macOS", "ARM64"];

#[test]
fn queue_counts_label_compatible_runners() {
    let mut f = Fixture::new();
    f.gh_runners(
        ago(10),
        vec![
            gh(1, "mac-busy", "online", true, MAC),
            gh(2, "mac-idle", "online", false, MAC),
            gh(3, "mac-off", "offline", false, MAC),
            gh(4, "linux", "online", false, &["self-hosted", "Linux"]),
        ],
    )
    .jobs(
        ago(20),
        vec![
            job(10, None, "queued", &["self-hosted", "macos"], None),
            job(11, None, "queued", &["self-hosted", "gpu"], None),
            job(12, None, "queued", &["ubuntu-latest"], None),
            job(13, None, "waiting", &["self-hosted"], None),
            job(14, Some(1), "in_progress", MAC, Some(ago(10))),
        ],
    );
    let v = f.view();
    assert_eq!(v.queue.jobs.len(), 2);
    assert_eq!(v.queue.other_queued, 1);
    assert!(v.queue.complete);
    let mac_job = v.queue.jobs.iter().find(|q| q.job.id == 10).unwrap();
    assert_eq!(mac_job.compatible_runners.len(), 3);
    assert_eq!(mac_job.compatible_online, 2);
    assert_eq!(mac_job.compatible_idle, 1);
    let gpu_job = v.queue.jobs.iter().find(|q| q.job.id == 11).unwrap();
    assert!(gpu_job.compatible_runners.is_empty());
    // Oldest first.
    assert_eq!(v.queue.jobs[0].job.id, 11);
    // A self-hosted job no online runner can take needs attention.
    assert_eq!(v.summary.attention, 1);
}

#[test]
fn queue_incomplete_without_fresh_job_data() {
    let mut f = Fixture::new();
    f.gh_runners(ago(10), vec![]);
    assert!(!f.view().queue.complete);
    f.jobs(ago(3600), vec![]);
    assert!(!f.view().queue.complete);
    f.jobs(ago(10), vec![]);
    assert!(f.view().queue.complete);
}

const JOIN_INVENTORY: &str = r#"
[organization]
name = "acme"
url = "https://github.com/acme"
runner_group = "trusted"

[[hosts]]
id = "mini"
platform = "darwin-arm64"
ssh = "builder@mini.example.ts.net"
root = "~/gh-runners/acme"
service_mode = "launchagent"
labels = ["fleet", "macos-build", "host-mini"]
runners = [
  { name = "mini-01", directory = "runner-01", replaces = { name = "Old-Mini", directory = "~/actions-runner" } },
  { name = "mini-02", directory = "runner-02" },
  { name = "mini-03", directory = "runner-03", decommission = true },
]
"#;

fn with_inventory(f: &mut Fixture) {
    let inventory = crate::desired::inventory::parse_inventory(JOIN_INVENTORY, &Default::default()).unwrap();
    f.plan.inventory = Some(Ok(inventory));
    f.obs.tailscale.as_mut().unwrap().value.peers.push(TailnetPeer {
        dns_name: Some("mini.example.ts.net".into()),
        ..peer("nMini", "Mac Mini", "macOS", true)
    });
}

#[test]
fn inventory_and_github_runners_join_their_tailnet_machine() {
    let mut f = Fixture::new();
    with_inventory(&mut f);
    f.gh_runners(
        ago(10),
        vec![
            gh(1, "mini-01", "online", true, MAC),
            // Not declared, but it carries the host's label.
            gh(2, "stray", "online", false, &["self-hosted", "host-mini"]),
            gh(3, "depot-x", "online", false, &["self-hosted"]),
        ],
    );
    let v = f.view();
    assert_eq!(v.machines.len(), 1);
    let mini = &v.machines[0];
    assert_eq!((mini.id.as_str(), mini.display_name.as_str()), ("ts:nMini", "mini"));
    let runners: Vec<(&str, &str)> = mini
        .runners
        .iter()
        .map(|r| {
            let role = match r.desired.as_ref().map(|d| &d.role) {
                Some(crate::fleet_plan::DesiredRunnerRole::Declared) => "declared",
                Some(crate::fleet_plan::DesiredRunnerRole::Undeclared) => "undeclared",
                other => panic!("unexpected role {other:?}"),
            };
            (r.name.as_str(), role)
        })
        .collect();
    // mini-01 from GitHub; mini-02 declared but nowhere yet; the stray by label.
    // The retired legacy runner and the finished decommission aren't listed.
    assert_eq!(
        runners,
        [
            ("mini-01", "declared"),
            ("mini-02", "declared"),
            ("stray", "undeclared")
        ]
    );
    assert!(matches!(mini.runners[0].activity, RunnerActivity::Busy { .. }));
    assert_eq!(mini.runners[0].key, "gh:acme:1");
    assert_eq!(mini.runners[1].key, "inv:mini:mini-02");
    // Only the pool runner stays unlinked.
    let unmatched: Vec<&str> = v.unmatched_runners.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(unmatched, ["depot-x"]);
}

#[test]
fn legacy_and_decommissioned_runners_show_while_they_still_exist() {
    let mut f = Fixture::new();
    with_inventory(&mut f);
    f.gh_runners(
        ago(10),
        vec![
            gh(1, "Old-Mini", "online", false, MAC),
            gh(2, "mini-03", "offline", false, MAC),
        ],
    );
    let v = f.view();
    let names: Vec<&str> = v.machines[0].runners.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["mini-01", "mini-02", "mini-03", "Old-Mini"]);
}

#[test]
fn without_live_github_runners_carry_the_last_checks_github_state() {
    let mut f = Fixture::new();
    with_inventory(&mut f);
    let mut snapshot = crate::desired::model::FleetSnapshot::default();
    let r = crate::desired::model::GithubRunner::new(7, "mini-01", "online", true, &["fleet"], Some(true));
    snapshot.github.insert("mini-01".into(), r);
    f.plan.snapshot = Some(snapshot);
    f.plan.checked_at = Some(ago(600));
    let v = f.view();
    let mini_01 = &v.machines[0].runners[0];
    assert_eq!(mini_01.key, "gh:acme:7", "keyed as the live entry will be");
    let g = mini_01.github.as_ref().unwrap();
    assert_eq!((g.status.as_str(), g.busy, g.stale), ("online", true, true));
    // Stale facts never become a current state.
    assert_eq!(mini_01.activity, RunnerActivity::Unknown);
    // A fresh check counts as current.
    f.plan.checked_at = Some(ago(30));
    assert!(matches!(
        f.view().machines[0].runners[0].activity,
        RunnerActivity::Busy { .. }
    ));
}

fn demo() -> (AppConfig, Observations, crate::fleet_plan::FleetPlanInputs) {
    let (config, observations) = crate::demo::dataset(now());
    (config, observations, crate::demo_desired::plan_inputs(now()))
}

#[test]
fn failed_ssh_keeps_health_and_local_facts_with_their_original_age() {
    let (config, mut obs, plan) = demo();
    let before = derive(&config, &obs, &plan, now(), DataSource::Demo);
    let studio = before.machines.iter().find(|m| m.display_name == "studio").unwrap();
    let old_health = studio.health.clone().unwrap();
    obs.machines.get_mut("inv:studio").unwrap().last_error = Some(Timed::new(now(), "SSH timed out.".into()));
    let after = derive(&config, &obs, &plan, now(), DataSource::Demo);
    let studio = after.machines.iter().find(|m| m.display_name == "studio").unwrap();
    assert_eq!(
        studio.health,
        Some(HealthView {
            stale: true,
            ..old_health
        })
    );
    assert_eq!(
        studio.connection,
        MachineConnection::Stale {
            checked_at: ago(6),
            reason: "SSH timed out.".into()
        }
    );
    assert!(studio.runners.iter().all(|r| r.local.as_ref().unwrap().stale));
    assert_eq!(
        studio.desired.as_ref().unwrap().error.as_deref(),
        Some("SSH timed out.")
    );
    assert_eq!(studio.runners[0].installed, Tri::Unknown);
}

#[test]
fn a_service_running_does_not_establish_busy_or_idle_when_github_is_stale() {
    let (config, mut obs, mut plan) = demo();
    obs.github.get_mut("acme-demo").unwrap().runners.as_mut().unwrap().at = ago(600);
    plan.checked_at = Some(ago(600));
    let view = derive(&config, &obs, &plan, now(), DataSource::Demo);
    let studio = view.machines.iter().find(|m| m.display_name == "studio").unwrap();
    assert_eq!(studio.runners[0].activity, RunnerActivity::Unknown);
    assert_eq!(studio.runners[0].connected, Tri::Unknown);
    assert_eq!(studio.runners[0].installed, Tri::Yes);
}

#[test]
fn job_details_are_linked_by_github_and_long_jobs_are_not_called_stuck() {
    let (config, mut obs, plan) = demo();
    obs.github
        .get_mut("acme-demo")
        .unwrap()
        .jobs
        .as_mut()
        .unwrap()
        .value
        .jobs[0]
        .started_at = Some(ago(24 * 3600));
    let view = derive(&config, &obs, &plan, now(), DataSource::Demo);
    let runner = view
        .machines
        .iter()
        .flat_map(|m| &m.runners)
        .find(|r| r.name == "studio-1")
        .unwrap();
    assert_eq!(runner.current_job.as_ref().unwrap().id, 9001);
    assert_eq!(
        runner.activity,
        RunnerActivity::Busy {
            since: Some(ago(24 * 3600))
        }
    );
    assert!(runner.warnings.iter().all(|w| !w.contains("stuck")));
    obs.github.get_mut("acme-demo").unwrap().jobs = None;
    let view = derive(&config, &obs, &plan, now(), DataSource::Demo);
    let runner = view
        .machines
        .iter()
        .flat_map(|m| &m.runners)
        .find(|r| r.name == "studio-1")
        .unwrap();
    assert_eq!(runner.current_job, None);
    assert_eq!(runner.activity, RunnerActivity::Busy { since: None });
}

#[test]
fn changed_ssh_target_never_inherits_old_health() {
    let (config, obs, mut plan) = demo();
    let inv = plan.inventory.as_mut().unwrap().as_mut().unwrap();
    inv.hosts.iter_mut().find(|h| h.id == "studio").unwrap().ssh_target = Some("different-host".into());
    let view = derive(&config, &obs, &plan, now(), DataSource::Demo);
    let studio = view.machines.iter().find(|m| m.display_name == "studio").unwrap();
    assert_eq!(studio.health, None);
    assert_eq!(studio.diagnostics, None);
    assert_eq!(studio.connection, MachineConnection::Pending);
}

#[test]
fn discovery_and_demo_cover_current_stale_offline_and_failed_ssh() {
    let (config, obs, plan) = demo();
    let view = derive(&config, &obs, &plan, now(), DataSource::Demo);
    let kinds: Vec<_> = view.machines.iter().map(|m| &m.connection).collect();
    assert!(kinds.iter().any(|c| matches!(c, MachineConnection::Connected { .. })));
    assert!(kinds.iter().any(|c| matches!(c, MachineConnection::Stale { .. })));
    assert!(kinds.iter().any(|c| matches!(c, MachineConnection::Offline { .. })));
    assert!(kinds.iter().any(|c| matches!(c, MachineConnection::Error { .. })));
    assert_eq!(view.other_machines.len(), 2);
    assert_eq!(view.ignored.len(), 1);
    // The hosted pool runner and a repository runner.
    assert_eq!(view.unmatched_runners.len(), 2);
    assert_eq!(view.queue.jobs.len(), 2);
    assert!(view.machines.iter().all(|m| m.desired.is_some()));
    let names: Vec<_> = view.machines.iter().map(|m| m.display_name.as_str()).collect();
    assert_eq!(
        names,
        ["build-linux-01", "mac-mini-02", "mac-mini-03", "studio", "win-builder"]
    );
    assert!(!view.other_machines.iter().any(|m| m.display_name == "iphone"));
}

#[test]
fn a_newer_explicit_check_supersedes_old_poll_runner_facts_and_errors() {
    let (config, mut obs, mut plan) = demo();
    let observation = obs.machines.get_mut("inv:studio").unwrap();
    observation.last_error = Some(Timed::new(ago(5), "old failure".into()));
    plan.host_checked_at.insert("studio".into(), now());
    let runner = plan.snapshot.as_mut().unwrap().local.get_mut("studio-1").unwrap();
    runner.version = Some("2.999.0".into());
    let view = derive(&config, &obs, &plan, now(), DataSource::Demo);
    let studio = view.machines.iter().find(|m| m.display_name == "studio").unwrap();
    assert_eq!(studio.desired.as_ref().unwrap().error, None);
    let local = studio
        .runners
        .iter()
        .find(|r| r.name == "studio-1")
        .unwrap()
        .local
        .as_ref()
        .unwrap();
    assert_eq!(
        (local.version.as_deref(), local.as_of, local.stale),
        (Some("2.999.0"), now(), false)
    );
    // The explicit runner check doesn't refresh the separate health sample.
    assert!(studio.health.as_ref().unwrap().stale);
}

#[test]
fn changing_inventory_paths_invalidates_machine_facts() {
    let (config, obs, mut plan) = demo();
    let inv = plan.inventory.as_mut().unwrap().as_mut().unwrap();
    inv.hosts.iter_mut().find(|h| h.id == "studio").unwrap().root = "~/different-runners".into();
    plan.snapshot = None; // The engine drops its snapshot when inventory content changes.
    let view = derive(&config, &obs, &plan, now(), DataSource::Demo);
    let studio = view.machines.iter().find(|m| m.display_name == "studio").unwrap();
    assert_eq!(studio.connection, MachineConnection::Pending);
    assert!(studio.runners.iter().all(|r| r.local.is_none()));
}

#[test]
fn a_peer_label_survives_becoming_an_inventory_machine() {
    let (mut config, obs, plan) = demo();
    config.machine_preferences.insert(
        "ts:nMini".into(),
        MachinePreferences {
            display_name: Some("Office Mini".into()),
            description: Some("Under the desk".into()),
        },
    );
    let view = derive(&config, &obs, &plan, now(), DataSource::Live);
    let machine = view.machines.iter().find(|m| m.id == "ts:nMini").unwrap();
    assert_eq!(
        (machine.display_name.as_str(), machine.description.as_deref()),
        ("Office Mini", Some("Under the desk"))
    );
    assert_eq!(machine.desired.as_ref().unwrap().host.id, "mac-mini-02");
}

#[test]
fn conflicting_inventory_hosts_keep_stable_unique_identity_and_separate_runners() {
    let mut f = Fixture::new();
    let inventory_text = format!(
        "{JOIN_INVENTORY}{}",
        r#"
[[hosts]]
id = "second"
platform = "darwin-arm64"
ssh = "another-user@mini.example.ts.net"
root = "~/gh-runners/other"
service_mode = "launchagent"
labels = ["fleet", "macos-build", "host-second"]
runners = [{ name = "second-01", directory = "runner-01" }]
"#
    );
    // Both host declarations are individually valid inventory entries.
    let inventory = crate::desired::inventory::parse_inventory(&inventory_text, &Default::default()).unwrap();
    f.plan.inventory = Some(Ok(inventory));
    f.obs.tailscale.as_mut().unwrap().value.peers.push(TailnetPeer {
        dns_name: Some("mini.example.ts.net".into()),
        ..peer("nMini", "Mac Mini", "macOS", true)
    });
    f.gh_runners(
        ago(10),
        vec![
            gh(1, "mini-01", "online", false, MAC),
            gh(2, "second-01", "online", false, MAC),
            gh(3, "stray", "online", false, &["host-mini"]),
        ],
    );
    f.plan.snapshot = Some(crate::desired::model::FleetSnapshot::default());
    f.config.machine_preferences.insert(
        "ts:nMini".into(),
        MachinePreferences {
            display_name: Some("Shared peer label".into()),
            description: None,
        },
    );
    let view = f.view();
    let identities: Vec<_> = view
        .machines
        .iter()
        .map(|m| (m.id.as_str(), m.display_name.as_str()))
        .collect();
    assert_eq!(identities, [("inv:mini", "mini"), ("inv:second", "second")]);
    assert!(view.other_machines.is_empty());
    for machine in &view.machines {
        let MachineConnection::Error { detail } = &machine.connection else {
            panic!("conflict must be an error")
        };
        assert!(detail.contains("mini, second"));
        assert!(detail.contains("same machine or SSH destination"));
        assert_eq!(machine.desired.as_ref().unwrap().error.as_ref(), Some(detail));
        assert_eq!(machine.desired.as_ref().unwrap().pending_changes, 0);
        assert!(machine.desired.as_ref().unwrap().plan.iter().all(|p| p.blocked));
        assert_eq!(machine.health, None);
        assert_eq!(machine.tailnet, None);
        assert!(
            machine
                .runners
                .iter()
                .all(|r| r.machine_id.as_deref() == Some(machine.id.as_str()) && r.local.is_none())
        );
        assert!(
            machine
                .runners
                .iter()
                .flat_map(|r| &r.desired.as_ref().unwrap().plan)
                .all(|p| p.blocked)
        );
    }
    assert_eq!(
        view.machines[0]
            .runners
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>(),
        ["mini-01", "mini-02"]
    );
    assert_eq!(
        view.machines[1]
            .runners
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>(),
        ["second-01"]
    );
    assert_eq!(
        view.unmatched_runners
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>(),
        ["stray"]
    );
    f.plan.inventory.as_mut().unwrap().as_mut().unwrap().hosts.reverse();
    assert_eq!(f.view().machines, view.machines);
    // Conflicts remain actionable errors even when tailnet discovery is unavailable.
    f.obs.tailscale = None;
    let without_peers = f.view();
    assert_eq!(without_peers.machines, view.machines);
}

#[test]
fn separate_github_errors_remain_visible_and_cached_jobs_are_incomplete() {
    let mut f = Fixture::new();
    f.gh_runners(ago(10), vec![])
        .jobs(ago(10), vec![job(10, None, "queued", MAC, None)]);
    let org = f.obs.github.get_mut("acme").unwrap();
    org.error = Some(Timed::new(now(), "Runner polling failed.".into()));
    org.jobs_error = Some(Timed::new(now(), "Job polling failed.".into()));
    let view = f.view();
    assert_eq!(
        view.orgs[0].error.as_deref(),
        Some("Runners: Runner polling failed. Jobs: Job polling failed.")
    );
    assert!(!view.queue.complete);
    assert_eq!(view.queue.jobs.len(), 1, "retain the last good job scan");
    f.obs.github.get_mut("acme").unwrap().error = None;
    assert_eq!(f.view().orgs[0].error.as_deref(), Some("Job polling failed."));
    f.obs.github.get_mut("acme").unwrap().jobs_error = None;
    assert!(f.view().queue.complete);
}

#[test]
fn identity_conflicts_do_not_reuse_previously_checked_local_facts() {
    let (config, obs, mut plan) = demo();
    let before = derive(&config, &obs, &plan, now(), DataSource::Live);
    let studio = before.machines.iter().find(|m| m.display_name == "studio").unwrap();
    assert!(studio.health.is_some());
    assert!(studio.runners.iter().any(|r| r.local.is_some()));
    let inventory = plan.inventory.as_mut().unwrap().as_mut().unwrap();
    let mut duplicate = inventory.hosts.iter().find(|h| h.id == "studio").unwrap().clone();
    duplicate.id = "studio-duplicate".into();
    for runner in &mut duplicate.runners {
        runner.name = format!("duplicate-{}", runner.name);
        runner.replaces = None;
    }
    inventory.hosts.push(duplicate);
    let after = derive(&config, &obs, &plan, now(), DataSource::Live);
    let studio = after.machines.iter().find(|m| m.id == "inv:studio").unwrap();
    assert!(matches!(studio.connection, MachineConnection::Error { .. }));
    assert_eq!(studio.health, None);
    assert_eq!(studio.diagnostics, None);
    assert!(studio.runners.iter().all(|r| r.local.is_none()));
    assert!(studio.desired.as_ref().unwrap().plan.iter().all(|item| item.blocked));
}

#[test]
fn repository_runners_take_only_their_repository_jobs_and_show_version_drift() {
    let mut f = Fixture::new();
    f.config.orgs[0].runner_repos = vec!["web".into()];
    let mut behind = gh(5, "web-mac", "online", false, MAC);
    behind.version = Some("2.329.0".into());
    let mut other_repo = job(11, None, "queued", MAC, None);
    other_repo.repo = "Acme/api".into();
    f.gh_runners(ago(10), vec![])
        .jobs(ago(20), vec![job(10, None, "queued", MAC, None), other_repo]);
    f.obs.github.get_mut("acme").unwrap().repo_runners =
        [("Acme/web".into(), Timed::new(ago(10), vec![behind]))].into();
    f.obs.runner_releases = Some(Timed::new(
        ago(60),
        vec![crate::runner_version::RunnerRelease {
            version: "2.330.0".into(),
            published_at: now() - Duration::days(40),
        }],
    ));

    let v = f.view();
    let github = v.unmatched_runners[0].github.as_ref().unwrap();
    assert_eq!(github.repo.as_deref(), Some("Acme/web"));
    assert_eq!(
        github.update,
        Some(crate::runner_version::RunnerUpdate {
            latest: "2.330.0".into(),
            missing_since: now() - Duration::days(40),
            overdue: true,
        })
    );
    let compatible: Vec<(u64, Vec<String>)> = v
        .queue
        .jobs
        .iter()
        .map(|q| (q.job.id, q.compatible_runners.clone()))
        .collect();
    assert_eq!(compatible, [(11, vec![]), (10, vec!["gh:acme:5".to_string()])]);
    let update = v.issues.iter().find(|i| i.id == "gh:acme:5:update").unwrap();
    assert_eq!(update.title, "web-mac: runner update overdue");
}

#[test]
fn repository_runners_never_join_inventory_hosts() {
    let mut f = Fixture::new();
    with_inventory(&mut f);
    f.gh_runners(ago(10), vec![]);
    let same_name = gh(1, "mini-01", "online", false, MAC);
    let host_label = gh(2, "stray", "online", false, &["self-hosted", "host-mini"]);
    f.obs.github.get_mut("acme").unwrap().repo_runners =
        [("Acme/web".into(), Timed::new(ago(10), vec![same_name, host_label]))].into();
    let v = f.view();
    let unmatched: Vec<&str> = v.unmatched_runners.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(unmatched, ["mini-01", "stray"]);
    assert!(v.machines[0].runners.iter().all(|r| r.github.is_none()));
}
