//! Page geometry, the text cursor, and pagination.
//!
//! # Points are not millimetres
//!
//! `printpdf` takes font sizes in **points** and positions in
//! **millimetres**. The original renderer subtracted point-valued font
//! sizes straight from a millimetre cursor — advancing 9.5 mm for a
//! 3.35 mm line, roughly three times too far. A full report ran off the
//! bottom of its single page: with a calibration snapshot and a
//! position captured, everything from *Environment & Geometry* onward
//! landed at a negative `Mm`, and the frequency-response table drew
//! zero rows under a "… N more rows" note because its row budget came
//! out negative. `printpdf` places off-page content without complaint,
//! so nothing failed — the output was simply missing.
//!
//! Every advance below is therefore a millimetre value derived from a
//! point size through [`PT_MM`], and every one of them goes through
//! [`Cursor`], which starts a new page rather than writing past the
//! bottom margin.

use printpdf::{
    Color, IndirectFontRef, Line, Mm, PdfDocumentReference, PdfLayerReference, Point, Rgb,
};

use super::metrics::{fit_prefix, text_mm, Face};
use crate::measurement::report_layout::Column;

// Page geometry — A4 portrait, millimetres.
pub(super) const PAGE_W_MM: f32 = 210.0;
pub(super) const PAGE_H_MM: f32 = 297.0;
pub(super) const MARGIN_MM: f32 = 15.0;

// Type sizes — points, as `printpdf` wants them.
const SIZE_TITLE: f32 = 18.0;
const SIZE_H2: f32 = 13.0;
pub(super) const SIZE_BODY: f32 = 9.5;
pub(super) const SIZE_SMALL: f32 = 8.0;

/// One typographic point in millimetres.
pub(super) const PT_MM: f32 = 25.4 / 72.0;

const TITLE_MM: f32 = SIZE_TITLE * PT_MM;
const H2_MM: f32 = SIZE_H2 * PT_MM;
pub(super) const BODY_MM: f32 = SIZE_BODY * PT_MM;
pub(super) const SMALL_MM: f32 = SIZE_SMALL * PT_MM;

/// Baseline-to-baseline distance for body text.
pub(super) const ROW_MM: f32 = BODY_MM * 1.45;

/// Left edge of the value column in a key/value row.
const VALUE_X_MM: f32 = MARGIN_MM + 34.0;

/// How many wrapped value lines stay on the same page as their label.
const KEEP_WITH_LABEL: usize = 3;

pub(super) struct Fonts {
    pub regular: IndirectFontRef,
    pub bold: IndirectFontRef,
    pub mono: IndirectFontRef,
}

/// A y-cursor over a growing sequence of pages. Text is placed by its
/// baseline; the cursor tracks the *top* of the next line.
pub(super) struct Cursor<'a> {
    doc: &'a PdfDocumentReference,
    fonts: &'a Fonts,
    layer: PdfLayerReference,
    y: f32,
}

impl<'a> Cursor<'a> {
    pub(super) fn new(
        doc: &'a PdfDocumentReference,
        fonts: &'a Fonts,
        layer: PdfLayerReference,
    ) -> Self {
        Self {
            doc,
            fonts,
            layer,
            y: PAGE_H_MM - MARGIN_MM,
        }
    }

    /// Draw a run at an absolute page position, for callers that place
    /// by geometry rather than by the cursor — the plot's tick labels.
    ///
    /// `x_mm` is the left edge and `y_mm` the baseline. Use
    /// [`super::metrics::text_mm`] to know how wide the run will be:
    /// nothing here clamps it to the page.
    pub(super) fn text_at(&self, text: &str, size_pt: f32, x_mm: f32, y_mm: f32, face: Face) {
        self.layer
            .use_text(encode(text), size_pt, Mm(x_mm), Mm(y_mm), self.font(face));
    }

    pub(super) fn y(&self) -> f32 {
        self.y
    }

    /// Move down, without a page break — callers that need one ask for
    /// it explicitly through [`Cursor::ensure`].
    pub(super) fn advance(&mut self, mm: f32) {
        self.y -= mm;
    }

    /// True when `need_mm` more would cross the bottom margin.
    pub(super) fn would_overflow(&self, need_mm: f32) -> bool {
        self.y - need_mm < MARGIN_MM
    }

