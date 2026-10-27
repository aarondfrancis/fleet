//! Pure derivation: (config, observations, plan inputs, now, source) -> FleetView.
//!
//! Rules the UI relies on:
//! - Ordering is by name only, never by status, so rows don't jump.
//! - Old data is kept and flagged `stale`; it is never replaced by zeros.
//! - "Installed" comes from SSH; "connected" and "doing work" come from GitHub.
//! - A current job is shown only when GitHub's jobs API links it to the runner.
//! - Elapsed time alone never marks anything as stuck.

use std::collections::{HashMap, HashSet};

use crate::desired::model::LocalRunner;
use chrono::{DateTime, Duration, Utc};

use crate::fleet_plan::{FleetPlanInputs, FleetPlanStatus};
use crate::labels;
use crate::model::*;

#[path = "derive_machines.rs"]
mod machines;

pub const SSH_STALE_AFTER: Duration = Duration::seconds(150);
pub const GITHUB_STALE_AFTER: Duration = Duration::seconds(150);
pub const JOBS_STALE_AFTER: Duration = Duration::seconds(300);

pub fn derive(
    config: &AppConfig,
    obs: &Observations,
    plan: &FleetPlanInputs,
    now: DateTime<Utc>,
    source: DataSource,
) -> FleetView {
    let mut view = derive_without_issues(config, obs, plan, now, source);
    crate::issues::attach(&mut view);
    view
}

/// [`derive`] without attention findings, for callers that attach live monitoring first.
pub(crate) fn derive_without_issues(
    config: &AppConfig,
    obs: &Observations,
    plan: &FleetPlanInputs,
    now: DateTime<Utc>,
    source: DataSource,
) -> FleetView {
    let plan = observed_plan(plan, obs, now);
    let plan = &plan;
    let checked = machines::Checked::build(plan);
    let gh = GithubIndex::build(config, obs, now);
    let mut matched: HashSet<String> = HashSet::new();
    let machines::Machines { fleet, other } =
        machines::assemble(config, obs, plan, checked.as_ref(), &gh, &mut matched, now);

    let mut unmatched_runners: Vec<RunnerView> = gh
        .runners
        .iter()
        .filter(|r| !matched.contains(&r.key))
        .map(|r| runner_view(r.key.clone(), None, Some(r), None, &gh))
        .collect();
    unmatched_runners.sort_by_key(|r| (r.org.clone(), r.name.to_lowercase(), r.key.clone()));

    let ignored = ignored_peers(config, obs);
    let all_runners: Vec<&RunnerView> = fleet
        .iter()
        .flat_map(|m| m.runners.iter())
        .chain(unmatched_runners.iter())
        .collect();
    let queue = queue_view(config, obs, &gh, &all_runners, now);
    let summary = summarize(&fleet, &all_runners, &queue);

    let mut view = FleetView {
        generated_at: now,
        source,
        summary,
        issues: Vec::new(),
        machines: fleet,
        other_machines: other,
        unmatched_runners,
        ignored,
        queue,
        orgs: org_statuses(config, obs),
        tailnet: TailnetStatusView {
            local_online: obs.tailscale.as_ref().and_then(|t| t.value.local_online),
            available: obs.tailscale.is_some(),
            tailnet_name: obs.tailscale.as_ref().and_then(|t| t.value.tailnet_name.clone()),
            as_of: obs.tailscale.as_ref().map(|t| t.at),
            error: obs.tailscale_error.as_ref().map(|e| e.value.clone()),
        },
        plan: plan_status(plan, checked.as_ref()),
        monitoring: crate::monitoring::FleetMonitoring::default(),
        activity: crate::job_history::FleetActivity::default(),
    };
    if source == DataSource::Demo {
        crate::monitoring::demo::attach(&mut view, now);
        crate::job_history::demo::attach(&mut view, now);
    }
    view
}

/// The latest SSH reads also inform drift, without changing stored reviews.
fn observed_plan(plan: &FleetPlanInputs, obs: &Observations, now: DateTime<Utc>) -> FleetPlanInputs {
    let mut plan = plan.clone();
    if let (Some(Ok(inventory)), Some(snapshot)) = (&plan.inventory, &mut plan.snapshot) {
        for host in &inventory.hosts {
            let Some(observation) = obs
                .machines
                .get(&inventory_machine_key(&host.id))
                .filter(|o| o.target.host() == Some(host))
            else {
                continue;
            };
            if let Some(report) = &observation.report
                && plan.host_checked_at.get(&host.id).is_none_or(|at| report.at >= *at)
            {
                for name in host.runner_names() {
                    snapshot.local.remove(name);
                }
                snapshot.local.extend(report.value.runners.clone());
                snapshot.host_errors.remove(&host.id);
                plan.host_checked_at.insert(host.id.clone(), report.at);
            }
            if let Some(error) = &observation.last_error
                && plan.host_checked_at.get(&host.id).is_none_or(|at| error.at >= *at)
            {
                snapshot.host_errors.insert(host.id.clone(), error.value.clone());
            } else if observation.report.as_ref().is_some_and(|r| {
                now - r.at > SSH_STALE_AFTER && plan.host_checked_at.get(&host.id).is_none_or(|at| r.at >= *at)
            }) {
                snapshot
                    .host_errors
                    .insert(host.id.clone(), "No recent SSH check.".into());
            }
        }
    }
    plan
}

