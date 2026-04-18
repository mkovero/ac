//! Virtual transfer channel overlays on top of the regular spectrum cell.
//!
//! The magnitude curve is rendered by the GPU pipeline like any other
//! channel; this module paints the extra lanes that a virtual channel
//! needs — phase polyline on a right-side ±180° axis, and a thin
//! coherence strip along the bottom that fades to grey below 0.5.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke};

use crate::data::types::{CellView, TransferFrame};
use crate::theme;

const PHASE_MIN_DEG: f32 = -180.0;
const PHASE_MAX_DEG: f32 = 180.0;
const PHASE_TRACE_WIDTH: f32 = 1.2;
const COH_STRIP_PX: f32 = 6.0;

/// Paint phase + coherence on top of an already-rendered magnitude cell.
pub fn draw(painter: &Painter, rect: Rect, cell_view: &CellView, tf: &TransferFrame) {
    if tf.freqs.is_empty() {
        return;
    }

    let label_color = Color32::from_rgb(
        theme::GRID_LABEL[0],
        theme::GRID_LABEL[1],
        theme::GRID_LABEL[2],
    );
    let phase_color = Color32::from_rgb(255, 205, 80);

    draw_phase_axis(painter, rect, label_color);
    draw_phase_polyline(
        painter,
        rect,
        cell_view,
        &tf.freqs,
        &tf.phase_deg,
        phase_color,
    );
    draw_coherence_strip(painter, rect, cell_view, &tf.freqs, &tf.coherence);
}

fn draw_phase_axis(painter: &Painter, rect: Rect, label_color: Color32) {
    // Three ticks on the right edge: +180°, 0°, -180°. Keep subtle — the
    // primary axis for the cell is still the magnitude scale on the left.
    for (deg, t) in [(180.0f32, 0.0f32), (0.0, 0.5), (-180.0, 1.0)] {
        let y = rect.top() + t * rect.height();
        painter.text(
            Pos2::new(rect.right() - 3.0, y),
            Align2::RIGHT_CENTER,
            format!("{deg:+.0}°"),
            FontId::monospace(theme::GRID_LABEL_PX),
            label_color,
        );
    }
}

fn draw_phase_polyline(
    painter: &Painter,
    rect: Rect,
    cell_view: &CellView,
    freqs: &[f32],
    phase_deg: &[f32],
    color: Color32,
) {
    let n = freqs.len().min(phase_deg.len());
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

    // Break segments at ±180° wrap boundaries to avoid long diagonal jumps
    // when the unwrapped phase crosses the axis.
    let mut segments: Vec<Vec<Pos2>> = Vec::new();
    let mut current: Vec<Pos2> = Vec::new();
    let mut last_v: Option<f32> = None;

    for i in 0..n {
        let f = freqs[i];
        if f <= 0.0 || !f.is_finite() {
            continue;
        }
        let v = phase_deg[i];
        if !v.is_finite() {
            continue;
        }
        let tx = (f.log10() - log_min) / span;
        if !(0.0..=1.0).contains(&tx) {
            continue;
        }
        if let Some(lv) = last_v {
            if (v - lv).abs() > 180.0 {
                if current.len() >= 2 {
                    segments.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
            }
        }
        last_v = Some(v);

        let ty = ((v.clamp(PHASE_MIN_DEG, PHASE_MAX_DEG) - PHASE_MIN_DEG) / y_span).clamp(0.0, 1.0);
        let x = rect.left() + tx * rect.width();
        let y = rect.bottom() - ty * rect.height();
        current.push(Pos2::new(x, y));
    }
    if current.len() >= 2 {
        segments.push(current);
    }
    for seg in segments {
        painter.add(Shape::line(seg, Stroke::new(PHASE_TRACE_WIDTH, color)));
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
    // Background so the strip reads as a distinct band even when the
    // magnitude curve is painted over the same pixels.
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(rect.left(), strip_top), rect.right_bottom()),
        egui::CornerRadius::same(0),
        Color32::from_rgba_unmultiplied(0, 0, 0, 160),
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

/// Coherence 0..1 → red → yellow → green. Values below 0.5 fade toward a
/// desaturated grey so low-coherence bands read as "don't trust this".
fn coherence_color(c: f32) -> Color32 {
    let c = c.clamp(0.0, 1.0);
    let (r, g, b) = if c < 0.5 {
        // grey → red at c=0.5
        let t = c / 0.5;
        let base = 90.0 * (1.0 - t);
        (base + t * 220.0, base + t * 50.0, base + t * 50.0)
    } else {
        // red → yellow → green at c=1.0
        let t = (c - 0.5) / 0.5;
        (220.0 * (1.0 - t) + 80.0 * t, 50.0 + t * 180.0, 50.0 * (1.0 - t))
    };
    Color32::from_rgb(r as u8, g as u8, b as u8)
}
