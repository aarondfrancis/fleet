//! Label matching between queued jobs and runners.
//!
//! GitHub routes a job to a self-hosted runner only if the runner has every
//! label in the job's `runs-on` (case-insensitive). Runner-group access rules
//! can still exclude a label-compatible runner, and the API doesn't expose
//! enough to check that per job, so the UI says "label-compatible", never
//! "will run here".

use std::collections::HashSet;

pub fn is_compatible(job_labels: &[String], runner_labels: &[String]) -> bool {
    if job_labels.is_empty() {
        return false;
    }
    let have: HashSet<String> = runner_labels.iter().map(|l| l.to_lowercase()).collect();
    job_labels.iter().all(|l| have.contains(&l.to_lowercase()))
}

/// Jobs that ask for `self-hosted` are clearly ours even if nothing matches now.
pub fn targets_self_hosted(job_labels: &[String]) -> bool {
    job_labels.iter().any(|l| l.eq_ignore_ascii_case("self-hosted"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn subset_case_insensitive() {
        let runner = v(&["self-hosted", "macOS", "ARM64", "xcode-16"]);
        assert!(is_compatible(&v(&["self-hosted", "macos"]), &runner));
        assert!(is_compatible(
            &v(&["self-hosted", "macOS", "arm64", "xcode-16"]),
            &runner
        ));
        assert!(!is_compatible(&v(&["self-hosted", "linux"]), &runner));
        assert!(!is_compatible(&v(&["ubuntu-latest"]), &runner));
    }

    #[test]
    fn empty_job_labels_never_match() {
        assert!(!is_compatible(&[], &v(&["self-hosted"])));
    }

    #[test]
    fn detects_self_hosted_target() {
        assert!(targets_self_hosted(&v(&["Self-Hosted", "gpu"])));
        assert!(!targets_self_hosted(&v(&["ubuntu-latest"])));
    }
}
