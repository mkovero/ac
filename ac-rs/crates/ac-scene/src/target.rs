//! A target curve (Smaart's Target Curve): a desired magnitude response,
//! imported from a `<freq_hz> <gain_db>` text file and drawn over the
//! magnitude pane so an EQ can be set to meet it.
//!
//! Drawn as given: its dB are the file's, on the same axis as the traces.
//! Level-matching is the operator's, with the trace offset (`J`) — the
//! target is the fixed reference, as in Smaart.

use crate::scene::{Provenance, Source, Trace};
use crate::ticks::{db_to_y, freq_to_x};

/// Most points a target file may hold — the mic-curve bound; a target is
/// usually a handful.
pub const MAX_POINTS: usize = 4096;

#[derive(Debug, Clone, PartialEq)]
pub struct TargetCurve {
    /// The file's own name, shown in the caption.
    pub name: String,
    pub freqs_hz: Vec<f32>,
    pub gain_db: Vec<f32>,
}

impl TargetCurve {
    /// Parse a target file with the mic-curve parser
    /// ([`ac_core::shared::freq_curve`]): two points at least, since a
    /// target is often a few corner points joined by straight lines.
    pub fn parse(name: &str, text: &str) -> Result<TargetCurve, String> {
        let (freqs_hz, gain_db) =
            ac_core::shared::freq_curve::parse_freq_db(text, "target curve", 2..=MAX_POINTS)
                .map_err(|e| format!("{e:#}"))?;
        Ok(TargetCurve {
            name: name.to_string(),
            freqs_hz,
            gain_db,
        })
    }

    /// The curve in the magnitude pane's coordinates: its points joined by
    /// straight lines on the log-frequency axis — log-linear
    /// interpolation, the same the mic curve uses between its points. Only
    /// over the file's own span: nothing is invented past its ends.
    pub fn trace(&self, freq_range: (f64, f64), db_range: (f64, f64)) -> Trace {
        let points = self
            .freqs_hz
            .iter()
            .zip(&self.gain_db)
            .map(|(&f, &g)| {
                (
                    freq_to_x(f64::from(f), freq_range.0, freq_range.1),
                    db_to_y(f64::from(g), db_range.0, db_range.1),
                )
            })
            .collect();
        Trace::single(
            points,
            Provenance {
                channel_role: "target".to_string(),
                source: Source::Target,
                sr: 0,
            },
        )
    }

    /// `"target: house.txt"` — which file is drawn.
    pub fn caption(&self) -> String {
        format!("target: {}", self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_few_corner_points_draw_as_given_on_the_shared_axes() {
        let t = TargetCurve::parse("house.txt", "20 6\n1000 0\n20000,-3\n").unwrap();
        let tr = t.trace((20.0, 20_000.0), (-80.0, 20.0));
        assert_eq!(tr.segments.len(), 1);
        let pts = &tr.segments[0];
        assert_eq!(pts.len(), 3);
        assert!((pts[0].0 - 0.0).abs() < 1e-9 && (pts[2].0 - 1.0).abs() < 1e-9);
        assert!((pts[0].1 - db_to_y(6.0, -80.0, 20.0)).abs() < 1e-9);
        assert!((pts[2].1 - db_to_y(-3.0, -80.0, 20.0)).abs() < 1e-9);
        assert_eq!(tr.provenance.source, Source::Target);
        assert_eq!(t.caption(), "target: house.txt");
    }

    #[test]
    fn a_single_point_or_a_bad_line_is_refused_with_the_reason() {
        let e = TargetCurve::parse("x", "1000 0\n").unwrap_err();
        assert_eq!(e, "target curve too sparse: got 1 points, need ≥ 2");
        let e = TargetCurve::parse("x", "20 6\nnope\n").unwrap_err();
        assert!(e.starts_with("line 2:"), "{e}");
    }
}
