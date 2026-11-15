//! The bounded local journal: finished jobs, runs still being followed, and recorded time.
//!
//! Writes only this dashboard's own file, atomically; a malformed or newer-format file is
//! preserved and new jobs are kept in memory only. New jobs and spans are saved after the
//! round that found them; other changes (longer spans, followed runs) at five-minute
//! checkpoints, so quitting can lose up to five minutes of them.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::{FleetFinishedJob, is_self_hosted};
use crate::model::{FleetView, GhJob, GhRunner, JobScan};

const VERSION: u32 = 1;
pub(crate) const RETENTION: Duration = Duration::days(30);
pub(crate) const MAX_JOBS: usize = 5_000;
const MAX_SPANS: usize = 2_000;
const MAX_PENDING_RUNS: usize = 500;
/// A run unseen for this long (deleted, or waiting on an approval) is no longer followed.
const PENDING_RETENTION: Duration = Duration::days(1);
/// A run read back unfinished is read again after this long.
const RECHECK_AFTER: Duration = Duration::minutes(5);
/// Recording continues the previous span across gaps up to this long (a few job polls).
const SPAN_GAP: Duration = Duration::minutes(3);
/// Changes other than new jobs and spans are saved at least this often.
const CHECKPOINT: Duration = Duration::minutes(5);

/// Time the app was recording: running, with every organization's job scan succeeding.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct Span {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PendingRun {
    /// Organization login, lowercase.
    org: String,
    /// `owner/name`
    repo: String,
    last_seen: DateTime<Utc>,
    checked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Journal {
    version: u32,
    /// Oldest first by completion time.
    #[serde(default)]
    jobs: Vec<FleetFinishedJob>,
    #[serde(default)]
    pending: BTreeMap<u64, PendingRun>,
    /// Sorted and non-overlapping.
    #[serde(default)]
    recorded: Vec<Span>,
}