    /// Guarantee `need_mm` of room below the cursor, starting a page if
    /// there is not.
    pub(super) fn ensure(&mut self, need_mm: f32) {
        if self.would_overflow(need_mm) {
            self.new_page();
        }
    }

    pub(super) fn new_page(&mut self) {
        let (page, layer) = self.doc.add_page(Mm(PAGE_W_MM), Mm(PAGE_H_MM), "Layer 1");
        self.layer = self.doc.get_page(page).get_layer(layer);
        self.y = PAGE_H_MM - MARGIN_MM;
    }

    // ----- text -----

    fn font(&self, face: Face) -> &IndirectFontRef {
        match face {
            Face::Regular => &self.fonts.regular,
            Face::Bold => &self.fonts.bold,
            Face::Mono => &self.fonts.mono,
        }
    }

    /// Draw one run. `face` is the same value the text was wrapped
    /// with, so a line can never be measured in one font and set in
    /// another.
    fn place(&self, text: &str, size_pt: f32, x_mm: f32, glyph_mm: f32, face: Face) {
        self.layer.use_text(
            encode(text),
            size_pt,
            Mm(x_mm),
            Mm(self.y - glyph_mm),
            self.font(face),
        );
    }

    pub(super) fn title(&mut self, text: &str) {
        self.ensure(TITLE_MM + 4.0);
        self.place(text, SIZE_TITLE, MARGIN_MM, TITLE_MM, Face::Bold);
        self.advance(TITLE_MM + 1.5);
        self.hline(self.y, MARGIN_MM, PAGE_W_MM - MARGIN_MM, 0.7);
        self.advance(2.5);
    }

    pub(super) fn heading(&mut self, text: &str) {
        // Keep a heading with at least one line of what follows it.
        self.ensure(H2_MM + 3.0 + ROW_MM);
        self.advance(2.5);
        self.place(text, SIZE_H2, MARGIN_MM, H2_MM, Face::Bold);
        self.advance(H2_MM + 1.5);
    }

    /// Key/value line. A long value wraps under itself rather than
    /// running off the right edge.
    ///
    /// The label is kept with up to [`KEEP_WITH_LABEL`] lines of its
    /// value; a value longer than that may continue on the next page,
    /// which is still better than one that runs off the current one.
    pub(super) fn kv(&mut self, label: &str, value: &str) {
        let width_mm = PAGE_W_MM - MARGIN_MM - VALUE_X_MM;
        let lines = wrap(&encode(value), width_mm, SIZE_BODY, Face::Mono);
        let kept = lines.len().min(KEEP_WITH_LABEL) as f32;
        self.ensure(ROW_MM * kept);
        self.place(label, SIZE_BODY, MARGIN_MM, BODY_MM, Face::Bold);
        for line in &lines {
            // Guard every line, not just the block: a value too long
            // for any page must still stop at the bottom margin.
            self.ensure(ROW_MM);
            self.place(line, SIZE_BODY, VALUE_X_MM, BODY_MM, Face::Mono);
            self.advance(ROW_MM);
        }
    }

    /// A full-width sentence, as a section body or a footnote.
    pub(super) fn note(&mut self, text: &str) {
        let width_mm = PAGE_W_MM - 2.0 * MARGIN_MM;
        for line in wrap(&encode(text), width_mm, SIZE_BODY, Face::Regular) {
            self.ensure(ROW_MM);
            self.place(&line, SIZE_BODY, MARGIN_MM, BODY_MM, Face::Regular);
            self.advance(ROW_MM);
        }
    }

    // ----- tables -----

    pub(super) fn table_header(&mut self, columns: &[Column]) {
        self.ensure(ROW_MM * 2.0);
        let mut x = MARGIN_MM;
        for c in columns {
            self.place(c.plain, SIZE_BODY, x, BODY_MM, Face::Bold);
            x += c.width_mm;
        }
        self.advance(BODY_MM + 0.8);
        self.hline(self.y, MARGIN_MM, PAGE_W_MM - MARGIN_MM, 0.3);
        self.advance(1.2);
    }

