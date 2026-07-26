//! Cursor and SPL readout formatting (deliverable 4). Formatting rules
//! are part of the contract, written down here rather than left
//! implicit in a call site:
//!
//! - Levels are formatted to 2 decimal places (`{:.2}`) — matches this
//!   project's established `-6.75 dB`-class precision convention
//!   (`qa-signoff-m1.5.md`'s fixture derivation), reused here rather
//!   than picking a new one.
//! - The reference label (`dBFS` vs `dB SPL`) is decided **only** by
//!   whether an SPL calibration layer is present (`spl.is_some()` on
//!   the input) — nothing else, per AC5.
//! - `spl`'s own value is voltage-cal-independent by design (parallel
//!   calibration layers off raw digital amplitude — see
//!   `ac_core::shared::calibration`'s module doc). This crate only
//!   formats the number; it doesn't re-derive it, so that guarantee
//!   lives entirely in `ac-core` and `ac-daemon`, not here.
//! - The SPL readout's weighting/integration tags are echoed verbatim
//!   from the frame — never renamed or re-derived. When the input has
//!   no integration tag (the snapshot-derived path — architect review,
//!   decision 3), the readout omits the integration clause entirely
//!   rather than fabricating one (decision 3a).
//! - Cursor frequency is formatted to **whole Hz, no decimals** (UX
//!   review on `handoff-ac-scene.md`) — the value names a log-spaced
//!   *column* (D18), not a single bin; at 1 kHz with 48 cols/octave the
//!   column is ~15 Hz wide, wider at higher frequencies, so any
//!   sub-Hz precision would claim resolution the column geometry
//!   doesn't have. This is a display-precision decision only — the
//!   underlying `f64` frequency and level values are unchanged and
//!   still QA-verified to full precision; only their rendering is
//!   capped here.

use ac_core::visualize::weighting_curves::WeightingCurve;

/// `"{value:.2} dB SPL (A, fast)"` or, with no integration tag (a
/// snapshot-derived scene), `"{value:.2} dB SPL (A)"`. Returns `None`
/// when `spl` is `None` (no SPL calibration layer) — there is nothing
/// to read out.
pub fn format_spl_readout(
    spl: Option<f64>,
    weighting: WeightingCurve,
    integration: Option<&str>,
) -> Option<String> {
    let spl = spl?;
    Some(match integration {
        Some(integ) => format!("{spl:.2} dB SPL ({}, {integ})", weighting.tag()),
        None => format!("{spl:.2} dB SPL ({})", weighting.tag()),
    })
}

/// `"{freq_hz:.0} Hz: {level:.2} dBFS"` or `"... dB SPL"` — the label is
/// decided purely by `has_spl_cal` (AC5), independent of the numeric
/// level shown (which is always the column's own band level, not the
/// broadband `spl` scalar). Frequency is whole Hz (UX review) — the
/// value names a column, not a bin; the level keeps its established
/// 2-decimal precision (a single scalar reading, not a band label).
pub fn format_cursor_readout(freq_hz: f64, level_db: f64, has_spl_cal: bool) -> String {
    let unit = if has_spl_cal { "dB SPL" } else { "dBFS" };
    format!("{freq_hz:.0} Hz: {level_db:.2} {unit}")
}

/// The ARMED banner (§5). Safety UI, not chrome: `ac-view` draws this
/// string verbatim and must never reformat it — F5 exists to catch a
/// renderer that re-derives the text.
///
/// `output_port` is appended in parentheses only when the session has a
/// sticky JACK port configured; with no port the operator sees the
/// channel number alone rather than an empty pair of brackets.
pub fn format_armed_banner(
    output_channel: u32,
    output_port: Option<&str>,
    level_dbfs: f64,
) -> String {
    format!(
        "ARMED  →  OUT {}   {level_dbfs:+.1} dBFS  — Enter starts, Esc cancels",
        format_output_target(output_channel, output_port)
    )
}

