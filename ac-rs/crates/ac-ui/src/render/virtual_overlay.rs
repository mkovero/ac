//! Virtual transfer channel rendering — phase subplot used in Single view.
//!
//! The magnitude curve is rendered by the GPU pipeline like any other
//! channel; this module paints the phase + coherence lane that sits
//! below it when the user focuses a virtual transfer in Single view.
//! Grid/Compare layouts don't draw this lane at all (magnitude only).

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke};

use crate::data::types::{CellView, TransferFrame};
use crate::theme;

const PHASE_MIN_DEG: f32 = -180.0;
const PHASE_MAX_DEG: f32 = 180.0;
const PHASE_TRACE_WIDTH: f32 = 1.2;
const COH_STRIP_PX: f32 = 6.0;
/// Bins below this coherence are hidden from the phase curve — phase is
/// meaningless where the output isn't linearly related to the reference.
/// Segments between the gate and 1.0 are rendered with alpha proportional
/// to the segment's average coherence, so noisy bands fade out instead of
/// jaggedly slashing across the subplot.
const PHASE_COH_GATE: f32 = 0.4;
const PHASE_ALPHA_MIN: f32 = 55.0;
const PHASE_ALPHA_MAX: f32 = 215.0;

/// Fraction of the cell height the spectrum takes when a virtual transfer
/// channel is shown in Single view. The remaining 1 - FRACTION goes to the
/// standalone phase subplot below it (see `draw_phase_subplot`).
pub const SPECTRUM_FRACTION_SINGLE: f32 = 0.60;

/// Standalone phase subplot for the Single-view split layout: own
/// background, own freq gridlines, phase axis, polyline, and coherence
/// strip. Caller picks `show_freq_labels` — usually `true` because the
/// subplot sits at the actual bottom of the cell, so the x-axis labels
/// belong here rather than on the spectrum above.
pub fn draw_phase_subplot(
    painter: &Painter,
    rect: Rect,
    cell_view: &CellView,
    tf: &TransferFrame,
    show_freq_labels: bool,
) {
    painter.rect_filled(
        rect,
        egui::CornerRadius::same(0),
        Color32::from_rgba_unmultiplied(0, 0, 0, 70),
    );

    let label_color = Color32::from_rgb(
        theme::GRID_LABEL[0],
        theme::GRID_LABEL[1],
        theme::GRID_LABEL[2],
    );
    // Muted cyan-grey; the previous near-saturated cyan fought the warm
    // spectrum palette too hard when coherence was poor.
    let phase_color = Color32::from_rgb(160, 200, 210);
    let grid_stroke = Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(255, 140, 80, (0.05 * 255.0) as u8),
    );

    let log_min = cell_view.freq_min.max(1.0).log10();
    let log_max = cell_view.freq_max.max(log_min.exp().max(1.1)).log10();
    let span = (log_max - log_min).max(0.0001);

    for f in crate::render::grid::freq_ticks(cell_view.freq_min, cell_view.freq_max) {
        let t = (f.log10() - log_min) / span;
        if !(0.0..=1.0).contains(&t) {
            continue;
        }
        let x = rect.left() + t * rect.width();
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            grid_stroke,
        );
        if show_freq_labels {
            painter.text(
                Pos2::new(x, rect.bottom() + 3.0),
                Align2::CENTER_TOP,
                crate::render::grid::format_freq_tick(f),
                FontId::monospace(theme::GRID_LABEL_PX),
                label_color,
            );
        }
    }

    if tf.freqs.is_empty() {
        return;
    }

    draw_phase_axis(painter, rect, label_color);
    draw_phase_polyline(
        painter,
        rect,
        cell_view,
        &tf.freqs,
        &tf.phase_deg,
        &tf.coherence,
        phase_color,
    );
    draw_coherence_strip(painter, rect, cell_view, &tf.freqs, &tf.coherence);
}