    /// A table, repeating its header on every page it spans.
    ///
    /// Every row is drawn. The previous renderer capped rows to what it
    /// believed fit on one page and replaced the rest with an ellipsis
    /// note; with a page to grow into there is nothing to elide, and a
    /// report that silently drops measured values is worse than a long
    /// one.
    pub(super) fn table(&mut self, columns: &[Column], rows: &[Vec<String>]) {
        self.table_header(columns);
        for row in rows {
            if self.would_overflow(ROW_MM) {
                self.new_page();
                self.table_header(columns);
            }
            let mut x = MARGIN_MM;
            for (cell, c) in row.iter().zip(columns) {
                self.place(cell, SIZE_BODY, x, BODY_MM, Face::Mono);
                x += c.width_mm;
            }
            self.advance(ROW_MM);
        }
    }

    // ----- rules -----

    pub(super) fn hline(&self, y_mm: f32, x0_mm: f32, x1_mm: f32, thickness: f32) {
        self.stroke(
            thickness,
            vec![
                (Point::new(Mm(x0_mm), Mm(y_mm)), false),
                (Point::new(Mm(x1_mm), Mm(y_mm)), false),
            ],
            false,
        );
    }

    pub(super) fn vline(&self, x_mm: f32, y0_mm: f32, y1_mm: f32, thickness: f32) {
        self.stroke(
            thickness,
            vec![
                (Point::new(Mm(x_mm), Mm(y0_mm)), false),
                (Point::new(Mm(x_mm), Mm(y1_mm)), false),
            ],
            false,
        );
    }

    pub(super) fn rect(&self, x0: f32, y0: f32, x1: f32, y1: f32, thickness: f32) {
        self.stroke(
            thickness,
            vec![
                (Point::new(Mm(x0), Mm(y0)), false),
                (Point::new(Mm(x1), Mm(y0)), false),
                (Point::new(Mm(x1), Mm(y1)), false),
                (Point::new(Mm(x0), Mm(y1)), false),
            ],
            true,
        );
    }

    /// Stroke a polyline in the trace colour, restoring the default
    /// hairline black afterwards so later rules are unaffected.
    pub(super) fn trace(&self, points: Vec<(Point, bool)>) {
        if points.len() < 2 {
            return;
        }
        self.layer
            .set_outline_color(Color::Rgb(Rgb::new(0.12, 0.47, 0.71, None)));
        self.stroke(0.6, points, false);
        self.layer
            .set_outline_color(Color::Rgb(Rgb::new(0.0, 0.0, 0.0, None)));
        self.layer.set_outline_thickness(0.3);
    }

    fn stroke(&self, thickness: f32, points: Vec<(Point, bool)>, is_closed: bool) {
        self.layer.set_outline_thickness(thickness);
        self.layer.add_line(Line { points, is_closed });
    }
}

/// True for characters the 14 core PDF fonts can encode.
///
/// They are WinAnsi-encoded: printable ASCII, the Latin-1 supplement,
/// and the handful of typographic characters CP1252 puts in `0x80..0x9F`.
fn is_winansi(ch: char) -> bool {
    matches!(ch, ' '..='~') || matches!(ch, '\u{a0}'..='\u{ff}') || CP1252_HIGH.contains(ch)
}

/// The typographic characters WinAnsi puts in `0x80..=0x9F`, where
/// Latin-1 has control characters. Membership only — see
/// [`WINANSI_HIGH`] for the byte each one sits at.
const CP1252_HIGH: &str = "\u{20ac}\u{201a}\u{192}\u{201e}\u{2026}\u{2020}\u{2021}\u{2c6}\u{2030}\u{160}\u{2039}\u{152}\u{17d}\u{2018}\u{2019}\u{201c}\u{201d}\u{2022}\u{2013}\u{2014}\u{2dc}\u{2122}\u{161}\u{203a}\u{153}\u{17e}\u{178}";

/// The same 27 characters at the byte each sits at, with `\u{fffd}`
/// in the five codes WinAnsi leaves unassigned. Decoding only; nothing
/// consults this to decide what is encodable.
#[cfg(test)]
const WINANSI_HIGH: [char; 32] = [
    '\u{20ac}', '\u{fffd}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{fffd}', '\u{17d}', '\u{fffd}',
    '\u{fffd}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{fffd}', '\u{17e}', '\u{178}',
];

