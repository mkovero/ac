//! The magnitude plot box: frame, log-frequency and dB grids, trace.
//!
//! Domains, gridline steps and tick labels come from
//! `report_layout::axis`, the same source the HTML backend draws from,
//! so one report plots on one grid in both formats.
//!
//! [`draw_chart`] paints a verification report's multi-series charts
//! (#398) from a `report_layout::chart::Chart`; the single-run plot stays
//! on [`draw`].

use printpdf::{Mm, Point};

use super::cursor::{Cursor, BODY_MM, MARGIN_MM, PAGE_W_MM, SIZE_SMALL, SMALL_MM};
use super::metrics::{text_mm, Face};
use crate::measurement::report_layout::axis;
use crate::measurement::report_layout::chart::{plottable, Chart};

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
        // Centred on its tick, but never past a margin: the decade at
        // the right edge of the box sits on x1, and a fixed nudge left
        // put its label 2.6 mm into the margin — off-page for a wider
        // one, and `printpdf` would not have said so.
        let label = axis::format_freq(f);
        let w = text_mm(&label, SIZE_SMALL, Face::Mono);
        let x_label = (x - w / 2.0).clamp(MARGIN_MM, PAGE_W_MM - MARGIN_MM - w);
        cur.text_at(&label, SIZE_SMALL, x_label, y0 - SMALL_MM - 0.5, Face::Mono);
    }

    for v in axis::db_gridlines(dmin, dmax) {
        let y = y_at(v);
        cur.hline(y, x0, x1, 0.15);
        cur.text_at(
            &format!("{v:.0}"),
            SIZE_SMALL,
            MARGIN_MM,
            y - SMALL_MM * 0.35,
            Face::Mono,
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

/// Height of a verification chart's box.
const CHART_H_MM: f32 = 70.0;

/// Dash and gap of an upper-bound series, millimetres.
const DASH_MM: f32 = 1.8;
const GAP_MM: f32 = 1.4;

/// Draw a [`Chart`]: title, frame, the chart's own ticks and labels, band
/// marks, every series (dashed when it is an upper bound) labelled at its
/// right-hand end, and a legend beneath. The core fonts give one trace
/// colour, so series are told apart by their labels, not by colour.
pub(super) fn draw_chart(cur: &mut Cursor, chart: &Chart) {
    if chart.is_empty() {
        return;
    }
    let legend_h = chart.series.len() as f32 * (SMALL_MM + 1.5);
    let needed = BODY_MM + 3.0 + CHART_H_MM + 2.0 * SMALL_MM + 3.0 + legend_h + 2.0;
    cur.ensure(needed);

    let top = cur.y();
    cur.text_at(
        &chart.title,
        SIZE_SMALL,
        MARGIN_MM,
        top - SMALL_MM,
        Face::Bold,
    );
    cur.text_at(
        &chart.y_label,
        SIZE_SMALL,
        MARGIN_MM + GUTTER_MM,
        top - 2.0 * SMALL_MM - 1.0,
        Face::Regular,
    );

    let x0 = MARGIN_MM + GUTTER_MM;
    let x1 = PAGE_W_MM - MARGIN_MM;
    let y1 = top - BODY_MM - 3.0;
    let y0 = y1 - CHART_H_MM;
    cur.rect(x0, y0, x1, y1, 0.4);

    let x_at = |v: f64| lerp(x0, x1, chart.x_frac(v) as f32);
    let y_at = |v: f64| lerp(y0, y1, chart.y_frac(v) as f32);

    for t in &chart.x_ticks {
        let x = x_at(t.value);
        cur.vline(x, y0, y1, 0.15);
        let w = text_mm(&t.label, SIZE_SMALL, Face::Mono);
        let x_label = (x - w / 2.0).clamp(MARGIN_MM, PAGE_W_MM - MARGIN_MM - w);
        cur.text_at(
            &t.label,
            SIZE_SMALL,
            x_label,
            y0 - SMALL_MM - 0.5,
            Face::Mono,
        );
    }
    for t in &chart.y_ticks {
        let y = y_at(t.value);
        cur.hline(y, x0, x1, 0.15);
        cur.text_at(
            &t.label,
            SIZE_SMALL,
            MARGIN_MM,
            y - SMALL_MM * 0.35,
            Face::Mono,
        );
    }
    let w = text_mm(&chart.x_label, SIZE_SMALL, Face::Regular);
    cur.text_at(
        &chart.x_label,
        SIZE_SMALL,
        x1 - w,
        y0 - 2.0 * SMALL_MM - 1.0,
        Face::Regular,
    );
    for b in &chart.bands {
        let (xa, xb) = (x_at(b.lo), x_at(b.hi));
        cur.vline(xa, y0, y1, 0.35);
        cur.vline(xb, y0, y1, 0.35);
        let w = text_mm(&b.label, SIZE_SMALL, Face::Mono);
        let x = ((xa + xb) / 2.0 - w / 2.0).clamp(x0, x1 - w);
        cur.text_at(&b.label, SIZE_SMALL, x, y1 - SMALL_MM - 0.5, Face::Mono);
    }

    for series in &chart.series {
        let mut runs: Vec<Vec<(f32, f32)>> = vec![Vec::new()];
        for p in &series.points {
            if plottable(chart.x_scale, *p) {
                if let Some(r) = runs.last_mut() {
                    r.push((x_at(p.0), y_at(p.1)));
                }
            } else if !runs.last().is_some_and(Vec::is_empty) {
                runs.push(Vec::new());
            }
        }
        for run in &runs {
            if series.upper_bound {
                for dash in dashes(run) {
                    stroke(cur, &dash);
                }
            } else {
                stroke(cur, run);
            }
            if chart.point_markers {
                for &(x, y) in run {
                    stroke(cur, &[(x - 0.8, y - 0.8), (x + 0.8, y + 0.8)]);
                    stroke(cur, &[(x - 0.8, y + 0.8), (x + 0.8, y - 0.8)]);
                }
            }
        }
        if let Some(&(x, y)) = runs.iter().flatten().last() {
            let w = text_mm(&series.label, SIZE_SMALL, Face::Regular);
            let x_label = (x - w - 1.0).clamp(x0, x1 - w);
            cur.text_at(&series.label, SIZE_SMALL, x_label, y + 1.0, Face::Regular);
        }
    }

    let mut ly = y0 - 2.0 * SMALL_MM - 4.0;
    for series in &chart.series {
        let sample = [
            (x0, ly + SMALL_MM * 0.35),
            (x0 + 10.0, ly + SMALL_MM * 0.35),
        ];
        if series.upper_bound {
            for dash in dashes(&sample) {
                stroke(cur, &dash);
            }
        } else {
            stroke(cur, &sample);
        }
        cur.text_at(&series.label, SIZE_SMALL, x0 + 12.0, ly, Face::Regular);
        ly -= SMALL_MM + 1.5;
    }

    cur.advance(needed);
}

fn stroke(cur: &Cursor, points: &[(f32, f32)]) {
    cur.trace(
        points
            .iter()
            .map(|&(x, y)| (Point::new(Mm(x), Mm(y)), false))
            .collect(),
    );
}

/// Cut a polyline into dash segments of [`DASH_MM`] separated by
/// [`GAP_MM`], measured along the line, so an upper bound reads as dashed
/// in a format whose painter has no dash style to hand.
fn dashes(line: &[(f32, f32)]) -> Vec<Vec<(f32, f32)>> {
    let mut out = Vec::new();
    let mut current: Vec<(f32, f32)> = Vec::new();
    let mut on = true;
    let mut left = DASH_MM;
    for w in line.windows(2) {
        let (mut a, b) = (w[0], w[1]);
        let mut len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        while len > 0.0 {
            if on && current.is_empty() {
                current.push(a);
            }
            let step = left.min(len);
            let t = step / len;
            let p = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            if on {
                current.push(p);
            }
            left -= step;
            len -= step;
            a = p;
            if left <= 0.0 {
                if on && current.len() >= 2 {
                    out.push(std::mem::take(&mut current));
                }
                current.clear();
                on = !on;
                left = if on { DASH_MM } else { GAP_MM };
            }
        }
    }
    if on && current.len() >= 2 {
        out.push(current);
    }
    out
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
    fn a_chart_plus_its_legend_fits_one_page() {
        let needed = BODY_MM + 3.0 + CHART_H_MM + 2.0 * SMALL_MM + 3.0 + 8.0 * (SMALL_MM + 1.5);
        assert!(needed < super::super::cursor::PAGE_H_MM - 2.0 * MARGIN_MM);
    }

    #[test]
    fn dashes_cover_the_line_with_gaps() {
        let d = dashes(&[(0.0, 0.0), (10.0, 0.0)]);
        // 10 mm of 1.8 on / 1.4 off: dashes start at 0, 3.2, 6.4, 9.6.
        assert_eq!(d.len(), 4, "{d:?}");
        assert!(d.iter().all(|s| s.len() >= 2));
        assert!((d[1][0].0 - 3.2).abs() < 1e-4, "{d:?}");
        assert!(dashes(&[(0.0, 0.0)]).is_empty());
    }

    #[test]
    fn lerp_clamps_out_of_range_positions() {
        assert_eq!(lerp(0.0, 10.0, -1.0), 0.0);
        assert_eq!(lerp(0.0, 10.0, 2.0), 10.0);
        assert_eq!(lerp(0.0, 10.0, f32::NAN), 0.0);
    }
}
