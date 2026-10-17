//! What a prune would remove, decided from the inventory, and the exact commands that remove it.
//!
//! A plan is built the same way for a dry run and a real one. The dry run is that plan with the
//! commands withheld, not a second estimator that could drift from what the real run does
//! (`CONTRIBUTING.md`). The selection below mirrors Docker's own filters — `until=` on image
//! creation and on build-cache last use, "unused" meaning no container at all, running or stopped
//! — so the figures a dry run prints are the figures `docker system df` would have given for the
//! same set.

use std::collections::BTreeSet;
use std::time::Duration;

use super::usage::{DiskUsage, parse_size, parse_timestamp};

/// The lowest daemon API that makes a plain `volume prune` mean anonymous volumes only. Before
/// it the same command removed every unused volume, named ones included.
const ANONYMOUS_PRUNE_API: (u32, u32) = (1, 42);

/// Which volumes `--docker-volumes` and `--docker-volumes-all` may remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeScope {
    /// Only volumes Docker created without a name, which nothing can find again.
    Anonymous,
    /// Every volume no container mounts, named ones included.
    All,
}

impl VolumeScope {
    /// The word for this scope in a report.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Anonymous => "anonymous",
            Self::All => "all",
        }
    }
}

/// The three things a prune can reclaim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneKind {
    /// Dangling images, and unused images past the age limit.
    Images,
    /// Build-cache records unused for longer than the age limit.
    BuildCache,
    /// Unmounted volumes, within the requested scope.
    Volumes,
}

impl LaneKind {
    /// The word for this lane in a report.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Images => "images",
            Self::BuildCache => "build cache",
            Self::Volumes => "volumes",
        }
    }
}

/// One independent unit of work: what it would remove, and the commands that do it.
///
/// Lanes touch disjoint things and run side by side; the commands inside one lane run in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanePlan {
    /// What this lane reclaims.
    pub kind: LaneKind,
    /// How many images, cache records or volumes the inventory says match.
    pub candidates: usize,
    /// What removing them frees, by the inventory's own sizes.
    pub estimated_bytes: u64,
    /// The `docker` argument lists to run, in order. Empty when the lane is skipped.
    pub commands: Vec<Vec<String>>,
    /// Why the lane will not run, when it will not.
    pub skipped: Option<String>,
}

/// The `until=` value Docker takes: a whole number of hours, rounded up so a plan never reaches
/// closer to now than the age that was asked for.
#[must_use]
pub fn until_hours(max_age: Duration) -> u64 {
    max_age.as_secs().div_ceil(3600)
}

fn until_filter(hours: u64) -> String {
    format!("until={hours}h")
}