/// The character a WinAnsi byte stands for — the inverse of what
/// `use_text` writes into a content stream, so a test can read a
/// placed run back and measure how wide it was drawn.
///
/// `0x80..=0x9F` is the only range where WinAnsi and Latin-1 disagree.
#[cfg(test)]
pub(super) fn winansi_char(byte: u8) -> char {
    match byte {
        0x80..=0x9f => WINANSI_HIGH[byte as usize - 0x80],
        _ => byte as char,
    }
}

/// Replace characters the core fonts cannot encode.
///
/// `printpdf` drops them silently, so `20 Hz \u{2192} 20 kHz` printed as
/// `20 Hz 20 kHz` — two numbers with no stated relation between them.
/// An ASCII stand-in is worse typography and better data; anything
/// without one becomes `?`, which at least shows a reader that
/// something was there.
fn encode(text: &str) -> String {
    if text.chars().all(is_winansi) {
        return text.to_string();
    }
    text.chars()
        .map(|c| match c {
            c if is_winansi(c) => c.to_string(),
            '\u{2192}' => "->".into(), // rightwards arrow
            '\u{2190}' => "<-".into(), // leftwards arrow
            '\u{2713}' => "*".into(),  // check mark
            '\u{2264}' => "<=".into(),
            '\u{2265}' => ">=".into(),
            '\u{2260}' => "!=".into(),
            _ => "?".into(),
        })
        .collect()
}