/// The DRIVING banner (§5). Same verbatim contract as
/// [`format_armed_banner`].
pub fn format_driving_banner(
    output_channel: u32,
    output_port: Option<&str>,
    level_dbfs: f64,
) -> String {
    format!(
        "DRIVING   OUT {}   {level_dbfs:+.1} dBFS  — Space/Enter/Esc stops",
        format_output_target(output_channel, output_port)
    )
}

/// Which output leg a health line is about (#205). The label is fixed-width
/// by design so the state column aligns across faults.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DriveLeg {
    /// The main stimulus output.
    Out,
    /// The reference output, when it resolves to a different port.
    RefOut,
}

impl DriveLeg {
    fn label(self) -> &'static str {
        match self {
            DriveLeg::Out => "OUT",
            DriveLeg::RefOut => "REF OUT",
        }
    }
}

/// Observed state of one drive leg (#205). Closed set, three positions and no
/// others — the vocabulary shared with the data-link line (#193) so there is
/// one health grammar rather than two ad-hoc warnings.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DrivePathState {
    /// Observed to have an edge. Renders as **nothing at all** — the happy path
    /// costs zero pixels and the void stays void.
    Connected,
    /// Observed to have no edge. This is #203's condition: driving into nothing.
    NotConnected,
    /// The daemon cannot see its graph (`--fake-audio`, a pre-#205 daemon, a
    /// non-JACK backend). Lowercase, because an absence of information is not a
    /// fault and must never be mistaken for one.
    ///
    /// The `Default`, deliberately: a view that has not yet been told anything
    /// must say so, not assume health.
    #[default]
    Unknown,
    /// This session never opened the leg. Renders as nothing: a passive
    /// (`drivable = false`) session has no drive path to report on, and
    /// flagging it would be a false positive by construction.
    NotApplicable,
}

/// Maximum columns for a JACK port name inside a health line, derived from the
/// 80-column budget: fixed chrome is `␣␣drive path␣␣␣REF OUT 3 ()␣␣␣NOT
/// CONNECTED` = 43 columns, leaving 37.
const HEALTH_PORT_MAX_COLS: usize = 37;

/// Elide a too-long port name **from the left** (`…:playback_5`) — the suffix
/// is the discriminating part, so the head is what can be spared.
fn elide_port_from_left(port: &str) -> String {
    let chars: Vec<char> = port.chars().collect();
    if chars.len() <= HEALTH_PORT_MAX_COLS {
        return port.to_string();
    }
    let keep = HEALTH_PORT_MAX_COLS.saturating_sub(1);
    let tail: String = chars[chars.len() - keep..].iter().collect();
    format!("\u{2026}{tail}")
}

/// Map a wire `conn_tags` value to a display state (#205).
///
/// The one place the daemon's tag vocabulary becomes a display vocabulary. An
/// absent tag, and any value this build does not recognise, both become
/// [`DrivePathState::Unknown`] — never `Connected`. A forward-compatible
/// vocabulary must degrade to "I cannot tell you", not to "everything is fine".
pub fn drive_path_state_from_tag(tag: Option<&str>) -> DrivePathState {
    match tag {
        Some("on") => DrivePathState::Connected,
        Some("off") => DrivePathState::NotConnected,
        Some("none") => DrivePathState::NotApplicable,
        _ => DrivePathState::Unknown,
    }
}

