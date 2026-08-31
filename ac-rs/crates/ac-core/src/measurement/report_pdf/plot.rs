//! The magnitude plot box: frame, log-frequency and dB grids, trace.
//!
//! Domains, gridline steps and tick labels come from
//! `report_layout::axis`, the same source the HTML backend draws from,
//! so one report plots on one grid in both formats.

use printpdf::{Mm, Point};

use super::cursor::{Cursor, MARGIN_MM, PAGE_W_MM, SIZE_SMALL, SMALL_MM};
use crate::measurement::report_layout::axis;

/// Height of the plot box itself, excluding its frequency labels.
const PLOT_H_MM: f32 = 75.0;

/// Left gutter for dB tick labels.
const GUTTER_MM: f32 = 14.0;

/// Draw a `(frequency, level)` trace. Fewer than two plottable points
/// is nothing to draw, and the caller's table still carries the data.
pub(super) fn draw(cur: &mut Cursor, series: &[(f64, f64)]) {
    let plottable: Vec<(f64, f64)> = series
        .iter()
        .copied()
        .filter(|(f, v)| *f > 0.0 && f.is_finite() && v.is_finite())
        .collect();
    if plottable.len() < 2 {
        return;
    }

    let needed = 2.0 + PLOT_H_MM + SMALL_MM + 3.0;
    cur.ensure(needed);

    let x0 = MARGIN_MM + GUTTER_MM;
    let x1 = PAGE_W_MM - MARGIN_MM;
    let y1 = cur.y() - 2.0;
    let y0 = y1 - PLOT_H_MM;

    cur.rect(x0, y0, x1, y1, 0.4);

    let (fmin, fmax) = axis::log_freq_domain(plottable.iter().map(|(f, _)| *f));
    let (dmin, dmax) = axis::db_domain(plottable.iter().map(|(_, v)| *v));

    let x_at = |f: f64| lerp(x0, x1, axis::log_pos(f, fmin, fmax) as f32);
    let y_at = |v: f64| lerp(y0, y1, axis::lin_pos(v, dmin, dmax) as f32);

    for f in axis::freq_ticks(fmin, fmax) {
        let x = x_at(f);
        cur.vline(x, y0, y1, 0.15);
        cur.layer().use_text(
            axis::format_freq(f),
            SIZE_SMALL,
            Mm(x - 2.5),
            Mm(y0 - SMALL_MM - 0.5),
            &cur.fonts().mono,
        );
    }

    for v in axis::db_gridlines(dmin, dmax) {
        let y = y_at(v);
        cur.hline(y, x0, x1, 0.15);
        cur.layer().use_text(
            format!("{v:.0}"),
            SIZE_SMALL,
            Mm(MARGIN_MM),
            Mm(y - SMALL_MM * 0.35),
            &cur.fonts().mono,
        );
    }

    cur.trace(
        plottable
            .iter()
            .map(|(f, v)| (Point::new(Mm(x_at(*f)), Mm(y_at(*v))), false))
            .collect(),
    );

    cur.advance(needed);
}

/// `f32::clamp` returns NaN for a NaN input, which would place a point
/// off the media box; map it to the start of the range instead.
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plot_box_fits_between_the_margins() {
        let x0 = MARGIN_MM + GUTTER_MM;
        let x1 = PAGE_W_MM - MARGIN_MM;
        assert!(x1 > x0);
        assert!(x1 <= PAGE_W_MM - MARGIN_MM);
    }

    #[test]
    fn a_plot_plus_its_labels_fits_one_page() {
        // If the reserved height ever exceeded the printable area,
        // `Cursor::ensure` would loop: break the page, still not fit,
        // break again.
        let needed = 2.0 + PLOT_H_MM + SMALL_MM + 3.0;
        assert!(
            needed < super::super::cursor::PAGE_H_MM - 2.0 * MARGIN_MM,
            "plot reserves {needed} mm"
        );
    }

    #[test]
    fn lerp_clamps_out_of_range_positions() {
        assert_eq!(lerp(0.0, 10.0, -1.0), 0.0);
        assert_eq!(lerp(0.0, 10.0, 2.0), 10.0);
        assert_eq!(lerp(0.0, 10.0, f32::NAN), 0.0);
    }
}
