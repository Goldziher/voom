//! Keep policies: the rules that hold an artifact back after it has been proven.
//!
//! `min_age` in particular is what makes voom safe to run on a schedule — today's active build
//! is never touched, last month's is (ADR 0004).
//!
//! Durations and sizes are human strings, parsed **strictly**. An unparseable value is a hard
//! error rather than a silently ignored default, because a keep policy is a guard and a guard
//! the user believes they set must not quietly not exist.

use std::time::{Duration, SystemTime};

use crate::classify::SkipReason;
use crate::error::{Error, Result};

/// Rules that keep a proven artifact on disk.
///
/// Every field is optional, and merging narrows rather than replaces: an override sets only the
/// keys it names and the rest inherit. Replacement would mean a user who sets one key silently
/// loses every other guard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeepPolicy {
    /// Never remove an artifact modified more recently than this.
    pub min_age: Option<Duration>,
    /// Not worth the rebuild below this size.
    pub min_size: Option<u64>,
    /// Skip anything larger — likely not what the user meant.
    pub max_size: Option<u64>,
}

impl KeepPolicy {
    /// Applies `other` on top of `self`, key by key.
    #[must_use]
    pub fn narrow(self, other: Self) -> Self {
        Self {
            min_age: other.min_age.or(self.min_age),
            min_size: other.min_size.or(self.min_size),
            max_size: other.max_size.or(self.max_size),
        }
    }

    /// Whether anything is set at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.min_age.is_none() && self.min_size.is_none() && self.max_size.is_none()
    }

    /// Whether the artifact is too recently modified to remove.
    ///
    /// Checked from the artifact's own timestamp before it is sized, so an artifact held by age
    /// costs one `stat` rather than a recursive walk.
    #[must_use]
    pub fn holds_by_age(&self, modified: SystemTime, now: SystemTime) -> Option<SkipReason> {
        let min_age = self.min_age?;
        let age = now.duration_since(modified).ok()?;
        (age < min_age).then(|| SkipReason::KeptByPolicy {
            rule: "min_age",
            detail: format!("modified {} ago, min_age is {}", humanize(age), humanize(min_age)),
        })
    }

    /// Whether the artifact's size holds it.
    #[must_use]
    pub fn holds_by_size(&self, bytes: u64) -> Option<SkipReason> {
        use humansize::{DECIMAL, format_size};
        if let Some(min_size) = self.min_size
            && bytes < min_size
        {
            return Some(SkipReason::KeptByPolicy {
                rule: "min_size",
                detail: format!(
                    "{} is below {}",
                    format_size(bytes, DECIMAL),
                    format_size(min_size, DECIMAL)
                ),
            });
        }
        if let Some(max_size) = self.max_size
            && bytes > max_size
        {
            return Some(SkipReason::KeptByPolicy {
                rule: "max_size",
                detail: format!(
                    "{} is above {}",
                    format_size(bytes, DECIMAL),
                    format_size(max_size, DECIMAL)
                ),
            });
        }
        None
    }
}

/// 2^64 exactly. Comparing against `u64::MAX as f64` would itself be a lossy cast, and NaN has
/// to be tested explicitly because every comparison with it is false.
const OVERFLOW_BYTES: f64 = 18_446_744_073_709_551_616.0;

/// Splits a value into its numeric part and its unit suffix.
fn split_unit(input: &str) -> (&str, &str) {
    let boundary = input.find(|character: char| !character.is_ascii_digit() && character != '.');
    boundary.map_or((input, ""), |index| input.split_at(index))
}

/// Parses a human duration: `45s`, `30m`, `12h`, `7d`, `2w`.
///
/// # Errors
///
/// [`Error::InvalidDuration`] for an empty value, an unrecognised unit, or a missing number.
/// There is no default unit — `7` alone is rejected, because guessing between seconds and days
/// is exactly the ambiguity a safety-relevant value cannot afford.
pub fn parse_duration(input: &str) -> Result<Duration> {
    let trimmed = input.trim();
    let invalid = |reason: &'static str| Error::InvalidDuration {
        input: input.to_owned(),
        reason,
    };
    if trimmed.is_empty() {
        return Err(invalid("it is empty"));
    }

    let (number, unit) = split_unit(trimmed);
    let value: f64 = number.parse().map_err(|_| invalid("it does not start with a number"))?;
    if value < 0.0 {
        return Err(invalid("it is negative"));
    }

    let seconds = match unit {
        "s" | "sec" | "secs" => 1.0,
        "m" | "min" | "mins" => 60.0,
        "h" | "hr" | "hrs" => 3_600.0,
        "d" | "day" | "days" => 86_400.0,
        "w" | "week" | "weeks" => 604_800.0,
        "" => return Err(invalid("it has no unit — use s, m, h, d or w")),
        _ => return Err(invalid("its unit is not one of s, m, h, d, w")),
    };

    Ok(Duration::from_secs_f64(value * seconds))
}

