//! A backend-agnostic multi-series XY chart: domains, ticks with their
//! labels, labelled series and band marks, all decided once here so the
//! HTML and PDF painters draw the same chart from the same numbers.
//!
//! A painter maps a data point to its box through [`Chart::x_frac`] /
//! [`Chart::y_frac`] and draws. It never picks a domain, a tick or a
//! label, and never decides a series' style: whether a line is an upper
//! bound is [`Series::upper_bound`], set from the measurement's own
//! reading.

use super::axis;

/// How the x axis maps values to position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XScale {
    /// Log frequency, Hz.
    LogFrequency,
    Linear,
}

/// A gridline position and its label.
#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    pub value: f64,
    pub label: String,
}

/// One labelled line.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    /// Legend text, as printed.
    pub label: String,
    pub points: Vec<(f64, f64)>,
    /// The values are upper bounds, not measurements: a painter draws
    /// the line dashed so it cannot pass for a measured flat response.
    pub upper_bound: bool,
}

impl Series {
    pub fn new(label: impl Into<String>, points: Vec<(f64, f64)>) -> Self {
        Self {
            label: label.into(),
            points,
            upper_bound: false,
        }
    }
}

/// An x range marked with thin rules at both edges and a label between.
#[derive(Debug, Clone, PartialEq)]
pub struct BandMark {
    pub lo: f64,
    pub hi: f64,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    pub title: String,
    pub x_label: String,
    pub y_label: String,
    pub x_scale: XScale,
    pub x_domain: (f64, f64),
    pub y_domain: (f64, f64),
    pub x_ticks: Vec<Tick>,
    pub y_ticks: Vec<Tick>,
    pub series: Vec<Series>,
    pub bands: Vec<BandMark>,
    /// Draw a marker at every point: a chart over a handful of runs,
    /// where a line alone hides which points were measured.
    pub point_markers: bool,
}

impl Chart {
    /// Build a chart and decide its domains and ticks from the data.
    /// `y_min_span` is the narrowest y range drawn, in y units.
    pub fn new(
        title: impl Into<String>,
        x_label: impl Into<String>,
        y_label: impl Into<String>,
        x_scale: XScale,
        series: Vec<Series>,
        bands: Vec<BandMark>,
        y_min_span: f64,
    ) -> Self {
        let xs = || series.iter().flat_map(|s| s.points.iter().map(|p| p.0));
        let ys = series.iter().flat_map(|s| s.points.iter().map(|p| p.1));
        let (x_domain, x_ticks) = match x_scale {
            XScale::LogFrequency => {
                let (lo, hi) = axis::log_freq_domain(xs());
                let ticks = axis::freq_ticks(lo, hi)
                    .into_iter()
                    .map(|f| Tick {
                        value: f,
                        label: format!("{} Hz", axis::format_freq(f)),
                    })
                    .collect();
                ((lo, hi), ticks)
            }
            XScale::Linear => {
                let (lo, hi) = axis::linear_domain(xs(), 1.0);
                ((lo, hi), linear_ticks(lo, hi))
            }
        };
        let y_domain = axis::linear_domain(ys, y_min_span);
        let y_ticks = linear_ticks(y_domain.0, y_domain.1);
        Self {
            title: title.into(),
            x_label: x_label.into(),
            y_label: y_label.into(),
            x_scale,
            x_domain,
            y_domain,
            x_ticks,
            y_ticks,
            series,
            bands,
            point_markers: x_scale == XScale::Linear,
        }
    }

    /// Fractional x position in `[0, 1]`; 0 for a non-finite value.
    pub fn x_frac(&self, x: f64) -> f64 {
        match self.x_scale {
            XScale::LogFrequency => axis::log_pos(x, self.x_domain.0, self.x_domain.1),
            XScale::Linear => axis::lin_pos(x, self.x_domain.0, self.x_domain.1),
        }
    }

    /// Fractional y position in `[0, 1]`, 0 at the bottom.
    pub fn y_frac(&self, y: f64) -> f64 {
        axis::lin_pos(y, self.y_domain.0, self.y_domain.1)
    }

    /// Nothing to draw: no series with a plottable point.
    pub fn is_empty(&self) -> bool {
        !self
            .series
            .iter()
            .any(|s| s.points.iter().any(|p| plottable(self.x_scale, *p)))
    }
}

/// A point a painter may place: finite, and positive on a log axis.
pub fn plottable(scale: XScale, (x, y): (f64, f64)) -> bool {
    x.is_finite() && y.is_finite() && (scale == XScale::Linear || x > 0.0)
}

fn linear_ticks(lo: f64, hi: f64) -> Vec<Tick> {
    let step = axis::linear_step(hi - lo);
    axis::linear_ticks(lo, hi)
        .into_iter()
        .map(|v| Tick {
            value: v,
            label: axis::format_linear(v, step),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chart_decides_its_own_domains_and_labels() {
        let c = Chart::new(
            "t",
            "x",
            "y",
            XScale::Linear,
            vec![Series::new("a", vec![(-40.0, -1.0), (-30.0, 1.0)])],
            vec![],
            1.0,
        );
        assert!(c.x_domain.0 < -40.0 && c.x_domain.1 > -30.0);
        assert!(!c.x_ticks.is_empty() && !c.y_ticks.is_empty());
        assert!(c.point_markers);
        assert_eq!(c.x_frac(c.x_domain.0), 0.0);
        assert_eq!(c.y_frac(c.y_domain.1), 1.0);
        assert!(!c.is_empty());
    }

    #[test]
    fn log_ticks_carry_their_unit_and_nan_never_becomes_a_position() {
        let c = Chart::new(
            "t",
            "x",
            "y",
            XScale::LogFrequency,
            vec![Series::new(
                "a",
                vec![(0.0, 1.0), (100.0, 1.0), (10_000.0, f64::NAN)],
            )],
            vec![],
            1.0,
        );
        assert!(c.x_ticks.iter().all(|t| t.label.ends_with(" Hz")));
        assert_eq!(c.x_frac(f64::NAN), 0.0);
        assert!(!plottable(XScale::LogFrequency, (0.0, 1.0)));
        let empty = Chart::new("t", "x", "y", XScale::Linear, vec![], vec![], 1.0);
        assert!(empty.is_empty());
    }
}