/// Fleet-wide desired-state status for the UI.
fn plan_status(plan: &FleetPlanInputs, checked: Option<&machines::Checked>) -> FleetPlanStatus {
    let inventory = plan.inventory.as_ref();
    let snapshot = plan.snapshot.as_ref();
    let control = checked
        .map(|c| c.plan.iter().filter(|i| i.host == "control").cloned().collect())
        .unwrap_or_default();
    FleetPlanStatus {
        inventory_path: plan.inventory_path.clone(),
        inventory_error: inventory.and_then(|r| r.as_ref().err().cloned()),
        organization: inventory.and_then(|r| r.as_ref().ok()).map(|i| i.organization.clone()),
        github_error: snapshot.and_then(|s| s.github_error.clone()),
        policy_errors: snapshot.map(|s| s.policy_errors.clone()).unwrap_or_default(),
        control,
        checked_at: plan.checked_at,
        refreshing: plan.refreshing,
        control_checks: plan
            .doctor
            .as_ref()
            .map(|d| d.checks.iter().filter(|c| c.host == "control").cloned().collect())
            .unwrap_or_default(),
        checks_at: plan.doctor.as_ref().map(|d| d.checked_at),
        checks_error: plan.doctor.as_ref().and_then(|d| d.error.clone()),
        doctor_running: plan.doctor_running,
        operation: plan.operation.clone(),
    }
}

struct GhEntry {
    key: String,
    org: String,
    runner: GhRunner,
    at: DateTime<Utc>,
    stale: bool,
}

struct GithubIndex {
    runners: Vec<GhEntry>,
    /// (org, lowercase name) -> index
    by_name: HashMap<(String, String), usize>,
    /// (org, runner id) -> in-progress job
    jobs_by_runner: HashMap<(String, u64), GhJob>,
}

impl GithubIndex {
    fn build(config: &AppConfig, obs: &Observations, now: DateTime<Utc>) -> Self {
        let mut idx = GithubIndex {
            runners: Vec::new(),
            by_name: HashMap::new(),
            jobs_by_runner: HashMap::new(),
        };
        for org_cfg in &config.orgs {
            let org = org_cfg.login.to_lowercase();
            let Some(o) = obs.github.get(&org) else { continue };
            if let Some(runners) = &o.runners {
                let stale = now - runners.at > GITHUB_STALE_AFTER;
                for r in &runners.value {
                    let i = idx.runners.len();
                    idx.by_name.insert((org.clone(), r.name.to_lowercase()), i);
                    idx.runners.push(GhEntry {
                        key: format!("gh:{org}:{}", r.id),
                        org: org.clone(),
                        runner: r.clone(),
                        at: runners.at,
                        stale,
                    });
                }
            }
            if let Some(jobs) = &o.jobs
                && now - jobs.at <= JOBS_STALE_AFTER
            {
                for job in jobs.value.jobs.iter().filter(|j| j.status == "in_progress") {
                    if let Some(rid) = job.runner_id.filter(|id| *id != 0) {
                        idx.jobs_by_runner.insert((org.clone(), rid), job.clone());
                    }
                }
            }
        }
        idx
    }

    fn find(&self, org: &str, name: &str) -> Option<&GhEntry> {
        self.by_name
            .get(&(org.to_string(), name.to_lowercase()))
            .map(|&i| &self.runners[i])
    }
}

fn connection(
    peer: Option<&TailnetPeer>,
    observation: Option<&MachineObservation>,
    now: DateTime<Utc>,
) -> MachineConnection {
    if let Some(peer) = peer.filter(|p| !p.online) {
        return MachineConnection::Offline {
            last_seen: peer.last_seen,
        };
    }
    let Some(observation) = observation else {
        return MachineConnection::Pending;
    };
    let report = observation.report.as_ref();
    let error = observation
        .last_error
        .as_ref()
        .filter(|e| report.is_none_or(|r| e.at >= r.at));
    match (report, error) {
        (Some(report), None) if now - report.at <= SSH_STALE_AFTER => {
            MachineConnection::Connected { checked_at: report.at }
        }
        (Some(report), error) => MachineConnection::Stale {
            checked_at: report.at,
            reason: error.map_or_else(|| "No recent SSH check.".into(), |e| e.value.clone()),
        },
        (None, Some(error)) => MachineConnection::Error {
            detail: error.value.clone(),
        },
        (None, None) => MachineConnection::Pending,
    }
}