/// Parses a human size: `1024`, `1KB`, `50GB`, `1MiB`.
///
/// Decimal units are powers of 1000 and binary units powers of 1024, spelled the way the
/// standards do. A bare number is bytes.
///
/// # Errors
///
/// [`Error::InvalidSize`] for an empty value, a negative value, or an unrecognised unit.
pub fn parse_size(input: &str) -> Result<u64> {
    let trimmed = input.trim();
    let invalid = |reason: &'static str| Error::InvalidSize {
        input: input.to_owned(),
        reason,
    };
    if trimmed.is_empty() {
        return Err(invalid("it is empty"));
    }

    let (number, unit) = split_unit(trimmed);
    let value: f64 = number.parse().map_err(|_| invalid("it does not start with a number"))?;
    if value < 0.0 {
        return Err(invalid("it is negative"));
    }

    let multiplier: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "kib" | "k" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return Err(invalid("its unit is not one of B, KB, MB, GB, TB, KiB, MiB, GiB, TiB")),
    };

    let bytes = value * multiplier;
    if bytes.is_nan() || bytes >= OVERFLOW_BYTES {
        return Err(invalid("it is too large"));
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "bounds checked above"
    )]
    Ok(bytes as u64)
}

/// A compact human rendering of a duration, for skip explanations.
fn humanize(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        86_400..604_800 => format!("{}d", seconds / 86_400),
        _ => format!("{}w", seconds / 604_800),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_parse_every_documented_duration_unit() {
        assert_eq!(parse_duration("45s").unwrap(), Duration::from_secs(45));
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1_800));
        assert_eq!(parse_duration("12h").unwrap(), Duration::from_secs(43_200));
        assert_eq!(parse_duration("7d").unwrap(), Duration::from_secs(604_800));
        assert_eq!(parse_duration("2w").unwrap(), Duration::from_secs(1_209_600));
        assert_eq!(parse_duration(" 1d ").unwrap(), Duration::from_secs(86_400));
    }

    /// A bare number could mean seconds or days. Guessing is not acceptable for a value whose
    /// job is to stop a deletion.
    #[test]
    fn should_reject_a_duration_with_no_unit() {
        let error = parse_duration("7").unwrap_err();
        assert!(matches!(error, Error::InvalidDuration { .. }));
        assert!(error.to_string().contains("no unit"));
    }

    #[test]
    fn should_reject_an_unparseable_duration_rather_than_defaulting() {
        for input in ["", "d", "7y", "later", "-1d"] {
            assert!(parse_duration(input).is_err(), "`{input}` must be rejected");
        }
    }

    #[test]
    fn should_distinguish_decimal_and_binary_sizes() {
        assert_eq!(parse_size("1MB").unwrap(), 1_000_000);
        assert_eq!(parse_size("1MiB").unwrap(), 1_048_576);
        assert_eq!(parse_size("50GB").unwrap(), 50_000_000_000);
        assert_eq!(parse_size("1024").unwrap(), 1_024, "a bare number is bytes");
        assert_eq!(parse_size("1.5MB").unwrap(), 1_500_000);
        assert_eq!(parse_size("1mb").unwrap(), 1_000_000, "units are case-insensitive");
    }

    #[test]
    fn should_reject_an_unparseable_size() {
        for input in ["", "MB", "1XB", "-1MB", "big"] {
            assert!(parse_size(input).is_err(), "`{input}` must be rejected");
        }
    }

    #[test]
    fn should_hold_an_artifact_younger_than_min_age() {
        let policy = KeepPolicy {
            min_age: Some(Duration::from_secs(604_800)),
            ..KeepPolicy::default()
        };
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let recent = now - Duration::from_secs(3_600);
        let old = now - Duration::from_secs(1_209_600);

        let held = policy.holds_by_age(recent, now).expect("a recent artifact is held");
        assert!(matches!(held, SkipReason::KeptByPolicy { rule: "min_age", .. }));
        assert_eq!(held.to_string(), "kept by min_age (modified 1h ago, min_age is 1w)");
        assert!(policy.holds_by_age(old, now).is_none(), "an old artifact is released");
    }

    #[test]
    fn should_hold_artifacts_outside_the_size_bounds() {
        let policy = KeepPolicy {
            min_age: None,
            min_size: Some(1_000_000),
            max_size: Some(50_000_000_000),
        };
        assert!(policy.holds_by_size(500).is_some(), "too small to be worth the rebuild");
        assert!(
            policy.holds_by_size(60_000_000_000).is_some(),
            "too large to be what was meant"
        );
        assert!(policy.holds_by_size(4_000_000).is_none());
    }

    /// An override sets only the keys it names. Replacement would mean setting one key silently
    /// drops every other guard.
    #[test]
    fn should_narrow_rather_than_replace_when_merging() {
        let base = KeepPolicy {
            min_age: Some(Duration::from_secs(604_800)),
            min_size: Some(1_000_000),
            max_size: Some(50_000_000_000),
        };
        let override_age = KeepPolicy {
            min_age: Some(Duration::from_secs(86_400)),
            ..KeepPolicy::default()
        };
        let merged = base.narrow(override_age);

        assert_eq!(
            merged.min_age,
            Some(Duration::from_secs(86_400)),
            "the named key is overridden"
        );
        assert_eq!(merged.min_size, base.min_size, "unnamed keys survive");
        assert_eq!(merged.max_size, base.max_size);
    }

    #[test]
    fn an_empty_policy_holds_nothing() {
        let policy = KeepPolicy::default();
        assert!(policy.is_empty());
        assert!(policy.holds_by_size(0).is_none());
        assert!(policy.holds_by_age(SystemTime::UNIX_EPOCH, SystemTime::now()).is_none());
    }
}
