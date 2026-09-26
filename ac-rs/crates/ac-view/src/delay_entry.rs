//! The typed-delay entry (#669, `T` in the transfer view).
//!
//! Pure editable state, so what a keypress means is a unit test rather than
//! a click-through — the same split `settings.rs` uses. The app routes
//! digits, `-` and Backspace here while the entry is open, and `T` applies.
//!
//! `T`, not Enter, applies: Enter and Esc are the stimulus panic keys, and
//! [`crate::app`]'s panic-first dispatch consumes them whenever the drive is
//! live. An entry that committed on Enter would stop the measurement the
//! operator is aligning.

/// Longest accepted text, sign included. Nine digits is 10⁹ samples —
/// hours at any rate this daemon runs — so nothing typed can overflow `i64`.
const MAX_LEN: usize = 10;

/// The text being typed, in samples.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DelayEntry {
    text: String,
}

impl DelayEntry {
    /// One typed character. Digits append; `-` is accepted only first;
    /// anything else is ignored.
    pub fn push(&mut self, c: char) {
        if self.text.len() >= MAX_LEN {
            return;
        }
        match c {
            '0'..='9' => self.text.push(c),
            '-' if self.text.is_empty() => self.text.push(c),
            _ => {}
        }
    }

    pub fn backspace(&mut self) {
        self.text.pop();
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The typed value, or `None` when there is nothing to apply (empty, or
    /// a lone `-`) — which the app reads as cancel.
    pub fn value(&self) -> Option<i64> {
        self.text.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> DelayEntry {
        let mut e = DelayEntry::default();
        s.chars().for_each(|c| e.push(c));
        e
    }

    #[test]
    fn digits_and_a_leading_minus_make_a_value() {
        assert_eq!(typed("400").value(), Some(400));
        assert_eq!(typed("-12").value(), Some(-12));
        assert_eq!(typed("0").value(), Some(0));
    }

    #[test]
    fn a_minus_after_a_digit_and_other_characters_are_ignored() {
        assert_eq!(typed("4-0a0 .5").text(), "4005");
    }

    #[test]
    fn nothing_to_apply_is_none() {
        assert_eq!(typed("").value(), None);
        assert_eq!(typed("-").value(), None);
    }

    #[test]
    fn backspace_removes_the_last_character() {
        let mut e = typed("401");
        e.backspace();
        assert_eq!(e.value(), Some(40));
        e.backspace();
        e.backspace();
        e.backspace();
        assert_eq!(e.value(), None);
    }

    #[test]
    fn length_is_capped_so_no_typed_value_overflows() {
        let e = typed(&"9".repeat(30));
        assert_eq!(e.text().len(), MAX_LEN);
        assert!(e.value().is_some());
    }
}
