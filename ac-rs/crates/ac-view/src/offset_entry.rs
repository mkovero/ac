//! The typed trace-offset entry (`J` in the transfer view): Smaart's trace
//! offset, in dB, for the selected trace. Same split as `delay_entry.rs`:
//! pure editable state, so what a keypress means is a unit test.
//!
//! `J`, not Enter, applies, for the reason `T` does in the delay entry:
//! Enter belongs to the stimulus while it is armed.

/// Longest accepted text, sign and point included.
const MAX_LEN: usize = 6;

/// Largest offset accepted, dB either way. The magnitude pane spans 100 dB,
/// so a larger offset only moves the trace off screen.
pub const MAX_OFFSET_DB: f64 = 60.0;

/// The text being typed, in dB.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OffsetEntry {
    text: String,
}

impl OffsetEntry {
    /// Digits append; `-` only first; one `.` (or `,`, as a Finnish keyboard
    /// types it); anything else is ignored.
    pub fn push(&mut self, c: char) {
        if self.text.len() >= MAX_LEN {
            return;
        }
        match c {
            '0'..='9' => self.text.push(c),
            '-' if self.text.is_empty() => self.text.push(c),
            '.' | ',' if !self.text.contains('.') => self.text.push('.'),
            _ => {}
        }
    }

    pub fn backspace(&mut self) {
        self.text.pop();
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// What applying does: `Some(0.0)` for an empty entry (the offset goes
    /// back to none), the typed value when it parses within
    /// ±[`MAX_OFFSET_DB`], `None` otherwise (nothing to apply).
    pub fn value(&self) -> Option<f64> {
        if self.text.is_empty() {
            return Some(0.0);
        }
        self.text
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && v.abs() <= MAX_OFFSET_DB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> OffsetEntry {
        let mut e = OffsetEntry::default();
        s.chars().for_each(|c| e.push(c));
        e
    }

    #[test]
    fn decimals_signs_and_a_finnish_comma() {
        assert_eq!(typed("3").value(), Some(3.0));
        assert_eq!(typed("-6.5").value(), Some(-6.5));
        assert_eq!(typed("2,5").value(), Some(2.5));
        assert_eq!(typed("1.2.3").text(), "1.23");
        assert_eq!(typed("4-x").text(), "4");
    }

    #[test]
    fn empty_resets_and_nonsense_or_out_of_range_applies_nothing() {
        assert_eq!(typed("").value(), Some(0.0));
        assert_eq!(typed("-").value(), None);
        assert_eq!(typed(".").value(), None);
        assert_eq!(typed("61").value(), None);
        assert_eq!(typed("-60").value(), Some(-60.0));
    }
}