fn runner_view(
    key: String,
    machine_id: Option<&str>,
    gh_entry: Option<&GhEntry>,
    local: Option<(&LocalRunner, DateTime<Utc>, bool)>,
    gh: &GithubIndex,
) -> RunnerView {
    let org = gh_entry.map(|e| e.org.clone());

    let github = gh_entry.map(|e| GithubRunnerFacts {
        id: e.runner.id,
        status: e.runner.status.clone(),
        busy: e.runner.busy,
        labels: e.runner.labels.clone(),
        os: e.runner.os.clone(),
        version: e.runner.version.clone(),
        ephemeral: e.runner.ephemeral,
        as_of: e.at,
        stale: e.stale,
    });
    let local_facts = local.map(|(l, at, stale)| LocalRunnerFacts {
        install_dir: l.path.clone(),
        configured: l.configured,
        service_state: l.service_state.clone(),
        version: l.version.clone(),
        as_of: at,
        stale,
    });

    let current_job = gh_entry
        .and_then(|e| gh.jobs_by_runner.get(&(e.org.clone(), e.runner.id)))
        .map(job_view);

    let gh_fresh = github.as_ref().filter(|g| !g.stale);
    let local_fresh = local_facts.as_ref().filter(|l| !l.stale);
    let mut warnings = Vec::new();

    let activity = match gh_fresh {
        Some(g) if g.status != "online" => RunnerActivity::Offline,
        Some(g) if g.busy => RunnerActivity::Busy {
            since: current_job.as_ref().and_then(|j| j.started_at),
        },
        Some(_) => RunnerActivity::Idle,
        None => RunnerActivity::Unknown,
    };
    if let Some(l) = local_fresh {
        if !l.configured {
            warnings.push("The runner is not configured at its inventory path (no .runner file).".into());
        } else if !crate::desired::model::is_active(&l.service_state) {
            warnings.push(format!("Runner service is {} on the machine.", l.service_state));
        }
    }
    let name = local
        .map(|(l, _, _)| l.name.clone())
        .or_else(|| gh_entry.map(|e| e.runner.name.clone()))
        .unwrap_or_else(|| key.clone());

    RunnerView {
        key,
        name,
        org,
        machine_id: machine_id.map(str::to_string),
        installed: match local_fresh {
            Some(l) if l.configured => Tri::Yes,
            Some(_) => Tri::No,
            None => Tri::Unknown,
        },
        connected: match gh_fresh {
            Some(g) if g.status == "online" => Tri::Yes,
            Some(_) => Tri::No,
            None => Tri::Unknown,
        },
        activity,
        github,
        local: local_facts,
        current_job,
        warnings,
        desired: None,
    }
}

fn job_view(j: &GhJob) -> JobView {
    JobView {
        id: j.id,
        name: j.name.clone(),
        workflow_name: j.workflow_name.clone(),
        repo: j.repo.clone(),
        branch: j.head_branch.clone(),
        started_at: j.started_at,
        html_url: j.html_url.clone(),
        labels: j.labels.clone(),
    }
}

/// Tailnet machines the user chose to ignore, with their current tailnet facts.
fn ignored_peers(config: &AppConfig, obs: &Observations) -> Vec<DiscoveredPeerView> {
    let peers = obs
        .tailscale
        .as_ref()
        .map(|t| t.value.peers.as_slice())
        .unwrap_or_default();
    let mut ignored: Vec<DiscoveredPeerView> = config
        .ignored_peers
        .iter()
        .map(|i| match peers.iter().find(|p| p.node_id == i.tailscale_node_id) {
            Some(p) => DiscoveredPeerView {
                peer: p.clone(),
                ignored_at: Some(i.ignored_at),
            },
            None => placeholder_ignored(i),
        })
        .collect();
    ignored.sort_by_key(|d| (d.peer.hostname.to_lowercase(), d.peer.node_id.clone()));
    ignored
}

fn placeholder_ignored(i: &IgnoredPeer) -> DiscoveredPeerView {
    DiscoveredPeerView {
        peer: TailnetPeer {
            node_id: i.tailscale_node_id.clone(),
            hostname: i.name.clone(),
            dns_name: None,
            os: String::new(),
            ips: Vec::new(),
            online: false,
            last_seen: None,
            tags: Vec::new(),
            ssh_host_keys: Vec::new(),
        },
        ignored_at: Some(i.ignored_at),
    }
}

