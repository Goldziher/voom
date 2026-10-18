//! What `docker system df -v --format json` says, and the small parsers its string-typed fields
//! need.
//!
//! The CLI renders every number as a string for people — `"2.39GB"`, `"2 days ago"`, `"N/A"` —
//! so nothing here trusts a field to be present or parseable. A value that cannot be read
//! never counts toward a candidate: the planner keeps what it cannot understand.

use serde::{Deserialize, Deserializer};

/// The three inventories a plan is made from.
#[derive(Debug, Default, Deserialize)]
pub struct DiskUsage {
    /// Every image, one record per repository and tag.
    #[serde(rename = "Images", default, deserialize_with = "nullable")]
    pub images: Vec<ImageRecord>,
    /// Every local volume.
    #[serde(rename = "Volumes", default, deserialize_with = "nullable")]
    pub volumes: Vec<VolumeRecord>,
    /// Every build-cache record.
    #[serde(rename = "BuildCache", default, deserialize_with = "nullable")]
    pub build_cache: Vec<CacheRecord>,
}

/// One image tag.
#[derive(Debug, Default, Deserialize)]
pub struct ImageRecord {
    /// The full content id.
    #[serde(rename = "ID", default)]
    pub id: String,
    /// `<none>` for an untagged image.
    #[serde(rename = "Repository", default)]
    pub repository: String,
    /// How many containers, running or not, were made from it.
    #[serde(rename = "Containers", default)]
    pub containers: String,
    /// When it was built, as `2026-10-14 22:28:15 +0200 CEST`.
    #[serde(rename = "CreatedAt", default)]
    pub created_at: String,
    /// What removing it frees: the layers no other image shares.
    #[serde(rename = "UniqueSize", default)]
    pub unique_size: String,
}

/// One local volume.
#[derive(Debug, Default, Deserialize)]
pub struct VolumeRecord {
    /// Comma-separated `key=value` labels.
    #[serde(rename = "Labels", default)]
    pub labels: String,
    /// How many containers, running or not, mount it.
    #[serde(rename = "Links", default)]
    pub links: String,
    /// Its size on disk, `N/A` when the daemon did not measure it.
    #[serde(rename = "Size", default)]
    pub size: String,
}

/// One build-cache record.
#[derive(Debug, Default, Deserialize)]
pub struct CacheRecord {
    /// Whether a build is using it right now.
    #[serde(rename = "InUse", default)]
    pub in_use: String,
    /// When anything last read it, as `2026-09-08 06:22:34.821884134 +0000 UTC`.
    #[serde(rename = "LastUsedAt", default)]
    pub last_used_at: String,
    /// Its size on disk.
    #[serde(rename = "Size", default)]
    pub size: String,
}

/// The label Docker puts on a volume it created without a name.
pub const ANONYMOUS_VOLUME_LABEL: &str = "com.docker.volume.anonymous";

impl VolumeRecord {
    /// Whether Docker created this volume itself rather than a user naming it.
    #[must_use]
    pub fn is_anonymous(&self) -> bool {
        self.labels.split(',').any(|label| {
            label
                .split('=')
                .next()
                .is_some_and(|key| key.trim() == ANONYMOUS_VOLUME_LABEL)
        })
    }
}

/// The daemon renders an empty list as `null`, which a plain `Vec` refuses.
fn nullable<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

/// Parses `docker system df -v --format json`.
///
/// # Errors
///
/// The JSON error when the output is not the document this reads.
pub fn parse_usage(json: &str) -> Result<DiskUsage, serde_json::Error> {
    serde_json::from_str(json)
}

/// Reads a size as Docker prints it — `2.39GB`, `111.6kB`, `0B` — in bytes.
///
/// Docker's units are decimal, so `1GB` is 10^9. `N/A`, `-1`, and anything else unreadable are
/// `None`, never zero: zero is a claim.
#[must_use]
pub fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim();
    let split = text.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (number, unit) = text.split_at(split);
    let number: f64 = number.parse().ok()?;
    let multiplier: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "pb" => 1e15,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0_f64.powi(4),
        _ => return None,
    };
    let bytes = (number * multiplier).round();
    // The value is non-negative by construction (a digits-only prefix) and far below u64::MAX.
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked above")]
    (bytes.is_finite() && bytes >= 0.0).then_some(bytes as u64)
}

