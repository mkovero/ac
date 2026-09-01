//! Advance widths for the PDF core fonts.
//!
//! # A wrap budget must be measured in the font that draws it
//!
//! `printpdf` names the 14 core fonts rather than embedding them and
//! exposes no metrics for them, so a renderer that needs to know how
//! wide a line will be has to carry its own table. Without one,
//! [`super::cursor::Cursor::note`] sized its lines by Courier's fixed
//! 0.6 em advance and then drew them in Helvetica. Ordinary prose
//! averages narrower than that and wrapped short; a run of capitals,
//! digits or hex — an operator pasting a session id, an all-caps
//! warning — averages wider and ran past the right margin. `printpdf`
//! places off-page content without complaint, so that overflowed
//! invisibly: the same failure as the millimetre/point confusion
//! [`super::cursor::PT_MM`] fixes, one axis over.
//!
//! [`Face`] is therefore the unit of both measuring and drawing: a
//! caller picks one, and [`super::cursor::Cursor`] wraps and paints
//! through the same value. The two can no longer disagree.
//!
//! Widths are Adobe's own, taken from the Helvetica, Helvetica-Bold
//! and Courier AFMs, in 1/1000 em. Only the WinAnsi-encodable range is
//! tabulated, which is all `cursor::encode` can produce.

/// A core font, as both a metric and a thing to draw with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Face {
    /// Helvetica — section prose.
    Regular,
    /// Helvetica-Bold — titles, headings, labels, table headers.
    Bold,
    /// Courier — values and table cells, where columns must line up.
    Mono,
}

/// Courier advances 0.6 em for every character, without exception.
const COURIER: u16 = 600;

impl Face {
    /// Advance width of `ch`, in em.
    fn advance_em(self, ch: char) -> f32 {
        f32::from(self.advance_1000(ch)) / 1000.0
    }

    /// Advance width of `ch`, in 1/1000 em, as the AFMs state it.
    ///
    /// A character outside the tables is charged the face's widest
    /// glyph. Only U+20AC reaches that path — the 1990 AFMs predate
    /// it — and over-charging can only wrap a line early, never let
    /// one overrun.
    fn advance_1000(self, ch: char) -> u16 {
        let (ascii, latin1, cp1252, max) = match self {
            Face::Mono => return COURIER,
            Face::Regular => (
                &REGULAR_ASCII,
                &REGULAR_LATIN1,
                &REGULAR_CP1252,
                REGULAR_MAX,
            ),
            Face::Bold => (&BOLD_ASCII, &BOLD_LATIN1, &BOLD_CP1252, BOLD_MAX),
        };
        match ch {
            ' '..='~' => ascii[ch as usize - 0x20],
            '\u{a0}'..='\u{ff}' => latin1[ch as usize - 0xa0],
            _ => cp1252
                .iter()
                .find(|(c, _)| *c == ch)
                .map_or(max, |(_, w)| *w),
        }
    }
}

/// Width of `text` set in `face` at `size_pt`, in millimetres.
pub(super) fn text_mm(text: &str, size_pt: f32, face: Face) -> f32 {
    let em: f32 = text.chars().map(|c| face.advance_em(c)).sum();
    em * size_pt * super::cursor::PT_MM
}

/// The longest prefix of `word` that fits `width_mm`, as a byte index.
///
/// Never zero: a box too narrow for even one glyph still takes that
/// glyph, so a caller splitting a long word always makes progress.
pub(super) fn fit_prefix(word: &str, width_mm: f32, size_pt: f32, face: Face) -> usize {
    let mut used = 0.0;
    let mut end = 0;
    for (i, ch) in word.char_indices() {
        let w = text_mm(ch.encode_utf8(&mut [0u8; 4]), size_pt, face);
        if end > 0 && used + w > width_mm {
            return end;
        }
        used += w;
        end = i + ch.len_utf8();
    }
    end
}

// The two codepoints below that the AFMs do not list are filled with
// the glyph WinAnsiEncoding draws for them (PDF 32000-1 Annex D.2):
// U+00A0 is set as a space, U+00AD as a hyphen.

/// Helvetica, U+0020..=U+007E.
const REGULAR_ASCII: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