/// Greedy word wrap, measured in `face` at `size_pt`.
///
/// A word wider than the whole line is hard-split rather than allowed
/// to overrun — a filesystem path has no spaces to break at and would
/// otherwise run off the page.
///
/// The face is a parameter because the caller draws with it too: the
/// budget and the glyphs it pays for come from one font. Sizing a line
/// in Courier and setting it in Helvetica is how notes used to overrun
/// the right margin.
fn wrap(text: &str, width_mm: f32, size_pt: f32, face: Face) -> Vec<String> {
    let width = |s: &str| text_mm(s, size_pt, face);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut word = word;
        while width(word) > width_mm {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            // Always at least one character, so this terminates.
            let split = fit_prefix(word, width_mm, size_pt, face);
            lines.push(word[..split].to_string());
            word = &word[split..];
        }
        let need = if line.is_empty() {
            width(word)
        } else {
            width(&line) + width(" ") + width(word)
        };
        if need > width_mm && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_is_not_a_millimetre() {
        // The bug this module exists to prevent: 9.5 pt is 3.35 mm, not
        // 9.5 mm. If these ever coincide, the conversion was dropped.
        const {
            assert!(BODY_MM < SIZE_BODY / 2.0);
            // 9.5 pt of type must not cost 5.5 mm of page.
            assert!(ROW_MM < 5.5);
        }
        assert!((PT_MM - 0.352_777_8).abs() < 1e-6, "PT_MM={PT_MM}");
    }

    #[test]
    fn a_full_page_of_rows_fits_more_than_a_handful() {
        // The old 11 mm advance left room for ~24 rows a page and spent
        // them all on the sections above the first payload.
        let usable = PAGE_H_MM - 2.0 * MARGIN_MM;
        let rows = (usable / ROW_MM).floor() as usize;
        assert!(rows >= 50, "only {rows} rows fit a page");
    }

    /// The width of `n` Courier characters, the box the old character
    /// budget described.
    fn mono_cols(n: usize) -> f32 {
        text_mm(&"0".repeat(n), SIZE_BODY, Face::Mono)
    }

    /// Every wrapped line, measured in the face it will be drawn in.
    fn fits(lines: &[String], box_mm: f32, face: Face) -> bool {
        lines.iter().all(|l| text_mm(l, SIZE_BODY, face) <= box_mm)
    }

    #[test]
    fn wrap_splits_at_spaces_and_respects_the_limit() {
        let box_mm = mono_cols(12);
        let lines = wrap(
            "the quick brown fox jumps over the lazy dog",
            box_mm,
            SIZE_BODY,
            Face::Mono,
        );
        assert!(fits(&lines, box_mm, Face::Mono), "{lines:?}");
        assert_eq!(
            lines.join(" "),
            "the quick brown fox jumps over the lazy dog"
        );
    }

    #[test]
    fn wrap_hard_splits_an_unbreakable_word() {
        // A long path has nowhere to break; it must be cut, not run off
        // the right edge.
        let box_mm = mono_cols(10);
        let lines = wrap(
            "/very/long/path/without/any/spaces/at/all.frd",
            box_mm,
            SIZE_BODY,
            Face::Mono,
        );
        assert!(fits(&lines, box_mm, Face::Mono), "{lines:?}");
        assert_eq!(
            lines.concat(),
            "/very/long/path/without/any/spaces/at/all.frd"
        );
    }

    #[test]
    fn wrap_never_returns_nothing() {
        let box_mm = mono_cols(10);
        assert_eq!(wrap("", box_mm, SIZE_BODY, Face::Mono), vec![String::new()]);
        assert_eq!(
            wrap("   ", box_mm, SIZE_BODY, Face::Mono),
            vec![String::new()]
        );
    }

    #[test]
    fn a_wide_note_wraps_to_lines_that_fit_the_page() {
        // The bug this replaces: `note` sized its lines by Courier's
        // 0.6 em advance and drew them in Helvetica. Prose averages
        // narrower than that and wrapped short, so the report looked
        // right; capitals, digits and hex — a pasted session id, an
        // all-caps warning — average wider, and those lines ran off
        // the right edge as invisibly as off-page content did before
        // PT_MM.
        let box_mm = PAGE_W_MM - 2.0 * MARGIN_MM;
        for text in [
            "MEASUREMENT ABORTED: CHECK ROUTING BEFORE RETRYING THE SWEEP AT THIS DRIVE LEVEL",
            "session 9F3AC2B84E7D615A0C W%W%W%W% 88.5 dB SPL @ 1 m, 20 Hz - 20 kHz, MMMMMMMM",
            "température 21.3 °C — vitesse du son 344.2 m/s, mesurée à 45 % HR",
            &"W".repeat(600),
            &"A B ".repeat(200),
        ] {
            let lines = wrap(&encode(text), box_mm, SIZE_BODY, Face::Regular);
            for line in &lines {
                let w = text_mm(line, SIZE_BODY, Face::Regular);
                assert!(
                    w <= box_mm,
                    "line is {w:.1} mm in the face that draws it, box is {box_mm:.1} mm: {line:?}"
                );
            }
        }
    }

    #[test]
    fn a_value_still_wraps_to_the_column_it_is_set_in() {
        // `kv` draws its values in Courier, so its budget was always
        // the right one; measuring by width instead of by character
        // must not move it.
        let box_mm = PAGE_W_MM - MARGIN_MM - VALUE_X_MM;
        let path =
            "/home/mui/measurements/2026-04-23/session-9f3a/umik-1_calibration_90deg_48k.frd";
        let lines = wrap(&encode(path), box_mm, SIZE_BODY, Face::Mono);
        // 146 mm of Courier holds 72 characters; this path is 81.
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines.concat(), path);
        assert!(fits(&lines, box_mm, Face::Mono), "{lines:?}");
    }

    #[test]
    fn encode_keeps_what_the_core_fonts_can_draw() {
        // These all appear in real reports and are all WinAnsi.
        let kept = "21.3 \u{b0}C \u{2014} 331.3 + 0.606\u{b7}T \u{a7}15.12.3";
        assert_eq!(encode(kept), kept);
    }

    #[test]
    fn encode_substitutes_rather_than_dropping() {
        // An arrow silently vanishing turns a range into two unrelated
        // numbers; that is the failure this replaces.
        assert_eq!(
            encode("20.0 Hz \u{2192} 20000.0 Hz"),
            "20.0 Hz -> 20000.0 Hz"
        );
        assert_eq!(encode("\u{2713} verified"), "* verified");
        // Nothing may disappear without a mark in its place.
        assert_eq!(encode("a\u{4e2d}b"), "a?b");
    }

    #[test]
    fn wrap_handles_multibyte_without_panicking() {
        let box_mm = mono_cols(8);
        let lines = wrap(
            "température 21.3 °C — vitesse 344.2 m/s",
            box_mm,
            SIZE_BODY,
            Face::Mono,
        );
        assert!(fits(&lines, box_mm, Face::Mono), "{lines:?}");
    }

    #[test]
    fn wrap_terminates_in_a_box_narrower_than_a_glyph() {
        // A width no character fits must still make progress rather
        // than loop for ever appending empty lines.
        let lines = wrap("wide words here", 0.0, SIZE_BODY, Face::Regular);
        assert_eq!(lines.concat(), "widewordshere");
    }
}