/// Reads `2026-10-14 22:28:15 +0200 CEST` or `2026-09-08 06:22:34.821884134 +0000 UTC` as seconds
/// since the Unix epoch.
#[must_use]
pub fn parse_timestamp(text: &str) -> Option<i64> {
    let mut parts = text.split_whitespace();
    let date = parts.next()?;
    let time = parts.next()?;
    let offset = parts.next()?;

    let mut date = date.splitn(3, '-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || !(1..=9999).contains(&year) {
        return None;
    }

    let mut time = time.split(':');
    let hour: i64 = time.next()?.parse().ok()?;
    let minute: i64 = time.next()?.parse().ok()?;
    let second: i64 = time.next()?.split('.').next()?.parse().ok()?;

    let sign = match offset.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits = offset.get(1..5)?;
    let offset_seconds =
        sign * (digits.get(..2)?.parse::<i64>().ok()? * 3600 + digits.get(2..)?.parse::<i64>().ok()? * 60);

    // Every field comes off the wire, so the arithmetic is checked rather than trusted: a crafted
    // timestamp must yield `None`, not a panic in debug or a wrap in release.
    days_from_civil(year, month, day)
        .checked_mul(86_400)?
        .checked_add(hour.checked_mul(3600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)?
        .checked_sub(offset_seconds)
}

/// Days from 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The size a prune printed: `Total reclaimed space: 1.5GB` from `image` and `volume`, or the
/// `Total:\t1.5GB` the build-cache prune ends on.
#[must_use]
pub fn parse_reclaimed(output: &str) -> Option<u64> {
    output.lines().rev().find_map(|line| {
        let line = line.trim();
        let rest = line
            .strip_prefix("Total reclaimed space:")
            .or_else(|| line.strip_prefix("Total:"))?;
        parse_size(rest)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_read_docker_sizes_as_decimal_bytes() {
        assert_eq!(parse_size("2.39GB"), Some(2_390_000_000));
        assert_eq!(parse_size("111.6kB"), Some(111_600));
        assert_eq!(parse_size("0B"), Some(0));
        assert_eq!(parse_size("1.5GiB"), Some(1_610_612_736));
    }

    #[test]
    fn should_refuse_a_size_it_cannot_read() {
        assert_eq!(parse_size("N/A"), None);
        assert_eq!(parse_size("-1"), None);
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("12"), None);
    }

    #[test]
    fn should_read_a_timestamp_with_a_zone_offset() {
        // 2026-10-14 20:28:15 UTC.
        assert_eq!(parse_timestamp("2026-10-14 22:28:15 +0200 CEST"), Some(1_792_009_695));
    }

    #[test]
    fn should_read_a_timestamp_with_a_fraction_and_the_epoch() {
        assert_eq!(parse_timestamp("1970-01-02 00:00:00.123456789 +0000 UTC"), Some(86_400));
        assert_eq!(parse_timestamp("1969-12-31 19:00:00 -0500 EST"), Some(0));
    }

    #[test]
    fn should_refuse_a_timestamp_it_cannot_read() {
        assert_eq!(parse_timestamp("2 days ago"), None);
        assert_eq!(parse_timestamp(""), None);
        assert_eq!(parse_timestamp("2026-13-01 00:00:00 +0000 UTC"), None);
        // A year outside the calendar must be refused, not overflow the day arithmetic.
        assert_eq!(parse_timestamp("262143-01-01 00:00:00 +0000 UTC"), None);
        assert_eq!(parse_timestamp("9223372036854775807-01-01 00:00:00 +0000 UTC"), None);
    }

    #[test]
    fn should_read_the_total_each_prune_prints() {
        assert_eq!(
            parse_reclaimed("Deleted Images:\nuntagged: x\n\nTotal reclaimed space: 1.5GB\n"),
            Some(1_500_000_000)
        );
        assert_eq!(
            parse_reclaimed("ID\tRECLAIMABLE\nabc\ttrue\nTotal:\t6.8GB\n"),
            Some(6_800_000_000)
        );
        assert_eq!(parse_reclaimed("Total reclaimed space: 0B\n"), Some(0));
        assert_eq!(parse_reclaimed("nothing useful"), None);
    }

    #[test]
    fn should_recognise_an_anonymous_volume_by_its_label() {
        let volume = |labels: &str| VolumeRecord {
            labels: labels.to_owned(),
            ..VolumeRecord::default()
        };
        assert!(volume("com.docker.volume.anonymous=").is_anonymous());
        assert!(volume("a=b,com.docker.volume.anonymous=").is_anonymous());
        assert!(!volume("com.docker.compose.project=app").is_anonymous());
        assert!(!volume("").is_anonymous());
    }

    #[test]
    fn should_accept_null_inventories() {
        let usage = parse_usage(r#"{"Images":null,"Volumes":null,"BuildCache":null}"#).expect("parses");
        assert!(usage.images.is_empty() && usage.volumes.is_empty() && usage.build_cache.is_empty());
    }
}
