//! Which analysis the monitor worker runs this tick, and which modes set
//! their own cadence.

/// Which analysis the monitor worker runs this tick, parsed once from
/// the shared `analysis_mode` string.
///
/// Everything but `Fft` is ring-buffered: a short self-paced capture per
/// tick fed into a sliding ring. Only `Fft` sleeps to the requested
/// interval at the end of a tick — which is why this is an enum and not
/// three booleans. The old `if !is_cwt && !is_cqt && !is_reassigned`
/// pacing guard had to be updated by hand whenever a mode was added, and
/// silently mis-paced the new mode if it was not.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Fft,
    Cwt,
    Cqt,
    Reassigned,
}

impl Mode {
    /// An unrecognised tag falls back to the FFT path, matching the
    /// previous behaviour where all three `is_*` flags were false.
    pub(super) fn from_tag(tag: &str) -> Self {
        match tag {
            "cwt" => Self::Cwt,
            "cqt" => Self::Cqt,
            "reassigned" => Self::Reassigned,
            _ => Self::Fft,
        }
    }

    /// True when the mode sets its own cadence from `TickCtx::tick_secs`
    /// and must not also take the end-of-tick interval sleep.
    pub(super) fn paces_itself(self) -> bool {
        self != Self::Fft
    }
}

#[cfg(test)]
mod mode_tests {
    use super::Mode;

    /// The pacing guard used to be `!is_cwt && !is_cqt && !is_reassigned`,
    /// which had to be edited by hand for every new mode and silently
    /// double-paced one that was forgotten. Assert the property directly:
    /// exactly the ring-buffered modes pace themselves, and `Fft` does not.
    #[test]
    fn only_the_fft_mode_takes_the_interval_sleep() {
        assert!(!Mode::Fft.paces_itself());
        for m in [Mode::Cwt, Mode::Cqt, Mode::Reassigned] {
            assert!(m.paces_itself());
        }
    }

    /// Every tag the daemon publishes must round-trip, and anything else
    /// must land on the FFT path rather than on whichever variant happens
    /// to be first.
    #[test]
    fn unknown_tags_fall_back_to_fft() {
        assert!(Mode::from_tag("cwt") == Mode::Cwt);
        assert!(Mode::from_tag("cqt") == Mode::Cqt);
        assert!(Mode::from_tag("reassigned") == Mode::Reassigned);
        for tag in ["fft", "", "CWT", "spectrum", "reassign"] {
            assert!(
                Mode::from_tag(tag) == Mode::Fft,
                "{tag:?} should fall back to Fft"
            );
        }
    }
}