/// One drive-path health line (#205), or `None` when there is nothing to say.
///
/// Grammar, shared with the data-link line so #193 needs no second vocabulary:
///
/// ```text
///   <link>   <the thing named, with real values>   <STATE>
/// ```
///
/// Rendering rules, and the reasoning that makes them load-bearing:
///
/// - **Verified good and not-applicable return `None`.** Silence is the
///   healthy rendering. A line that appeared on every session would be chrome,
///   and chrome is not read.
/// - **A verified fault is `CAPS`** — weight without colour, legible piped to
///   a file, and caps already mean "this is the state that matters" in this UI
///   (`ARMED`/`DRIVING`).
/// - **`unknown` is lowercase.** It distinguishes "we looked and there are no
///   edges" from "we cannot look". Without that distinction the whole design
///   regresses into a quieter version of the lie it exists to fix.
///
/// The leg token comes from the same private `format_output_target` both
/// banners call, so the token here and the token in the banner are identical
/// **by construction rather than by convention** — which is the failure that
/// produced `OUT 0` in the first place.
pub fn format_drive_path_health(
    leg: DriveLeg,
    output_channel: u32,
    output_port: Option<&str>,
    state: DrivePathState,
) -> Option<String> {
    let verdict = match state {
        DrivePathState::Connected | DrivePathState::NotApplicable => return None,
        DrivePathState::NotConnected => "NOT CONNECTED",
        DrivePathState::Unknown => "unknown",
    };
    let elided = output_port.map(elide_port_from_left);
    Some(format!(
        "  drive path   {} {}   {verdict}",
        leg.label(),
        format_output_target(output_channel, elided.as_deref()),
    ))
}