fn queue_view(
    config: &AppConfig,
    obs: &Observations,
    gh: &GithubIndex,
    runners: &[&RunnerView],
    now: DateTime<Utc>,
) -> QueueView {
    let mut view = QueueView {
        complete: true,
        ..Default::default()
    };
    let by_key: HashMap<&str, &RunnerView> = runners.iter().map(|r| (r.key.as_str(), *r)).collect();

    for org_cfg in &config.orgs {
        if matches!(org_cfg.repo_scope, RepoScope::None) {
            view.complete = false;
            continue;
        }
        let org = org_cfg.login.to_lowercase();
        if obs.github.get(&org).is_some_and(|o| o.jobs_error.is_some()) {
            view.complete = false;
        }
        let Some(jobs) = obs.github.get(&org).and_then(|o| o.jobs.as_ref()) else {
            view.complete = false;
            continue;
        };
        if now - jobs.at > JOBS_STALE_AFTER || jobs.value.truncated {
            view.complete = false;
        }
        view.as_of = Some(view.as_of.map_or(jobs.at, |a: DateTime<Utc>| a.min(jobs.at)));

        for job in jobs.value.jobs.iter().filter(|j| j.status == "queued") {
            let compatible: Vec<&GhEntry> = gh
                .runners
                .iter()
                .filter(|r| r.org == org && labels::is_compatible(&job.labels, &r.runner.labels))
                .collect();
            if compatible.is_empty() && !labels::targets_self_hosted(&job.labels) {
                view.other_queued += 1;
                continue;
            }
            let online: Vec<&&GhEntry> = compatible
                .iter()
                .filter(|r| !r.stale && r.runner.status == "online")
                .collect();
            let idle = online
                .iter()
                .filter(|r| {
                    !r.runner.busy
                        && by_key
                            .get(r.key.as_str())
                            .is_none_or(|v| !matches!(v.activity, RunnerActivity::Busy { .. }))
                })
                .count();
            view.jobs.push(QueuedJobView {
                org: org.clone(),
                job: job_view(job),
                created_at: job.created_at,
                compatible_runners: compatible.iter().map(|r| r.key.clone()).collect(),
                compatible_online: count_u32(online.len()),
                compatible_idle: count_u32(idle),
            });
        }
    }
    view.jobs.sort_by_key(|q| (q.created_at, q.job.id));
    view
}

fn summarize(machines: &[MachineView], runners: &[&RunnerView], queue: &QueueView) -> FleetSummary {
    let mut s = FleetSummary {
        machines: count_u32(machines.len()),
        runners: count_u32(runners.len()),
        queued_jobs: count_u32(queue.jobs.len()),
        ..Default::default()
    };
    s.machines_connected = count_u32(
        machines
            .iter()
            .filter(|m| matches!(m.connection, MachineConnection::Connected { .. }))
            .count(),
    );
    for r in runners {
        match r.activity {
            RunnerActivity::Busy { .. } => {
                s.runners_busy += 1;
                if r.connected == Tri::Yes {
                    s.runners_online += 1;
                }
            }
            RunnerActivity::Idle => {
                if r.connected == Tri::Yes {
                    s.runners_online += 1;
                }
            }
            RunnerActivity::Offline => s.runners_offline += 1,
            RunnerActivity::Unknown => s.runners_unknown += 1,
        }
    }
    s
}

fn org_statuses(config: &AppConfig, obs: &Observations) -> Vec<OrgStatusView> {
    let mut orgs: Vec<OrgStatusView> = config
        .orgs
        .iter()
        .map(|cfg| {
            let o = obs.github.get(&cfg.login.to_lowercase());
            OrgStatusView {
                login: cfg.login.clone(),
                has_token: o.is_none_or(|o| !o.missing_token),
                runners_as_of: o.and_then(|o| o.runners.as_ref()).map(|r| r.at),
                jobs_as_of: o.and_then(|o| o.jobs.as_ref()).map(|j| j.at),
                repos_scanned: o
                    .and_then(|o| o.jobs.as_ref())
                    .map(|j| count_u32(j.value.repos_scanned.len()))
                    .unwrap_or(0),
                jobs_truncated: o.and_then(|o| o.jobs.as_ref()).is_some_and(|j| j.value.truncated),
                error: o.and_then(|o| match (&o.error, &o.jobs_error) {
                    (Some(runners), Some(jobs)) => Some(format!("Runners: {} Jobs: {}", runners.value, jobs.value)),
                    (Some(error), None) | (None, Some(error)) => Some(error.value.clone()),
                    (None, None) => None,
                }),
                rate_limit: o.and_then(|o| o.rate_limit.clone()),
                repo_scope: cfg.repo_scope.clone(),
                auth: cfg.auth,
            }
        })
        .collect();
    orgs.sort_by_key(|o| o.login.to_lowercase());
    orgs
}
