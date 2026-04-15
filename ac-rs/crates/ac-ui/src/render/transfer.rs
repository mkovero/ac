//! Live H1 transfer function view — pure egui painter.
//!
//! Splits the plot cell into three stacked sub-panels (magnitude, phase,
//! coherence) and draws them from the latest `TransferFrame`. The frequency
//! axis is shared with the spectrum renderer via the meas channel's
//! `CellView`, so `l`-cycle into Transfer keeps whatever freq zoom the user
//! already had.
//!
//! No wgpu: polyline count is small (≤ 2000 points × 3 panels), egui shapes
//! are fast enough and let all labels / colours share the existing overlay
//! layer.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke};

use crate::data::types::{CellView, TransferFrame};
use crate::theme;

/// Relative vertical split (top-to-bottom) of the three sub-panels within a
/// single plot cell. Magnitude gets the bulk because it is the most commonly
/// interrogated band; phase + coherence share the bottom half.
const MAG_FRAC: f32 = 0.55;
const PHASE_FRAC: f32 = 0.25;
// Coherence frac = 1.0 - MAG - PHASE (= 0.20).
const SUB_GAP_PX: f32 = 6.0;

const MAG_MIN_DB: f32 = -40.0;
const MAG_MAX_DB: f32 = 20.0;
const PHASE_MIN_DEG: f32 = -180.0;
const PHASE_MAX_DEG: f32 = 180.0;
const COH_MIN: f32 = 0.0;
const COH_MAX: f32 = 1.0;

const TRACE_WIDTH: f32 = 1.6;

pub struct SubRects {
    pub mag: Rect,
    pub phase: Rect,
    pub coh: Rect,
}

pub fn split_cell(rect: Rect) -> SubRects {
    let h = rect.height();
    let mag_h = (h * MAG_FRAC) - SUB_GAP_PX * 0.5;
    let phase_h = (h * PHASE_FRAC) - SUB_GAP_PX * 0.5;
    let coh_h = h - mag_h - phase_h - SUB_GAP_PX * 2.0;
    let top = rect.top();
    let mag = Rect::from_min_size(
        Pos2::new(rect.left(), top),
        egui::vec2(rect.width(), mag_h.max(1.0)),
    );
    let phase_top = mag.bottom() + SUB_GAP_PX;
    let phase = Rect::from_min_size(
        Pos2::new(rect.left(), phase_top),
        egui::vec2(rect.width(), phase_h.max(1.0)),
    );
    let coh_top = phase.bottom() + SUB_GAP_PX;
    let coh = Rect::from_min_size(
        Pos2::new(rect.left(), coh_top),
        egui::vec2(rect.width(), coh_h.max(1.0)),
    );
    SubRects { mag, phase, coh }
}

pub fn draw(
    painter: &Painter,
    rect: Rect,
    cell_view: &CellView,
    frame: Option<&TransferFrame>,
    trace_color: [f32; 4],
) {
    let sub = split_cell(rect);
    let label_color = Color32::from_rgb(
        theme::GRID_LABEL[0],
        theme::GRID_LABEL[1],
        theme::GRID_LABEL[2],
    );
    draw_panel_chrome(painter, sub.mag, cell_view, "dB", MAG_MIN_DB, MAG_MAX_DB, 20.0, label_color, false);
    draw_panel_chrome(painter, sub.phase, cell_view, "deg", PHASE_MIN_DEG, PHASE_MAX_DEG, 90.0, label_color, false);
    draw_panel_chrome(painter, sub.coh, cell_view, "coh", COH_MIN, COH_MAX, 0.25, label_color, true);

    let Some(frame) = frame else { return };
    if frame.freqs.is_empty() {
        return;
    }
    let color = Color32::from_rgb(
        (trace_color[0] * 255.0) as u8,
        (trace_color[1] * 255.0) as u8,
        (trace_color[2] * 255.0) as u8,
    );
    draw_polyline(painter, sub.mag, cell_view, &frame.freqs, &frame.magnitude_db, MAG_MIN_DB, MAG_MAX_DB, color);
    draw_polyline(painter, sub.phase, cell_view, &frame.freqs, &frame.phase_deg, PHASE_MIN_DEG, PHASE_MAX_DEG, color);
    draw_polyline(painter, sub.coh, cell_view, &frame.freqs, &frame.coherence, COH_MIN, COH_MAX, color);
}

