//! Runner version drift against recent `actions/runner` releases.
//!
//! GitHub stops sending jobs to a self-hosted runner that hasn't installed an update within
//! 30 days of its release, so the age counts from the oldest release the runner is missing,
//! not from the latest one.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// After this, GitHub may stop sending jobs to a runner missing a release.
pub const UPDATE_DEADLINE: Duration = Duration::days(30);
/// Releases read longer ago than this aren't compared: they may miss newer ones.
pub const RELEASES_USABLE_FOR: Duration = Duration::days(1);

/// A published, non-pre-release `actions/runner` release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerRelease {
    /// Without the tag's leading `v`, e.g. `2.329.0`.
    pub version: String,
    pub published_at: DateTime<Utc>,
}

/// A runner behind the latest release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RunnerUpdate {
    pub latest: String,
    /// When the oldest release the runner is missing was published (the oldest release read,
    /// for a runner older than all of them).
    pub missing_since: DateTime<Utc>,
    /// Missing a release for over 30 days: GitHub may stop sending jobs to the runner.
    pub overdue: bool,
}

/// `major.minor.patch`, ignoring a leading `v`.
fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|p| p.parse().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// How far `current` is behind `releases` (any order); None when it's current or can't be parsed.
pub(crate) fn update(current: &str, releases: &[RunnerRelease], now: DateTime<Utc>) -> Option<RunnerUpdate> {
    let have = parse(current)?;
    let missing: Vec<((u64, u64, u64), &RunnerRelease)> = releases
        .iter()
        .filter_map(|r| Some((parse(&r.version)?, r)))
        .filter(|(version, _)| *version > have)
        .collect();
    let (_, latest) = missing.iter().max_by_key(|(version, _)| *version)?;
    let missing_since = missing.iter().map(|(_, r)| r.published_at).min()?;
    Some(RunnerUpdate {
        latest: latest.version.clone(),
        missing_since,
        overdue: now - missing_since > UPDATE_DEADLINE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 3, 12, 0, 0).unwrap()
    }

    /// (version, days since publishing), newest first like GitHub's list.
    fn releases(list: &[(&str, i64)]) -> Vec<RunnerRelease> {
        list.iter()
            .map(|(version, days)| RunnerRelease {
                version: (*version).into(),
                published_at: now() - Duration::days(*days),
            })
            .collect()
    }

    #[test]
    fn age_counts_from_the_oldest_missing_release() {
        // 2.330.0 came out 40 days ago; the newer patch only 3 days ago.
        let list = releases(&[("2.330.1", 3), ("2.330.0", 40), ("2.329.0", 70)]);
        assert_eq!(
            update("2.329.0", &list, now()),
            Some(RunnerUpdate {
                latest: "2.330.1".into(),
                missing_since: now() - Duration::days(40),
                overdue: true,
            })
        );
        assert_eq!(
            update("2.330.0", &list, now()),
            Some(RunnerUpdate {
                latest: "2.330.1".into(),
                missing_since: now() - Duration::days(3),
                overdue: false,
            })
        );
        // Older than every release read: the oldest one read is the lower bound.
        assert!(update("2.300.0", &list, now()).is_some_and(|u| u.overdue));
    }

    #[test]
    fn current_newer_or_unparseable_versions_need_nothing() {
        let list = releases(&[("2.330.0", 90), ("bogus", 1)]);
        for current in ["2.330.0", "v2.330.0", "2.331.0", "unknown", "2.330"] {
            assert_eq!(update(current, &list, now()), None, "{current}");
        }
        assert_eq!(update("2.329.0", &[], now()), None);
    }
}
