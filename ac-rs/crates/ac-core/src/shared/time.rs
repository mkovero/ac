//! Tier 0 — canonical timestamp formatting.
//!
//! All daemon-emitted timestamps and on-disk artefacts use UTC ISO-8601 so
//! every surface agrees on a single, unambiguous representation:
//!
//! - [`now_utc_iso8601`] — human/report/frame timestamps, `2026-05-27T14:22:08Z`.
//! - [`now_utc_filename_stamp`] — the same instant compacted for filenames,
//!   `20260527T142208Z` (no separators, filesystem-safe, still sorts).

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
        assert!(chrono::DateTime::parse_from_rfc3339(&s).is_ok(), "got {s:?}");
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