fn draw_phase_axis(painter: &Painter, rect: Rect, label_color: Color32) {
    // Reference lines at 0°, ±90°, ±180° plus a tick label on the right
    // edge. Kept subtle so they don't compete with the trace itself.
    let zero_stroke = Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(160, 200, 210, 45),
    );
    let tick_stroke = Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(180, 180, 180, 24),
    );

    for (deg, t) in [
        (180.0f32, 0.0f32),
        (90.0, 0.25),
        (0.0, 0.5),
        (-90.0, 0.75),
        (-180.0, 1.0),
    ] {
        let y = rect.top() + t * rect.height();
        let stroke = if deg == 0.0 { zero_stroke } else { tick_stroke };
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            stroke,
        );
        painter.text(
            Pos2::new(rect.right() - 3.0, y),
            Align2::RIGHT_CENTER,
            format!("{deg:+.0}°"),
            FontId::monospace(theme::GRID_LABEL_PX),
            label_color,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_phase_polyline(
    painter: &Painter,
    rect: Rect,
    cell_view: &CellView,
    freqs: &[f32],
    phase_deg: &[f32],
    coherence: &[f32],
    color: Color32,
) {
    let n = freqs.len().min(phase_deg.len()).min(coherence.len());
    if n < 2 {
        return;
    }
    let log_min = cell_view.freq_min.max(1.0).log10();
    let log_max = cell_view
        .freq_max
        .max(cell_view.freq_min.max(1.0) * 1.01)
        .log10();
    let span = (log_max - log_min).max(0.0001);
    let y_span = (PHASE_MAX_DEG - PHASE_MIN_DEG).max(0.0001);

    // Segments break on: coherence-gate crossings, ±180° wraps, or
    // off-screen bins. Wraps used to extend to the rect edges so the
    // jump looked "intentional", but in low-SNR data that just produced
    // vertical slashes. Break cleanly instead and let the gap speak.
    // Each segment remembers its mean coherence so we can fade the
    // stroke — near-gate runs show up as ghostly hints, high-coh runs
    // are solid.
    struct Seg {
        points: Vec<Pos2>,
        coh_sum: f32,
        coh_n: u32,
    }
    let mut segments: Vec<Seg> = Vec::new();
    let mut current = Seg {
        points: Vec::new(),
        coh_sum: 0.0,
        coh_n: 0,
    };
    let mut last_phase: Option<f32> = None;

    let flush = |current: &mut Seg, segments: &mut Vec<Seg>| {
        if current.points.len() >= 2 {
            segments.push(Seg {
                points: std::mem::take(&mut current.points),
                coh_sum: current.coh_sum,
                coh_n: current.coh_n,
            });
        } else {
            current.points.clear();
        }
        current.coh_sum = 0.0;
        current.coh_n = 0;
    };

    for i in 0..n {
        let f = freqs[i];
        if f <= 0.0 || !f.is_finite() {
            continue;
        }
        let v = phase_deg[i];
        if !v.is_finite() {
            continue;
        }
        let coh = coherence[i];
        if !coh.is_finite() || coh < PHASE_COH_GATE {
            flush(&mut current, &mut segments);
            last_phase = None;
            continue;
        }
        let tx = (f.log10() - log_min) / span;
        if !(0.0..=1.0).contains(&tx) {
            continue;
        }
        if let Some(prev_v) = last_phase {
            if (v - prev_v).abs() > 180.0 {
                flush(&mut current, &mut segments);
            }
        }
        let x = rect.left() + tx * rect.width();
        let ty = ((v.clamp(PHASE_MIN_DEG, PHASE_MAX_DEG) - PHASE_MIN_DEG) / y_span).clamp(0.0, 1.0);
        let y = rect.bottom() - ty * rect.height();
        current.points.push(Pos2::new(x, y));
        current.coh_sum += coh.clamp(0.0, 1.0);
        current.coh_n += 1;
        last_phase = Some(v);
    }
    if current.points.len() >= 2 {
        segments.push(current);
    }

    for seg in segments {
        let avg = if seg.coh_n > 0 {
            seg.coh_sum / seg.coh_n as f32
        } else {
            PHASE_COH_GATE
        };
        let t = ((avg - PHASE_COH_GATE) / (1.0 - PHASE_COH_GATE)).clamp(0.0, 1.0);
        // sqrt gives a gentler ramp so mid-coherence segments already
        // read as present rather than near-invisible.
        let alpha = (PHASE_ALPHA_MIN + t.sqrt() * (PHASE_ALPHA_MAX - PHASE_ALPHA_MIN)) as u8;
        let stroke = Stroke::new(
            PHASE_TRACE_WIDTH,
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha),
        );
        painter.add(Shape::line(seg.points, stroke));
    }
}

fn draw_coherence_strip(
    painter: &Painter,
    rect: Rect,
    cell_view: &CellView,
    freqs: &[f32],
    coherence: &[f32],
) {
    let n = freqs.len().min(coherence.len());
    if n < 2 {
        return;
    }
    let log_min = cell_view.freq_min.max(1.0).log10();
    let log_max = cell_view
        .freq_max
        .max(cell_view.freq_min.max(1.0) * 1.01)
        .log10();
    let span = (log_max - log_min).max(0.0001);

    let strip_top = rect.bottom() - COH_STRIP_PX;
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(rect.left(), strip_top), rect.right_bottom()),
        egui::CornerRadius::same(0),
        Color32::from_rgba_unmultiplied(0, 0, 0, 140),
    );

    let mut prev_x: Option<f32> = None;
    for i in 0..n {
        let f = freqs[i];
        if f <= 0.0 || !f.is_finite() {
            continue;
        }
        let c = coherence[i].clamp(0.0, 1.0);
        let tx = (f.log10() - log_min) / span;
        if !(0.0..=1.0).contains(&tx) {
            prev_x = None;
            continue;
        }
        let x = rect.left() + tx * rect.width();
        let x0 = prev_x.unwrap_or(x);
        prev_x = Some(x);
        if (x - x0).abs() < 0.5 {
            continue;
        }
        let color = coherence_color(c);
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(x0, strip_top), Pos2::new(x, rect.bottom())),
            egui::CornerRadius::same(0),
            color,
        );
    }
}

/// Coherence → muted palette: low coh fades to transparent slate, high
/// coh settles on a calm teal. Previous red→yellow→green scale read as
/// alarming in the Single view where the strip sits right next to the
/// phase trace.
fn coherence_color(c: f32) -> Color32 {
    let c = c.clamp(0.0, 1.0);
    if c < PHASE_COH_GATE {
        let t = c / PHASE_COH_GATE;
        let alpha = (40.0 * t) as u8;
        return Color32::from_rgba_unmultiplied(90, 95, 100, alpha);
    }
    let t = ((c - PHASE_COH_GATE) / (1.0 - PHASE_COH_GATE)).clamp(0.0, 1.0);
    let r = 120.0 * (1.0 - t) + 110.0 * t;
    let g = 130.0 * (1.0 - t) + 175.0 * t;
    let b = 135.0 * (1.0 - t) + 165.0 * t;
    let alpha = (120.0 + 95.0 * t) as u8;
    Color32::from_rgba_unmultiplied(r as u8, g as u8, b as u8, alpha)
}
