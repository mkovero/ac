use egui::{Color32, Painter, Pos2, Rect, Stroke};

use crate::data::types::{CellView, ViewMode};
use crate::theme;

pub struct WaterfallTimeAxis {
    /// Seconds between successive waterfall rows (producer frame interval).
    pub row_period_s: f32,
    /// Fractional number of newest rows currently stretched across the cell.
    /// Tracks `CellView::rows_visible_f` directly so the time axis grows
    /// and shrinks smoothly while the shader-facing `u32` counterpart steps
    /// at integer boundaries. One label per ~4 subdivisions of the axis.
    pub rows_visible: f32,
}

pub fn draw_grid(
    painter: &Painter,
    rect: Rect,
    view: &CellView,
    view_mode: ViewMode,
    show_labels: bool,
    show_freq_labels: bool,
    time_axis: Option<WaterfallTimeAxis>,
    spl_offset_db: Option<f32>,
) {
    // Scope mode owns its full cell — no spectrum-style freq/dB grid or
    // axis labels. The substrate aesthetic is "ember on pure black";
    // overlay clutter breaks it.
    if matches!(
        view_mode,
        ViewMode::Scope
            | ViewMode::SpectrumEmber
            | ViewMode::Goniometer
            | ViewMode::IoTransfer
            | ViewMode::BodeMag
            | ViewMode::Coherence
            | ViewMode::BodePhase
            | ViewMode::GroupDelay
            | ViewMode::Nyquist
    ) {
        return;
    }
    let stroke = Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(255, 140, 80, (0.05 * 255.0) as u8),
    );
    let label_color = Color32::from_rgb(
        theme::GRID_LABEL[0],
        theme::GRID_LABEL[1],
        theme::GRID_LABEL[2],
    );

    let log_min = view.freq_min.max(1.0).log10();
    let log_max = view.freq_max.max(log_min.exp().max(1.1)).log10();
    let span = (log_max - log_min).max(0.0001);

    for f in freq_ticks(view.freq_min, view.freq_max) {
        let t = (f.log10() - log_min) / span;
        if !(0.0..=1.0).contains(&t) {
            continue;
        }
        let x = rect.left() + t * rect.width();
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            stroke,
        );
        if show_freq_labels {
            painter.text(
                Pos2::new(x, rect.bottom() + 3.0),
                egui::Align2::CENTER_TOP,
                format_freq_tick(f),
                egui::FontId::monospace(theme::GRID_LABEL_PX),
                label_color,
            );
        }
    }

    match view_mode {
        ViewMode::Spectrum => {
            // dB grid lines + labels on the Y axis. When the channel has an
            // SPL offset, labels read in dB SPL (positive); the underlying
            // `db_min..db_max` range stays in dBFS so zoom / scroll math is
            // unchanged. The unit (`dBFS` vs `dB SPL`) is identified in the
            // bottom-left readout — y-axis tick labels are just numbers.
            let db_step = 20.0_f32;
            let db_span = (view.db_max - view.db_min).max(0.0001);
            let mut db = (view.db_min / db_step).ceil() * db_step;
            while db <= view.db_max + 0.001 {
                let t = (db - view.db_min) / db_span;
                let y = rect.bottom() - t * rect.height();
                painter.line_segment(
                    [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                    stroke,
                );
                if show_labels {
                    let label_db = db + spl_offset_db.unwrap_or(0.0);
                    painter.text(
                        Pos2::new(rect.left() - 3.0, y),
                        egui::Align2::RIGHT_CENTER,
                        format!("{:.0}", label_db),
                        egui::FontId::monospace(theme::GRID_LABEL_PX),
                        label_color,
                    );
                }
                db += db_step;
            }
        }
        ViewMode::Waterfall => {
            // Y axis is time: newest row is at the top (t = 0), oldest at the
            // bottom (t = rows_visible * row_period). Ctrl+scroll shrinks
            // rows_visible so the label range collapses to the recent past.
            // Tick positions are chosen in row-space (not second-space) so
            // small shifts in the row-period estimator only update the
            // *label text*, never the label *position* — the eye is much
            // more sensitive to gridlines moving than to "-5.0s" becoming
            // "-4.9s" at a fixed height.
            if !show_labels {
                return;
            }
            let axis = time_axis.unwrap_or(WaterfallTimeAxis {
                row_period_s: 0.1,
                rows_visible: 256.0,
            });
            for (frac, t_s) in time_ticks(axis.rows_visible, axis.row_period_s) {
                let y = rect.top() + frac * rect.height();
                painter.line_segment(
                    [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                    stroke,
                );
                painter.text(
                    Pos2::new(rect.left() - 3.0, y),
                    egui::Align2::RIGHT_CENTER,
                    format_time_tick(t_s),
                    egui::FontId::monospace(theme::GRID_LABEL_PX),
                    label_color,
                );
            }
        }
        ViewMode::Scope
        | ViewMode::SpectrumEmber
        | ViewMode::Goniometer
        | ViewMode::IoTransfer
        | ViewMode::BodeMag
        | ViewMode::Coherence
        | ViewMode::BodePhase
        | ViewMode::GroupDelay
        | ViewMode::Nyquist => {
            // Ember-substrate views paint their own field on pure black —
            // skip the spectrum/waterfall axis grid. Unreachable here
            // because of the early return at the top of the function, but
            // kept for match exhaustiveness.
        }
    }
}

/// Build a log-friendly tick set that stays dense enough to feel useful
/// regardless of the zoom level: decade×{1,2,5} when the view spans a decade
/// or more, and 1-2-5 linear ticks inside a sub-decade window.
pub fn freq_ticks(fmin: f32, fmax: f32) -> Vec<f32> {
    if fmin <= 0.0 || fmax <= fmin {
        return Vec::new();
    }
    let log_min = fmin.log10();
    let log_max = fmax.log10();
    let span = log_max - log_min;

    if span >= 1.0 {
        let mut out = Vec::new();
        let start = log_min.floor() as i32 - 1;
        let end = log_max.ceil() as i32 + 1;
        for d in start..=end {
            let base = 10_f32.powi(d);
            for m in [1.0_f32, 2.0, 5.0] {
                let f = m * base;
                if f >= fmin && f <= fmax {
                    out.push(f);
                }
            }
        }
        out
    } else {
        let target = 6.0_f32;
        let raw = (fmax - fmin) / target;
        if raw <= 0.0 {
            return Vec::new();
        }
        let mag = 10_f32.powf(raw.log10().floor());
        let norm = raw / mag;
        let nice = if norm < 1.5 {
            1.0
        } else if norm < 3.5 {
            2.0
        } else if norm < 7.5 {
            5.0
        } else {
            10.0
        };
        let step = (nice * mag).max(1e-6);
        let mut f = (fmin / step).ceil() * step;
        let mut out = Vec::new();
        while f <= fmax + step * 0.001 {
            out.push(f);
            f += step;
        }
        out
    }
}

pub fn format_freq_tick(f: f32) -> String {
    if f >= 1000.0 {
        let k = f / 1000.0;
        if (k.round() - k).abs() < 1e-3 {
            format!("{:.0}k", k)
        } else {
            format!("{:.1}k", k)
        }
    } else if (f.round() - f).abs() < 1e-3 {
        format!("{:.0}", f)
    } else {
        format!("{:.1}", f)
    }
}

/// Pick waterfall Y-axis ticks in row-space so the label positions are a
/// function of `rows_visible` alone — a jitter in `row_period_s` only
/// affects the displayed seconds, not where the gridline sits. Returns
/// `(frac_from_top, seconds_ago)` pairs: `frac = rows_ago / rows_visible`
/// anchors the line, `seconds_ago = rows_ago * row_period_s` is the label.
/// The 1-2-5 "nice" step is picked in row counts (not seconds) so the step
/// choice only flips when the user scrolls time-zoom, not when the cadence
/// estimator drifts.
fn time_ticks(rows_visible: f32, row_period_s: f32) -> Vec<(f32, f32)> {
    if rows_visible <= 0.0 {
        return vec![(0.0, 0.0)];
    }
    let target = 4.0_f32;
    let raw = rows_visible / target;
    if raw <= 0.0 {
        return vec![(0.0, 0.0)];
    }
    let mag = 10_f32.powf(raw.log10().floor());
    let norm = raw / mag;
    let nice = if norm < 1.5 {
        1.0
    } else if norm < 3.5 {
        2.0
    } else if norm < 7.5 {
        5.0
    } else {
        10.0
    };
    let step_rows = (nice * mag).max(1.0);
    let mut out = vec![(0.0_f32, 0.0_f32)];
    let mut r = step_rows;
    while r <= rows_visible + step_rows * 0.001 {
        let frac = (r / rows_visible).clamp(0.0, 1.0);
        out.push((frac, r * row_period_s));
        r += step_rows;
    }
    out
}

fn format_time_tick(t_s: f32) -> String {
    // Positive values are "age from now", so display as negative seconds for
    // the reader: 0 → "0", 1.5 → "-1.5s".
    if t_s < 0.01 {
        "0".to_string()
    } else if t_s >= 10.0 {
        format!("-{:.0}s", t_s)
    } else {
        format!("-{:.1}s", t_s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── format_freq_tick ──────────────────────────────────────────────

    #[test]
    fn freq_tick_integer_hz() {
        assert_eq!(format_freq_tick(100.0), "100");
        assert_eq!(format_freq_tick(200.0), "200");
        assert_eq!(format_freq_tick(500.0), "500");
    }

    #[test]
    fn freq_tick_fractional_hz() {
        assert_eq!(format_freq_tick(100.5), "100.5");
    }

    #[test]
    fn freq_tick_integer_khz() {
        assert_eq!(format_freq_tick(1000.0), "1k");
        assert_eq!(format_freq_tick(2000.0), "2k");
        assert_eq!(format_freq_tick(10000.0), "10k");
        assert_eq!(format_freq_tick(20000.0), "20k");
    }

    #[test]
    fn freq_tick_fractional_khz() {
        assert_eq!(format_freq_tick(2500.0), "2.5k");
        assert_eq!(format_freq_tick(1500.0), "1.5k");
    }

    #[test]
    fn freq_tick_5khz() {
        assert_eq!(format_freq_tick(5000.0), "5k");
    }

    // ── freq_ticks (log mode) ─────────────────────────────────────────

    #[test]
    fn freq_ticks_log_full_range() {
        let ticks = freq_ticks(20.0, 20000.0);
        assert!(ticks.contains(&100.0));
        assert!(ticks.contains(&1000.0));
        assert!(ticks.contains(&10000.0));
    }

    #[test]
    fn freq_ticks_log_contains_decade_markers() {
        let ticks = freq_ticks(20.0, 20000.0);
        for &f in &[20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0] {
            assert!(ticks.contains(&f), "missing {f}");
        }
    }

    #[test]
    fn freq_ticks_log_narrow_range() {
        let ticks = freq_ticks(900.0, 2000.0);
        assert!(ticks.contains(&1000.0));
        assert!(ticks.contains(&2000.0));
        assert!(!ticks.contains(&500.0));
    }

    // ── time_ticks ────────────────────────────────────────────────────

    #[test]
    fn time_ticks_always_starts_at_zero() {
        let ticks = time_ticks(100.0, 0.1);
        assert_eq!(ticks[0], (0.0, 0.0));
    }

    #[test]
    fn time_ticks_zero_rows() {
        assert_eq!(time_ticks(0.0, 0.1), vec![(0.0, 0.0)]);
    }

    #[test]
    fn time_ticks_positions_track_rows_not_seconds() {
        // Primary guarantee: a small drift in row_period_s must not shift
        // any tick's fractional y position. Only the seconds label moves.
        let a = time_ticks(100.0, 0.100);
        let b = time_ticks(100.0, 0.105);
        assert_eq!(a.len(), b.len());
        for (i, ((fa, _), (fb, _))) in a.iter().zip(b.iter()).enumerate() {
            assert!(
                (fa - fb).abs() < 1e-6,
                "tick {i} position drifted: {fa} vs {fb}"
            );
        }
    }

    #[test]
    fn time_ticks_step_stays_picked_across_period_jitter() {
        // A 5% wobble in row_period must not flip the 1-2-5 step pick.
        let a = time_ticks(100.0, 0.100);
        let b = time_ticks(100.0, 0.095);
        assert_eq!(
            a.len(),
            b.len(),
            "tick count changed with row_period jitter"
        );
    }

    // ── format_time_tick ──────────────────────────────────────────────

    #[test]
    fn format_time_zero() {
        assert_eq!(format_time_tick(0.0), "0");
    }

    #[test]
    fn format_time_small() {
        assert_eq!(format_time_tick(1.5), "-1.5s");
    }

    #[test]
    fn format_time_large() {
        assert_eq!(format_time_tick(30.0), "-30s");
    }
}
