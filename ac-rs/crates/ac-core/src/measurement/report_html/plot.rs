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
use crate::measurement::report_layout::chart::{plottable, Chart, XScale};

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

/// Series colours, cycled. Colour is a convenience only: every series
/// also carries its label at the right-hand end of its line and in the
/// legend, so a greyscale print still names each line.
const PALETTE: [&str; 6] = [
    "#1f77b4", "#d62728", "#2ca02c", "#9467bd", "#ff7f0e", "#8c564b",
];

/// Render a [`Chart`] — any number of labelled series on a log-frequency
/// or linear x axis — as a standalone `<svg>`, with a legend under the
/// plot box. Every domain, tick and label comes from the chart; an
/// upper-bound series draws dashed. An empty chart yields an empty
/// string.
pub(super) fn chart(chart: &Chart) -> String {
    if chart.is_empty() {
        return String::new();
    }
    let pad_t = 28.0;
    let plot_h = 260.0;
    let legend_row = 16.0;
    let height = pad_t + plot_h + PAD_B + 8.0 + legend_row * chart.series.len() as f64;
    let plot_w = WIDTH - PAD_L - PAD_R;
    let x = |v: f64| PAD_L + chart.x_frac(v) * plot_w;
    let y = |v: f64| pad_t + (1.0 - chart.y_frac(v)) * plot_h;

    let mut s = String::new();
    let _ = writeln!(
        s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" \
         width=\"{w}\" height=\"{h}\" role=\"img\" aria-label=\"{aria}\">",
        w = WIDTH as i64,
        h = height as i64,
        aria = super::html_escape(&chart.title),
    );
    let _ = writeln!(
        s,
        "<text x=\"{PAD_L}\" y=\"16\" class=\"title\">{}</text>",
        super::html_escape(&chart.title)
    );
    for t in &chart.x_ticks {
        let xp = x(t.value);
        let _ = writeln!(
            s,
            "<line class=\"grid\" x1=\"{xp:.1}\" y1=\"{pad_t}\" x2=\"{xp:.1}\" y2=\"{y1}\" />",
            y1 = pad_t + plot_h,
        );
        let _ = writeln!(
            s,
            "<text x=\"{xp:.1}\" y=\"{ty:.1}\" text-anchor=\"middle\">{}</text>",
            super::html_escape(&t.label),
            ty = pad_t + plot_h + 14.0,
        );
    }
    for t in &chart.y_ticks {
        let yp = y(t.value);
        let _ = writeln!(
            s,
            "<line class=\"grid\" x1=\"{PAD_L}\" y1=\"{yp:.1}\" x2=\"{x1}\" y2=\"{yp:.1}\" />",
            x1 = WIDTH - PAD_R,
        );
        let _ = writeln!(
            s,
            "<text x=\"{tx:.1}\" y=\"{ty:.1}\" text-anchor=\"end\">{}</text>",
            super::html_escape(&t.label),
            tx = PAD_L - 6.0,
            ty = yp + 3.5,
        );
    }
    for b in &chart.bands {
        for edge in [b.lo, b.hi] {
            let xp = x(edge);
            let _ = writeln!(
                s,
                "<line class=\"band\" x1=\"{xp:.1}\" y1=\"{pad_t}\" x2=\"{xp:.1}\" y2=\"{y1}\" />",
                y1 = pad_t + plot_h,
            );
        }
        let mid = match chart.x_scale {
            XScale::LogFrequency => (b.lo * b.hi).sqrt(),
            XScale::Linear => (b.lo + b.hi) / 2.0,
        };
        let _ = writeln!(
            s,
            "<text class=\"band\" x=\"{xp:.1}\" y=\"{ty:.1}\" text-anchor=\"middle\">{}</text>",
            super::html_escape(&b.label),
            xp = x(mid),
            ty = pad_t + 12.0,
        );
    }
    let _ = writeln!(
        s,
        "<rect class=\"axis\" x=\"{PAD_L}\" y=\"{pad_t}\" width=\"{plot_w}\" height=\"{plot_h}\" />"
    );
    let _ = writeln!(
        s,
        "<text x=\"{tx:.1}\" y=\"{ty:.1}\" text-anchor=\"end\">{}</text>",
        super::html_escape(&chart.x_label),
        tx = WIDTH - PAD_R,
        ty = pad_t + plot_h + 28.0,
    );
    let _ = writeln!(
        s,
        "<text x=\"{PAD_L}\" y=\"{ty:.1}\">{}</text>",
        super::html_escape(&chart.y_label),
        ty = pad_t - 4.0,
    );

    for (i, series) in chart.series.iter().enumerate() {
        let colour = PALETTE[i % PALETTE.len()];
        let dash = if series.upper_bound {
            " stroke-dasharray=\"6 4\""
        } else {
            ""
        };
        let mut d = String::new();
        let mut pen_down = false;
        let mut last = None;
        for p in &series.points {
            if !plottable(chart.x_scale, *p) {
                pen_down = false;
                continue;
            }
            let (xp, yp) = (x(p.0), y(p.1));
            let _ = write!(d, "{}{xp:.2} {yp:.2} ", if pen_down { 'L' } else { 'M' });
            pen_down = true;
            last = Some((xp, yp));
            if chart.point_markers {
                let _ = writeln!(
                    s,
                    "<circle cx=\"{xp:.2}\" cy=\"{yp:.2}\" r=\"3\" fill=\"{colour}\" />"
                );
            }
        }
        if !d.is_empty() {
            let _ = writeln!(
                s,
                "<path class=\"series\" stroke=\"{colour}\"{dash} d=\"{}\" />",
                d.trim_end()
            );
        }
        if let Some((xp, yp)) = last {
            let _ = writeln!(
                s,
                "<text x=\"{tx:.1}\" y=\"{ty:.1}\" text-anchor=\"end\" fill=\"{colour}\">{}</text>",
                super::html_escape(&series.label),
                tx = xp - 4.0,
                ty = yp - 5.0,
            );
        }
        let ly = pad_t + plot_h + PAD_B + 8.0 + legend_row * i as f64;
        let _ = writeln!(
            s,
            "<line x1=\"{PAD_L}\" y1=\"{ly:.1}\" x2=\"{x2}\" y2=\"{ly:.1}\" stroke=\"{colour}\" stroke-width=\"1.6\"{dash} />",
            x2 = PAD_L + 30.0,
        );
        let _ = writeln!(
            s,
            "<text x=\"{tx}\" y=\"{ty:.1}\">{}</text>",
            super::html_escape(&series.label),
            tx = PAD_L + 38.0,
            ty = ly + 3.5,
        );
    }
    let _ = writeln!(s, "</svg>");
    s
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

    fn two_series_chart(upper_bound: bool) -> Chart {
        use crate::measurement::report_layout::chart::Series;
        let mut floor = Series::new(
            "H4 \u{2264} floor-limited",
            vec![(-40.0, -80.0), (-30.0, -80.0)],
        );
        floor.upper_bound = upper_bound;
        Chart::new(
            "Harmonic <level>",
            "drive, dBFS",
            "dB",
            XScale::Linear,
            vec![
                Series::new(
                    "H2 tracks drive",
                    vec![(-40.0, -70.0), (-30.0, f64::NAN), (-20.0, -50.0)],
                ),
                floor,
            ],
            vec![],
            6.0,
        )
    }

    #[test]
    fn chart_draws_every_series_with_its_label_and_dashes_an_upper_bound() {
        let svg = chart(&two_series_chart(true));
        assert_eq!(svg.matches("class=\"series\"").count(), 2, "{svg}");
        assert!(svg.contains("H2 tracks drive"));
        assert!(svg.contains("H4 \u{2264} floor-limited"));
        assert!(svg.contains("Harmonic &lt;level&gt;"));
        // One dashed path and one dashed legend swatch.
        assert_eq!(svg.matches("stroke-dasharray").count(), 2, "{svg}");
        assert!(!chart(&two_series_chart(false)).contains("stroke-dasharray"));
        assert!(!svg.contains("NaN"), "{svg}");
    }

    #[test]
    fn an_empty_chart_draws_nothing() {
        let empty = Chart::new("t", "x", "y", XScale::Linear, vec![], vec![], 1.0);
        assert!(chart(&empty).is_empty());
    }

    #[test]
    fn aria_label_is_escaped() {
        let svg = magnitude("a<b>&c", 300.0, &[(100.0, -1.0), (1_000.0, -2.0)]);
        assert!(svg.contains("a&lt;b&gt;&amp;c"), "{svg}");
    }
}