fn command(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

fn command_until(parts: &[&str], hours: u64) -> Vec<String> {
    let mut command = command(parts);
    command.extend(["--filter".to_owned(), until_filter(hours)]);
    command
}

fn cutoff(now: i64, hours: u64) -> i64 {
    now.saturating_sub(i64::try_from(hours.saturating_mul(3600)).unwrap_or(i64::MAX))
}

fn is_untagged(repository: &str) -> bool {
    matches!(repository.trim(), "" | "<none>")
}

/// The image lane: every dangling image, then every unused image older than `max_age`.
#[must_use]
pub fn images(usage: &DiskUsage, now: i64, max_age: Duration) -> LanePlan {
    let hours = until_hours(max_age);
    let cutoff = cutoff(now, hours);

    let mut seen = BTreeSet::new();
    let mut candidates = 0;
    let mut estimated_bytes = 0;
    for image in &usage.images {
        // One image can carry several tags, one record each; it is removed, and counted, once.
        if !seen.insert(image.id.as_str()) {
            continue;
        }
        let tags = || usage.images.iter().filter(|other| other.id == image.id);
        // Anything the inventory cannot read counts as in use: a container count of `N/A` is not
        // zero containers.
        let unused = tags().all(|tag| tag.containers.trim().parse::<u64>() == Ok(0));
        let dangling = tags().all(|tag| is_untagged(&tag.repository));
        let old = parse_timestamp(&image.created_at).is_some_and(|created| created < cutoff);
        if unused && (dangling || old) {
            candidates += 1;
            estimated_bytes += parse_size(&image.unique_size).unwrap_or(0);
        }
    }

    LanePlan {
        kind: LaneKind::Images,
        candidates,
        estimated_bytes,
        commands: vec![
            command(&["image", "prune", "--force"]),
            command_until(&["image", "prune", "--all", "--force"], hours),
        ],
        skipped: None,
    }
}

/// The build-cache lane: records no build is using and nothing has read for `max_age`.
///
/// `--all` because without it Docker removes only the dangling records and leaves the large,
/// reusable layers a `docker build` pulled in; the age filter is what keeps the cache a recent
/// build still wants.
#[must_use]
pub fn build_cache(usage: &DiskUsage, now: i64, max_age: Duration) -> LanePlan {
    let hours = until_hours(max_age);
    let cutoff = cutoff(now, hours);

    let selected: Vec<_> = usage
        .build_cache
        .iter()
        .filter(|record| record.in_use.trim().eq_ignore_ascii_case("false"))
        .filter(|record| parse_timestamp(&record.last_used_at).is_some_and(|used| used < cutoff))
        .collect();

    LanePlan {
        kind: LaneKind::BuildCache,
        candidates: selected.len(),
        estimated_bytes: selected.iter().filter_map(|record| parse_size(&record.size)).sum(),
        commands: vec![command_until(&["builder", "prune", "--all", "--force"], hours)],
        skipped: None,
    }
}

/// The volume lane: volumes no container mounts, within `scope`.
///
/// `api_version` gates the anonymous form. A daemon older than 1.42 reads a plain `volume prune`
/// as "every unused volume", so asking it for anonymous ones would remove named data; the lane is
/// skipped there instead.
#[must_use]
pub fn volumes(usage: &DiskUsage, scope: VolumeScope, api_version: &str) -> LanePlan {
    let selected: Vec<_> = usage
        .volumes
        .iter()
        .filter(|volume| volume.links.trim().parse::<u64>() == Ok(0))
        .filter(|volume| scope == VolumeScope::All || volume.is_anonymous())
        .collect();

    let (commands, skipped) = if scope == VolumeScope::Anonymous && !api_at_least(api_version, ANONYMOUS_PRUNE_API) {
        let version = api_version.trim();
        (
            Vec::new(),
            Some(format!(
                "daemon API {} predates anonymous-only volume pruning (1.42)",
                if version.is_empty() { "unknown" } else { version }
            )),
        )
    } else {
        let prune = match scope {
            VolumeScope::Anonymous => command(&["volume", "prune", "--force"]),
            VolumeScope::All => command(&["volume", "prune", "--all", "--force"]),
        };
        (vec![prune], None)
    };

    LanePlan {
        kind: LaneKind::Volumes,
        candidates: selected.len(),
        estimated_bytes: selected.iter().filter_map(|volume| parse_size(&volume.size)).sum(),
        commands,
        skipped,
    }
}

/// Whether a `major.minor` API version is at least `floor`. An unreadable version is not.
fn api_at_least(version: &str, floor: (u32, u32)) -> bool {
    let mut parts = version.trim().split('.');
    let (Some(major), Some(minor)) = (
        parts.next().and_then(|part| part.parse::<u32>().ok()),
        parts.next().and_then(|part| part.parse::<u32>().ok()),
    ) else {
        return false;
    };
    (major, minor) >= floor
}

#[cfg(test)]
mod tests {
    use super::super::usage::parse_usage;
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/docker/df_verbose.json");
    /// 2026-10-17 00:00:00 UTC, the day the fixture was recorded.
    const NOW: i64 = 1_792_195_200;
    const WEEK: Duration = Duration::from_secs(7 * 86_400);

    fn usage() -> DiskUsage {
        parse_usage(FIXTURE).expect("the recorded fixture parses")
    }

    fn joined(command: &[String]) -> String {
        command.join(" ")
    }

    #[test]
    fn should_round_the_age_up_to_whole_hours() {
        assert_eq!(until_hours(WEEK), 168);
        assert_eq!(until_hours(Duration::from_secs(3601)), 2);
        assert_eq!(until_hours(Duration::ZERO), 0);
    }

    #[test]
    fn should_build_the_image_commands_with_an_until_filter() {
        let lane = images(&usage(), NOW, WEEK);
        let commands: Vec<_> = lane.commands.iter().map(|command| joined(command)).collect();
        assert_eq!(
            commands,
            ["image prune --force", "image prune --all --force --filter until=168h"]
        );
    }

    #[test]
    fn should_select_dangling_and_old_unused_images_once_each() {
        let lane = images(&usage(), NOW, WEEK);
        // The dangling image, plus the 8-day-old unused `golang` listed under two tags. The
        // 2-day-old unused `erlang` is too young, `armis/dazel` has a container, and `busybox`
        // has an unreadable count.
        assert_eq!(lane.candidates, 2);
        assert_eq!(lane.estimated_bytes, 100_000_000 + 1_500_000_000);
    }

    #[test]
    fn should_widen_the_image_set_as_the_age_shrinks() {
        let lane = images(&usage(), NOW, Duration::from_secs(86_400));
        assert_eq!(lane.candidates, 3, "the 2-day-old erlang image is now past the limit");
    }

    #[test]
    fn should_never_select_an_image_a_container_uses_or_one_it_cannot_read() {
        let lane = images(&usage(), NOW, Duration::ZERO);
        assert_eq!(lane.candidates, 3, "dazel (in use) and busybox (unreadable count) stay");
    }

    #[test]
    fn should_select_only_idle_cache_records_past_the_age() {
        let lane = build_cache(&usage(), NOW, WEEK);
        assert_eq!(
            lane.candidates, 1,
            "one in use, one used yesterday, one unused for weeks"
        );
        assert_eq!(lane.estimated_bytes, 115_000_000);
        assert_eq!(
            joined(&lane.commands[0]),
            "builder prune --all --force --filter until=168h"
        );
    }

    #[test]
    fn should_select_only_unmounted_anonymous_volumes_by_default() {
        let lane = volumes(&usage(), VolumeScope::Anonymous, "1.54");
        assert_eq!(lane.candidates, 1);
        assert_eq!(lane.estimated_bytes, 111_600);
        assert_eq!(joined(&lane.commands[0]), "volume prune --force");
    }

    #[test]
    fn should_widen_to_named_unmounted_volumes_only_for_all() {
        let lane = volumes(&usage(), VolumeScope::All, "1.54");
        assert_eq!(lane.candidates, 2, "the mounted named volume is still kept");
        assert_eq!(joined(&lane.commands[0]), "volume prune --all --force");
    }

    #[test]
    fn should_skip_anonymous_pruning_on_a_daemon_that_would_remove_named_volumes() {
        let lane = volumes(&usage(), VolumeScope::Anonymous, "1.41");
        assert_eq!(lane.commands.len(), 0);
        assert!(lane.skipped.is_some_and(|reason| reason.contains("1.41")));
    }

    #[test]
    fn should_compare_api_versions_numerically() {
        assert!(api_at_least("1.54", (1, 42)));
        assert!(api_at_least("1.42", (1, 42)));
        assert!(!api_at_least("1.9", (1, 42)));
        assert!(!api_at_least("", (1, 42)));
    }
}
