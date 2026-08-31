//! One inline-SVG plot renderer.
//!
//! The three plots this backend draws — ungated magnitude, gated
//! magnitude, gated phase — used to be three near-identical ~110-line
//! functions that shared a decade-grid loop, an axis frame, an SVG
//! preamble and an x-mapping closure, and differed only in height,
//! field, label and trace class. They are one function and three
//! descriptions now.

use std::fmt::Write as _;

use crate::measurement::report_layout::axis;

const WIDTH: f64 = 900.0;
const PAD_L: f64 = 60.0;
const PAD_R: f64 = 20.0;
const PAD_B: f64 = 40.0;

/// What the y-axis measures. Magnitude takes its domain from the data;
/// phase is fixed to the range `atan2` wraps into, so a phase panel
/// stacked under a magnitude panel keeps a stable vertical scale
/// between reports.
pub(super) enum YAxis {
    Db,
    PhaseDegrees,
}

pub(super) struct Plot<'a> {
    pub height: f64,
    pub pad_t: f64,
    pub aria: &'a str,
    pub trace_class: &'a str,
    pub y_axis: YAxis,
    /// Start a new sub-path when successive values jump by more than
    /// this, so a `\u{00b1}180\u{00b0}` wrap does not draw as a vertical
    /// spike a reader could mistake for a real transient (#284).
    pub break_above: Option<f64>,
}

impl Plot<'_> {
    /// Render `series` — `(frequency, value)` pairs, already free of DC
    /// — as a standalone `<svg>` element. Fewer than two points is
    /// nothing to plot and yields an empty string.
    pub(super) fn render(&self, series: &[(f64, f64)]) -> String {
        if series.len() < 2 {
            return String::new();
        }
        let (fmin, fmax) = axis::log_freq_domain(series.iter().map(|(f, _)| *f));
        let (ymin, ymax) = match self.y_axis {
            YAxis::Db => axis::db_domain(series.iter().map(|(_, v)| *v)),
            YAxis::PhaseDegrees => (-180.0, 180.0),
        };

        let plot_w = WIDTH - PAD_L - PAD_R;
        let plot_h = self.height - self.pad_t - PAD_B;
        let x = |f: f64| PAD_L + axis::log_pos(f, fmin, fmax) * plot_w;
        let y = |v: f64| self.pad_t + (1.0 - axis::lin_pos(v, ymin, ymax)) * plot_h;

        let mut s = String::new();
        let _ = writeln!(
            s,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" \
             width=\"{w}\" height=\"{h}\" role=\"img\" aria-label=\"{aria}\">",
            w = WIDTH as i64,
            h = self.height as i64,
            aria = super::html_escape(self.aria),
        );

        for f in axis::freq_ticks(fmin, fmax) {
            let xp = x(f);
            let _ = writeln!(
                s,
                "<line class=\"grid\" x1=\"{xp:.1}\" y1=\"{y0}\" x2=\"{xp:.1}\" y2=\"{y1}\" />",
                y0 = self.pad_t as i64,
                y1 = (self.height - PAD_B) as i64,
            );
            let _ = writeln!(
                s,
                "<text x=\"{xp:.1}\" y=\"{ty:.1}\" text-anchor=\"middle\">{label} Hz</text>",
                ty = self.height - PAD_B + 14.0,
                label = axis::format_freq(f),
            );
        }

        for (v, label) in self.y_ticks(ymin, ymax) {
            let yp = y(v);
            let _ = writeln!(
                s,
                "<line class=\"grid\" x1=\"{x0}\" y1=\"{yp:.1}\" x2=\"{x1}\" y2=\"{yp:.1}\" />",
                x0 = PAD_L as i64,
                x1 = (WIDTH - PAD_R) as i64,
            );
            let _ = writeln!(
                s,
                "<text x=\"{tx:.1}\" y=\"{ty:.1}\" text-anchor=\"end\">{label}</text>",
                tx = PAD_L - 6.0,
                ty = yp + 3.5,
            );
        }

        let _ = writeln!(
            s,
            "<rect class=\"axis\" x=\"{x0}\" y=\"{y0}\" width=\"{w}\" height=\"{h}\" />",
            x0 = PAD_L as i64,
            y0 = self.pad_t as i64,
            w = plot_w as i64,
            h = plot_h as i64,
        );

        let mut d = String::new();
        let mut prev: Option<f64> = None;
        for (f, v) in series {
            // An unmeasurable point breaks the trace rather than
            // pinning it to the axis: a gap says "no reading here", a
            // line to the floor asserts a value nobody measured.
            if !f.is_finite() || *f <= 0.0 || !v.is_finite() {
                prev = None;
                continue;
            }
            let start_new = match (prev, self.break_above) {
                (None, _) => true,
                (Some(p), Some(limit)) => (v - p).abs() > limit,
                (Some(_), None) => false,
            };
            let _ = write!(
                d,
                "{}{:.2} {:.2} ",
                if start_new { 'M' } else { 'L' },
                x(*f),
                y(*v)
            );
            prev = Some(*v);
        }
        if !d.is_empty() {
            let _ = writeln!(
                s,
                "<path class=\"{}\" d=\"{}\" />",
                self.trace_class,
                d.trim_end()
            );
        }
        let _ = writeln!(s, "</svg>");
        s
    }

    fn y_ticks(&self, ymin: f64, ymax: f64) -> Vec<(f64, String)> {
        match self.y_axis {
            YAxis::Db => axis::db_gridlines(ymin, ymax)
                .into_iter()
                .map(|v| (v, format!("{v:.0} dB")))
                .collect(),
            YAxis::PhaseDegrees => [-180.0_f64, 0.0, 180.0]
                .into_iter()
                .map(|v| (v, format!("{v:+.0}\u{b0}")))
                .collect(),
        }
    }
}