/// Helvetica, U+00A0..=U+00FF.
const REGULAR_LATIN1: [u16; 96] = [
    278, 333, 556, 556, 556, 556, 260, 556, 333, 737, 370, 556, 584, 333, 737, 333, 400, 584, 333,
    333, 333, 556, 537, 278, 333, 333, 365, 556, 834, 834, 834, 611, 667, 667, 667, 667, 667, 667,
    1000, 722, 667, 667, 667, 667, 278, 278, 278, 278, 722, 722, 778, 778, 778, 778, 778, 584, 778,
    722, 722, 722, 722, 667, 667, 611, 556, 556, 556, 556, 556, 556, 889, 500, 556, 556, 556, 556,
    278, 278, 278, 278, 556, 556, 556, 556, 556, 556, 556, 584, 611, 556, 556, 556, 556, 500, 556,
    500,
];

/// Helvetica, the CP1252 characters in U+0080..U+009F.
const REGULAR_CP1252: [(char, u16); 26] = [
    ('\u{201a}', 222),
    ('\u{192}', 556),
    ('\u{201e}', 333),
    ('\u{2026}', 1000),
    ('\u{2020}', 556),
    ('\u{2021}', 556),
    ('\u{2c6}', 333),
    ('\u{2030}', 1000),
    ('\u{160}', 667),
    ('\u{2039}', 333),
    ('\u{152}', 1000),
    ('\u{17d}', 611),
    ('\u{2018}', 222),
    ('\u{2019}', 222),
    ('\u{201c}', 333),
    ('\u{201d}', 333),
    ('\u{2022}', 350),
    ('\u{2013}', 556),
    ('\u{2014}', 1000),
    ('\u{2dc}', 333),
    ('\u{2122}', 1000),
    ('\u{161}', 500),
    ('\u{203a}', 333),
    ('\u{153}', 944),
    ('\u{17e}', 500),
    ('\u{178}', 667),
];

/// The widest glyph in Helvetica.
const REGULAR_MAX: u16 = 1015;

/// Helvetica-Bold, U+0020..=U+007E.
const BOLD_ASCII: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667,
    611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 333, 278, 333, 584, 556, 333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556,
    278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
];

/// Helvetica-Bold, U+00A0..=U+00FF.
const BOLD_LATIN1: [u16; 96] = [
    278, 333, 556, 556, 556, 556, 280, 556, 333, 737, 370, 556, 584, 333, 737, 333, 400, 584, 333,
    333, 333, 611, 556, 278, 333, 333, 365, 556, 834, 834, 834, 611, 722, 722, 722, 722, 722, 722,
    1000, 722, 667, 667, 667, 667, 278, 278, 278, 278, 722, 722, 778, 778, 778, 778, 778, 584, 778,
    722, 722, 722, 722, 667, 667, 611, 556, 556, 556, 556, 556, 556, 889, 556, 556, 556, 556, 556,
    278, 278, 278, 278, 611, 611, 611, 611, 611, 611, 611, 584, 611, 611, 611, 611, 611, 556, 611,
    556,
];

/// Helvetica-Bold, the CP1252 characters in U+0080..U+009F.
const BOLD_CP1252: [(char, u16); 26] = [
    ('\u{201a}', 278),
    ('\u{192}', 556),
    ('\u{201e}', 500),
    ('\u{2026}', 1000),
    ('\u{2020}', 556),
    ('\u{2021}', 556),
    ('\u{2c6}', 333),
    ('\u{2030}', 1000),
    ('\u{160}', 667),
    ('\u{2039}', 333),
    ('\u{152}', 1000),
    ('\u{17d}', 611),
    ('\u{2018}', 278),
    ('\u{2019}', 278),
    ('\u{201c}', 500),
    ('\u{201d}', 500),
    ('\u{2022}', 350),
    ('\u{2013}', 556),
    ('\u{2014}', 1000),
    ('\u{2dc}', 333),
    ('\u{2122}', 1000),
    ('\u{161}', 556),
    ('\u{203a}', 333),
    ('\u{153}', 944),
    ('\u{17e}', 500),
    ('\u{178}', 667),
];