fn format_output_target(output_channel: u32, output_port: Option<&str>) -> String {
    match output_port {
        Some(port) => format!("{output_channel} ({port})"),
        None => format!("{output_channel}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // F5: byte-exact banners, both the with-port and no-port shapes.
    #[test]
    fn banner_strings_are_byte_exact() {
        assert_eq!(
            format_armed_banner(3, Some("Fireface400:AN3"), -20.0),
            "ARMED  →  OUT 3 (Fireface400:AN3)   -20.0 dBFS  — Enter starts, Esc cancels"
        );
        assert_eq!(
            format_driving_banner(3, Some("Fireface400:AN3"), -20.0),
            "DRIVING   OUT 3 (Fireface400:AN3)   -20.0 dBFS  — Space/Enter/Esc stops"
        );
        assert_eq!(
            format_armed_banner(0, None, -10.0),
            "ARMED  →  OUT 0   -10.0 dBFS  — Enter starts, Esc cancels"
        );
        assert_eq!(
            format_driving_banner(0, None, -10.0),
            "DRIVING   OUT 0   -10.0 dBFS  — Space/Enter/Esc stops"
        );
    }

    #[test]
    fn spl_readout_none_when_no_spl_cal() {
        assert_eq!(
            format_spl_readout(None, WeightingCurve::A, Some("fast")),
            None
        );
    }

    #[test]
    fn spl_readout_with_integration_tag() {
        assert_eq!(
            format_spl_readout(Some(72.3), WeightingCurve::A, Some("fast")),
            Some("72.30 dB SPL (A, fast)".to_string())
        );
    }

    #[test]
    fn spl_readout_without_integration_tag() {
        assert_eq!(
            format_spl_readout(Some(-6.75), WeightingCurve::Z, None),
            Some("-6.75 dB SPL (Z)".to_string())
        );
    }

    #[test]
    fn cursor_readout_labels() {
        assert_eq!(
            format_cursor_readout(1000.0, -6.75, false),
            "1000 Hz: -6.75 dBFS"
        );
        assert_eq!(
            format_cursor_readout(1000.0, -6.75, true),
            "1000 Hz: -6.75 dB SPL"
        );
    }

    // ---- #205 drive-path health line: byte-exact, F5 style ----

    /// The UX-specified line, verbatim, at the reference rig's real values.
    #[test]
    fn drive_path_fault_line_is_byte_exact() {
        assert_eq!(
            format_drive_path_health(
                DriveLeg::Out,
                4,
                Some("Babyface Pro Pro:playback_5"),
                DrivePathState::NotConnected
            )
            .unwrap(),
            "  drive path   OUT 4 (Babyface Pro Pro:playback_5)   NOT CONNECTED"
        );
    }

    #[test]
    fn ref_out_fault_line_is_byte_exact() {
        assert_eq!(
            format_drive_path_health(
                DriveLeg::RefOut,
                3,
                Some("Babyface Pro Pro:playback_4"),
                DrivePathState::NotConnected
            )
            .unwrap(),
            "  drive path   REF OUT 3 (Babyface Pro Pro:playback_4)   NOT CONNECTED"
        );
    }

    /// `unknown` is lowercase and must never read as a fault.
    #[test]
    fn unknown_line_is_lowercase_and_byte_exact() {
        let line = format_drive_path_health(
            DriveLeg::Out,
            4,
            Some("Babyface Pro Pro:playback_5"),
            DrivePathState::Unknown,
        )
        .unwrap();
        assert_eq!(
            line,
            "  drive path   OUT 4 (Babyface Pro Pro:playback_5)   unknown"
        );
        assert!(
            !line.contains("NOT CONNECTED") && !line.contains("UNKNOWN"),
            "unobservable must not be rendered in the fault register: {line}"
        );
    }

    /// Verified-good and not-applicable are **silent**. The happy path and the
    /// passive session both cost zero pixels.
    #[test]
    fn healthy_and_passive_render_nothing() {
        assert_eq!(
            format_drive_path_health(DriveLeg::Out, 4, Some("x:y"), DrivePathState::Connected),
            None,
            "a connected leg must emit no line"
        );
        assert_eq!(
            format_drive_path_health(DriveLeg::Out, 4, None, DrivePathState::NotApplicable),
            None,
            "a passive session has no drive path to report on"
        );
    }

    /// The health line's leg token is produced by the same formatter the
    /// banners use. Asserted as a substring relationship rather than by
    /// duplicating the format string, so the two cannot drift apart — which is
    /// the failure that produced `OUT 0`.
    #[test]
    fn health_line_and_banner_share_the_output_token() {
        let (ch, port) = (4u32, "Babyface Pro Pro:playback_5");
        let token = format!("OUT {}", format_output_target(ch, Some(port)));

        let health =
            format_drive_path_health(DriveLeg::Out, ch, Some(port), DrivePathState::NotConnected)
                .unwrap();
        let armed = format_armed_banner(ch, Some(port), -40.0);
        let driving = format_driving_banner(ch, Some(port), -40.0);

        for (name, s) in [
            ("health", &health),
            ("armed", &armed),
            ("driving", &driving),
        ] {
            assert!(
                s.contains(&token),
                "{name} line must carry the shared token {token:?}, got: {s}"
            );
        }
    }

    /// Over-long port names elide from the left, keeping the discriminating
    /// suffix, and the line stays inside 80 columns.
    #[test]
    fn long_port_names_elide_from_the_left_and_fit_80_columns() {
        let long = "some-absurdly-long-jack-client-name-here:playback_5";
        assert!(long.len() > HEALTH_PORT_MAX_COLS);
        let line = format_drive_path_health(
            DriveLeg::RefOut,
            3,
            Some(long),
            DrivePathState::NotConnected,
        )
        .unwrap();
        assert!(
            line.contains("\u{2026}") && line.contains(":playback_5"),
            "must elide from the left and keep the suffix: {line}"
        );
        assert!(
            line.chars().count() <= 80,
            "health line must fit 80 columns, got {}: {line}",
            line.chars().count()
        );
    }

    /// The worst case the UX design counted: longest label, longest permitted
    /// port name.
    #[test]
    fn widest_legal_line_fits_80_columns() {
        let port = "x".repeat(HEALTH_PORT_MAX_COLS);
        let line = format_drive_path_health(
            DriveLeg::RefOut,
            3,
            Some(&port),
            DrivePathState::NotConnected,
        )
        .unwrap();
        assert!(
            line.chars().count() <= 80,
            "worst case must fit 80 columns, got {}: {line}",
            line.chars().count()
        );
    }
}