pub(super) fn magnitude(aria: &str, height: f64, series: &[(f64, f64)]) -> String {
    Plot {
        height,
        pad_t: 20.0,
        aria,
        trace_class: "trace",
        y_axis: YAxis::Db,
        break_above: None,
    }
    .render(series)
}

pub(super) fn phase(series: &[(f64, f64)]) -> String {
    Plot {
        height: 140.0,
        pad_t: 10.0,
        aria: "Gated frequency response phase",
        trace_class: "trace-phase",
        // A stacked second panel rather than a dual-axis overlay: dB and
        // degrees sharing one y-axis is exactly the optical noise a
        // static document, with no zoom or toggle to disambiguate,
        // should not carry (#284).
        y_axis: YAxis::PhaseDegrees,
        break_above: Some(180.0),
    }
    .render(series)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fewer_than_two_points_plots_nothing() {
        assert!(magnitude("x", 300.0, &[]).is_empty());
        assert!(magnitude("x", 300.0, &[(100.0, -1.0)]).is_empty());
    }

    #[test]
    fn no_coordinate_is_ever_nan() {
        // A single distinct frequency gives a zero-width log domain;
        // the previous renderer divided by it and wrote `NaN` straight
        // into the path data.
        for series in [
            vec![(1_000.0, -20.0), (1_000.0, -20.0)],
            vec![(100.0, -20.0), (1_000.0, -20.0)],
            vec![(100.0, f64::NAN), (1_000.0, -20.0)],
        ] {
            let svg = magnitude("x", 300.0, &series);
            assert!(!svg.contains("NaN"), "{svg}");
            assert!(!svg.contains("inf"), "{svg}");
        }
    }

    #[test]
    fn phase_breaks_the_path_at_a_wrap() {
        // +170 -> -170 is a wrap, not a 340-degree excursion.
        let svg = phase(&[(100.0, 170.0), (200.0, -170.0), (400.0, -160.0)]);
        let d = svg
            .split("d=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .expect("path data");
        assert_eq!(d.matches('M').count(), 2, "{d}");
    }

    #[test]
    fn phase_axis_is_fixed_regardless_of_the_data_range() {
        let narrow = phase(&[(100.0, 1.0), (1_000.0, 2.0)]);
        assert!(narrow.contains("+180"), "{narrow}");
        assert!(narrow.contains("-180"), "{narrow}");
    }

    #[test]
    fn aria_label_is_escaped() {
        let svg = magnitude("a<b>&c", 300.0, &[(100.0, -1.0), (1_000.0, -2.0)]);
        assert!(svg.contains("a&lt;b&gt;&amp;c"), "{svg}");
    }
}