#[allow(clippy::too_many_arguments)]
fn draw_panel_chrome(
    painter: &Painter,
    rect: Rect,
    cell_view: &CellView,
    unit: &str,
    y_min: f32,
    y_max: f32,
    y_step: f32,
    label_color: Color32,
    show_freq_labels: bool,
) {
    let stroke = Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(255, 140, 80, (0.05 * 255.0) as u8),
    );
    let frame_stroke = Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(255, 140, 80, (0.18 * 255.0) as u8),
    );
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(0),
        frame_stroke,
        egui::StrokeKind::Inside,
    );

    // Frequency grid lines (log-x), shared across all three panels.
    let log_min = cell_view.freq_min.max(1.0).log10();
    let log_max = cell_view
        .freq_max
        .max(cell_view.freq_min.max(1.0) * 1.01)
        .log10();
    let span = (log_max - log_min).max(0.0001);
    for f in crate::render::grid::freq_ticks(cell_view.freq_min, cell_view.freq_max) {
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
                Align2::CENTER_TOP,
                crate::render::grid::format_freq_tick(f),
                FontId::monospace(theme::GRID_LABEL_PX),
                label_color,
            );
        }
    }

    // Y grid lines + labels using the panel's own units.
    let y_span = (y_max - y_min).max(0.0001);
    let mut y = (y_min / y_step).ceil() * y_step;
    while y <= y_max + 0.001 {
        let t = (y - y_min) / y_span;
        let px = rect.bottom() - t * rect.height();
        painter.line_segment(
            [Pos2::new(rect.left(), px), Pos2::new(rect.right(), px)],
            stroke,
        );
        let label = if unit == "coh" {
            format!("{:.2}", y)
        } else {
            format!("{:.0}", y)
        };
        painter.text(
            Pos2::new(rect.left() - 3.0, px),
            Align2::RIGHT_CENTER,
            label,
            FontId::monospace(theme::GRID_LABEL_PX),
            label_color,
        );
        y += y_step;
    }

    // Panel name in the top-left corner so the reader knows which axis they
    // are looking at without having to guess from the numbers.
    let name = match unit {
        "dB" => "|H| dB",
        "deg" => "phase",
        "coh" => "coherence",
        _ => unit,
    };
    painter.text(
        Pos2::new(rect.left() + 6.0, rect.top() + 3.0),
        Align2::LEFT_TOP,
        name,
        FontId::monospace(theme::GRID_LABEL_PX),
        label_color,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_polyline(
    painter: &Painter,
    rect: Rect,
    cell_view: &CellView,
    freqs: &[f32],
    values: &[f32],
    y_min: f32,
    y_max: f32,
    color: Color32,
) {
    let n = freqs.len().min(values.len());
    if n < 2 {
        return;
    }
    let log_min = cell_view.freq_min.max(1.0).log10();
    let log_max = cell_view
        .freq_max
        .max(cell_view.freq_min.max(1.0) * 1.01)
        .log10();
    let span = (log_max - log_min).max(0.0001);
    let y_span = (y_max - y_min).max(0.0001);

    let mut pts: Vec<Pos2> = Vec::with_capacity(n);
    for i in 0..n {
        let f = freqs[i];
        if f <= 0.0 || !f.is_finite() {
            continue;
        }
        let v = values[i];
        if !v.is_finite() {
            continue;
        }
        let tx = (f.log10() - log_min) / span;
        if !(0.0..=1.0).contains(&tx) {
            continue;
        }
        let ty = ((v.clamp(y_min, y_max) - y_min) / y_span).clamp(0.0, 1.0);
        let x = rect.left() + tx * rect.width();
        let y = rect.bottom() - ty * rect.height();
        pts.push(Pos2::new(x, y));
    }
    if pts.len() >= 2 {
        painter.add(Shape::line(pts, Stroke::new(TRACE_WIDTH, color)));
    }
}