/// The widest glyph in Helvetica-Bold.
const BOLD_MAX: u16 = 1000;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::report_pdf::cursor::SIZE_BODY;

    #[test]
    fn courier_is_the_only_face_a_character_count_can_measure() {
        // The premise of the bug: a character budget is exact in
        // Courier and wrong in Helvetica, in both directions. `note`
        // borrowed the Courier budget to wrap Helvetica, so wide
        // characters overran the box they were wrapped into.
        for ch in ['A', 'W', 'M', '0', '%'] {
            assert!(
                (Face::Mono.advance_em(ch) - 0.6).abs() < 1e-6,
                "{ch:?} is not 0.6 em in Courier"
            );
        }
        assert!(
            Face::Regular.advance_em('W') > 0.6,
            "cap W must be wider than the Courier budget, or there was no bug"
        );
        assert!(
            Face::Regular.advance_em('i') < 0.6,
            "narrow prose must be narrower, or the budget would be safe by luck"
        );
    }

    #[test]
    fn a_line_of_capitals_outgrows_the_courier_budget_for_the_same_text() {
        // Concretely, on the full-width note box: the character count
        // Courier allows does not fit in Helvetica.
        let width_mm = 180.0;
        let chars = (width_mm / text_mm("W", SIZE_BODY, Face::Mono)).floor() as usize;
        let line = "W".repeat(chars);
        assert!(
            text_mm(&line, SIZE_BODY, Face::Regular) > width_mm,
            "{chars} capitals fit {width_mm} mm in both faces; the metric mismatch is unmeasurable"
        );
    }

    #[test]
    fn every_tabulated_width_is_a_plausible_advance() {
        // A zero would silently let a line grow without bound; nothing
        // in these faces is wider than an em and a half.
        for face in [Face::Regular, Face::Bold] {
            for ch in (0x20u32..=0x7e).chain(0xa0..=0xff) {
                let ch = char::from_u32(ch).expect("latin-1 is valid utf-8");
                let w = face.advance_1000(ch);
                assert!((150..=1500).contains(&w), "{face:?} {ch:?} = {w}/1000 em");
            }
        }
    }

    #[test]
    fn an_untabulated_character_is_charged_the_widest_glyph() {
        // Wrapping may only ever over-charge: a character nobody
        // measured must not be free.
        assert_eq!(Face::Regular.advance_1000('\u{20ac}'), REGULAR_MAX);
        assert_eq!(Face::Bold.advance_1000('\u{20ac}'), BOLD_MAX);
        for face in [Face::Regular, Face::Bold] {
            for ch in (0x20u32..=0x7e).chain(0xa0..=0xff) {
                let ch = char::from_u32(ch).expect("latin-1 is valid utf-8");
                assert!(face.advance_1000(ch) <= face.advance_1000('\u{20ac}'));
            }
        }
    }

    #[test]
    fn fit_prefix_always_advances() {
        // A box narrower than one glyph must still consume one, or the
        // hard-split loop in `wrap` never terminates.
        assert_eq!(fit_prefix("Wide", 0.0, SIZE_BODY, Face::Regular), 1);
        assert_eq!(fit_prefix("\u{b0}C", 0.0, SIZE_BODY, Face::Regular), 2);
        assert_eq!(fit_prefix("", 100.0, SIZE_BODY, Face::Regular), 0);
    }

    #[test]
    fn fit_prefix_stops_at_the_width_it_was_given() {
        let width = text_mm("MMM", SIZE_BODY, Face::Regular);
        let end = fit_prefix("MMMMMM", width, SIZE_BODY, Face::Regular);
        assert_eq!(end, 3, "took {end} Ms where 3 fit");
    }

    #[test]
    fn measuring_is_additive() {
        // `wrap` sizes a candidate line as label + space + word rather
        // than re-measuring the concatenation.
        let joined = text_mm("session id", SIZE_BODY, Face::Regular);
        let parts = text_mm("session", SIZE_BODY, Face::Regular)
            + text_mm(" ", SIZE_BODY, Face::Regular)
            + text_mm("id", SIZE_BODY, Face::Regular);
        assert!((joined - parts).abs() < 1e-4, "{joined} vs {parts}");
    }
}