impl Default for Journal {
    fn default() -> Self {
        Self {
            version: VERSION,
            jobs: Vec::new(),
            pending: BTreeMap::new(),
            recorded: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct JobHistory {
    path: PathBuf,
    journal: Journal,
    storage_error: Option<String>,
    writable: bool,
    /// New jobs or spans: save after this round.
    urgent: bool,
    /// When the oldest unsaved change was made.
    unsaved_since: Option<DateTime<Utc>>,
}

impl JobHistory {
    pub fn load(path: PathBuf) -> Self {
        let result = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Journal>(&bytes)
                .map_err(|e| format!("{e}."))
                .and_then(|j| {
                    if j.version == VERSION {
                        Ok(j)
                    } else {
                        Err(format!("Unsupported job history version {}.", j.version))
                    }
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Journal::default()),
            Err(e) => Err(format!("{e}.")),
        };
        let writable = result.is_ok();
        let (journal, storage_error) = match result {
            Ok(journal) => (journal, None),
            Err(e) => (
                Journal::default(),
                Some(format!(
                    "Couldn't read job history: {e} The existing file has been preserved; new jobs are kept only until the app quits."
                )),
            ),
        };
        Self {
            path,
            journal,
            storage_error,
            writable,
            urgent: false,
            unsaved_since: None,
        }
    }

    fn changed(&mut self, now: DateTime<Utc>) {
        self.unsaved_since.get_or_insert(now);
    }

    /// Extends recorded time to `now`, or starts a new span after a gap.
    pub fn cover(&mut self, now: DateTime<Utc>) {
        match self.journal.recorded.last_mut() {
            Some(last) if now < last.to => {}
            Some(last) if now - last.to <= SPAN_GAP => last.to = now,
            _ => {
                self.journal.recorded.push(Span { from: now, to: now });
                self.urgent = true;
            }
        }
        self.changed(now);
    }

    /// Follows the runs of queued and running self-hosted jobs in a fresh scan.
    pub fn track(&mut self, org: &str, scan: &JobScan, runners: &[GhRunner], now: DateTime<Utc>) {
        for job in scan.jobs.iter().filter(|j| is_self_hosted(j, runners)) {
            let run = self.journal.pending.entry(job.run_id).or_insert_with(|| PendingRun {
                org: org.to_lowercase(),
                repo: job.repo.clone(),
                last_seen: now,
                checked_at: None,
            });
            run.last_seen = now;
            run.checked_at = None;
            self.changed(now);
        }
    }

    /// Followed runs of `org` that left the scan and are due a read: `(run id, owner/name)`.
    pub fn due(&self, org: &str, scan: &JobScan, now: DateTime<Utc>, limit: usize) -> Vec<(u64, String)> {
        let org = org.to_lowercase();
        let active: HashSet<u64> = scan.jobs.iter().map(|j| j.run_id).collect();
        let mut due: Vec<_> = self
            .journal
            .pending
            .iter()
            .filter(|(id, run)| {
                run.org == org && !active.contains(id) && run.checked_at.is_none_or(|at| now - at >= RECHECK_AFTER)
            })
            .collect();
        due.sort_by_key(|(_, run)| (run.checked_at, run.last_seen));
        due.into_iter()
            .take(limit)
            .map(|(id, run)| (*id, run.repo.clone()))
            .collect()
    }

    /// Records a run's completed self-hosted jobs. A run whose complete listing has no
    /// unfinished self-hosted job is no longer followed; otherwise it's read again later.
    pub fn read_run(&mut self, run_id: u64, jobs: &[GhJob], truncated: bool, runners: &[GhRunner], now: DateTime<Utc>) {
        let Some(org) = self.journal.pending.get(&run_id).map(|r| r.org.clone()) else {
            return;
        };
        let mut finished = !truncated;
        for job in jobs.iter().filter(|j| is_self_hosted(j, runners)) {
            if job.status != "completed" {
                finished = false;
            } else if !self.journal.jobs.iter().any(|j| j.id == job.id)
                && let Some(record) = FleetFinishedJob::from_github(&org, job, runners)
            {
                self.journal.jobs.push(record);
                self.urgent = true;
            }
        }
        if finished {
            self.forget_run(run_id, now);
        } else {
            self.checked(run_id, now);
        }
    }

    /// Stops following a run: it finished, or GitHub no longer has it.
    pub fn forget_run(&mut self, run_id: u64, now: DateTime<Utc>) {
        if self.journal.pending.remove(&run_id).is_some() {
            self.changed(now);
        }
    }

    /// A read failed or found the run unfinished: wait before reading it again.
    pub fn checked(&mut self, run_id: u64, now: DateTime<Utc>) {
        if let Some(run) = self.journal.pending.get_mut(&run_id) {
            run.checked_at = Some(now);
            self.changed(now);
        }
    }

    /// Applies the age and count limits.
    fn prune(&mut self, now: DateTime<Utc>) {
        let cutoff = now - RETENTION;
        let j = &mut self.journal;
        j.jobs.retain(|job| job.completed_at >= cutoff);
        j.jobs.sort_by_key(|job| (job.completed_at, job.id));
        j.jobs.drain(..j.jobs.len().saturating_sub(MAX_JOBS));
        j.recorded.retain(|s| s.to >= cutoff);
        j.recorded.drain(..j.recorded.len().saturating_sub(MAX_SPANS));
        j.pending.retain(|_, r| now - r.last_seen <= PENDING_RETENTION);
        if j.pending.len() > MAX_PENDING_RUNS {
            let mut oldest: Vec<_> = j.pending.iter().map(|(id, r)| (r.last_seen, *id)).collect();
            oldest.sort_unstable();
            for (_, id) in &oldest[..oldest.len() - MAX_PENDING_RUNS] {
                j.pending.remove(id);
            }
        }
    }

    /// Prunes, then returns the file's path and contents if a save is due. Serialized under the
    /// caller's lock; write without it, then report with [`Self::saved`].
    pub fn save_due(&mut self, now: DateTime<Utc>) -> Option<(PathBuf, Vec<u8>)> {
        self.prune(now);
        let due = self.urgent || self.unsaved_since.is_some_and(|at| now - at >= CHECKPOINT);
        // An unreadable file's load error already explains why nothing is saved.
        if !due || !self.writable {
            return None;
        }
        match serde_json::to_vec(&self.journal) {
            Ok(bytes) => {
                self.urgent = false;
                self.unsaved_since = None;
                Some((self.path.clone(), bytes))
            }
            Err(e) => {
                self.storage_error = Some(format!("Couldn't save job history: {e}."));
                None
            }
        }
    }

    /// Records how a write from [`Self::save_due`] went; a failed one is retried next round.
    pub fn saved(&mut self, result: std::io::Result<()>, now: DateTime<Utc>) {
        match result {
            Ok(()) => self.storage_error = None,
            Err(e) => {
                self.urgent = true;
                self.changed(now);
                self.storage_error = Some(format!("Couldn't save job history: {e}."));
            }
        }
    }

    pub fn attach(&self, view: &mut FleetView, now: DateTime<Utc>) {
        view.activity = super::summary::activity(&self.journal.jobs, &self.journal.recorded, view, now);
        view.activity.storage_error.clone_from(&self.storage_error);
    }

    #[cfg(test)]
    pub fn jobs(&self) -> &[FleetFinishedJob] {
        &self.journal.jobs
    }

    #[cfg(test)]
    pub fn recorded(&self) -> &[Span] {
        &self.journal.recorded
    }

    #[cfg(test)]
    pub fn storage_error(&self) -> Option<&str> {
        self.storage_error.as_deref()
    }
}
