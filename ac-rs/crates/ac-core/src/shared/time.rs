//! Tier 0 — canonical timestamp formatting.
//!
//! All daemon-emitted timestamps and on-disk artefacts use UTC ISO-8601 so
//! every surface agrees on a single, unambiguous representation:
//!
//! - [`now_utc_iso8601`] — human/report/frame timestamps, `2026-05-27T14:22:08Z`.
//! - [`now_utc_filename_stamp`] — the same instant compacted for filenames,
//!   `20260527T142208Z` (no separators, filesystem-safe, still sorts).
//! - [`age_from_iso8601`] — humanized "N days ago" for a stored timestamp,
//!   the fast-scan answer to "is this still good" without date arithmetic
//!   by hand.

/// Format string for display/report/frame timestamps (`%Y-%m-%dT%H:%M:%SZ`).
const ISO8601_UTC: &str = "%Y-%m-%dT%H:%M:%SZ";

/// Format string for filename stamps (`%Y%m%dT%H%M%SZ`).
const ISO8601_UTC_COMPACT: &str = "%Y%m%dT%H%M%SZ";

/// Current UTC time as `2026-05-27T14:22:08Z`.
///
/// The canonical timestamp for `MeasurementReport`, daemon frames, calibration
/// `imported_at`, and CSV export headers.
pub fn now_utc_iso8601() -> String {
    chrono::Utc::now().format(ISO8601_UTC).to_string()
}

/// Current UTC time as `20260527T142208Z`, for embedding in filenames.
pub fn now_utc_filename_stamp() -> String {
    chrono::Utc::now().format(ISO8601_UTC_COMPACT).to_string()
}

/// Humanized age of an RFC3339 timestamp, relative to now — `"2 days ago"`,
/// `"3 hours ago"`, `"just now"`. An unparseable timestamp (e.g. an older
/// on-disk artefact predating this field) returns `"unknown age"` rather
/// than panicking — this is a display helper, not a validator.
pub fn age_from_iso8601(ts: &str) -> String {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(ts) else {
        return "unknown age".to_string();
    };
    let secs = chrono::Utc::now()
        .signed_duration_since(then.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0);

    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        let n = secs / 60;
        format!("{n} minute{} ago", if n == 1 { "" } else { "s" })
    } else if secs < 86_400 {
        let n = secs / 3600;
        format!("{n} hour{} ago", if n == 1 { "" } else { "s" })
    } else {
        let n = secs / 86_400;
        format!("{n} day{} ago", if n == 1 { "" } else { "s" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_has_expected_shape() {
        let s = now_utc_iso8601();
        // `YYYY-MM-DDTHH:MM:SSZ` — 20 chars, trailing Z, T separator.
        assert_eq!(s.len(), 20, "got {s:?}");
        assert!(s.ends_with('Z'), "got {s:?}");
        assert_eq!(s.as_bytes()[10], b'T', "got {s:?}");
        assert_eq!(s.as_bytes()[4], b'-', "got {s:?}");
        assert_eq!(s.as_bytes()[13], b':', "got {s:?}");
        // Round-trips through chrono's RFC3339 parser.
        assert!(
            chrono::DateTime::parse_from_rfc3339(&s).is_ok(),
            "got {s:?}"
        );
    }

    #[test]
    fn age_from_iso8601_buckets_by_elapsed_time() {
        let now = chrono::Utc::now();
        let fmt = |d: chrono::Duration| (now - d).format(ISO8601_UTC).to_string();

        assert_eq!(
            age_from_iso8601(&fmt(chrono::Duration::seconds(5))),
            "just now"
        );
        assert_eq!(
            age_from_iso8601(&fmt(chrono::Duration::minutes(2))),
            "2 minutes ago"
        );
        assert_eq!(
            age_from_iso8601(&fmt(chrono::Duration::minutes(1))),
            "1 minute ago"
        );
        assert_eq!(
            age_from_iso8601(&fmt(chrono::Duration::hours(3))),
            "3 hours ago"
        );
        assert_eq!(
            age_from_iso8601(&fmt(chrono::Duration::days(2))),
            "2 days ago"
        );
        assert_eq!(
            age_from_iso8601(&fmt(chrono::Duration::days(1))),
            "1 day ago"
        );
    }

    #[test]
    fn age_from_iso8601_unparseable_input_does_not_panic() {
        assert_eq!(age_from_iso8601("not a timestamp"), "unknown age");
    }

    #[test]
    fn filename_stamp_has_expected_shape() {
        let s = now_utc_filename_stamp();
        // `YYYYMMDDTHHMMSSZ` — 16 chars, trailing Z, no separators except T.
        assert_eq!(s.len(), 16, "got {s:?}");
        assert!(s.ends_with('Z'), "got {s:?}");
        assert_eq!(s.as_bytes()[8], b'T', "got {s:?}");
        assert!(
            s[..8].bytes().all(|b| b.is_ascii_digit()),
            "date part not all digits: {s:?}"
        );
        assert!(
            s[9..15].bytes().all(|b| b.is_ascii_digit()),
            "time part not all digits: {s:?}"
        );
    }
}
