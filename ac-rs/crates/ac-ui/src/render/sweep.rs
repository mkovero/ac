use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke};

use crate::data::store::SweepState;
use crate::data::types::SweepKind;
use crate::theme;

const THD_FRAC_FREQ: f32 = 0.45;
const GAIN_FRAC_FREQ: f32 = 0.25;

const THD_FRAC_LEVEL: f32 = 0.55;

const SUB_GAP_PX: f32 = 6.0;
const TRACE_WIDTH: f32 = 1.6;

const THD_COLOR: Color32 = Color32::from_rgb(80, 160, 255);
const THDN_COLOR: Color32 = Color32::from_rgb(255, 160, 60);
const GAIN_COLOR: Color32 = Color32::from_rgb(180, 100, 255);
const CLIP_COLOR: Color32 = Color32::from_rgb(255, 60, 60);
const SELECT_COLOR: Color32 = Color32::from_rgb(255, 255, 100);

const THD_MIN_PCT: f32 = 0.001;
const THD_MAX_PCT: f32 = 100.0;
const GAIN_MIN_DB: f32 = -20.0;
const GAIN_MAX_DB: f32 = 10.0;
const SPEC_MIN_DB: f32 = -140.0;
const SPEC_MAX_DB: f32 = 0.0;

pub struct SweepSubRects {
    pub thd: Rect,
    pub gain: Rect,
    pub spectrum: Option<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepHitPanel {
    Thd,
    Gain,
    SpectrumDetail,
}

pub fn split_cell(rect: Rect, kind: SweepKind) -> SweepSubRects {
    let h = rect.height();
    match kind {
        SweepKind::Frequency => {
            let thd_h = h * THD_FRAC_FREQ - SUB_GAP_PX * 0.5;
            let gain_h = h * GAIN_FRAC_FREQ - SUB_GAP_PX;
            let spec_h = h - thd_h - gain_h - SUB_GAP_PX * 2.0;
            let thd = Rect::from_min_size(
                Pos2::new(rect.left(), rect.top()),
                egui::vec2(rect.width(), thd_h.max(1.0)),
            );
            let gain = Rect::from_min_size(
                Pos2::new(rect.left(), thd.bottom() + SUB_GAP_PX),
                egui::vec2(rect.width(), gain_h.max(1.0)),
            );
            let spec = Rect::from_min_size(
                Pos2::new(rect.left(), gain.bottom() + SUB_GAP_PX),
                egui::vec2(rect.width(), spec_h.max(1.0)),
            );
            SweepSubRects {
                thd,
                gain,
                spectrum: Some(spec),
            }
        }
        SweepKind::Level => {
            let thd_h = h * THD_FRAC_LEVEL - SUB_GAP_PX * 0.5;
            let gain_h = h - thd_h - SUB_GAP_PX;
            let thd = Rect::from_min_size(
                Pos2::new(rect.left(), rect.top()),
                egui::vec2(rect.width(), thd_h.max(1.0)),
            );
            let gain = Rect::from_min_size(
                Pos2::new(rect.left(), thd.bottom() + SUB_GAP_PX),
                egui::vec2(rect.width(), gain_h.max(1.0)),
            );
            SweepSubRects {
                thd,
                gain,
                spectrum: None,
            }
        }
    }
}

pub fn hit_test(rect: Rect, cursor: Pos2, kind: SweepKind) -> Option<(SweepHitPanel, f32)> {
    let sub = split_cell(rect, kind);
    if sub.thd.contains(cursor) {
        let ny = 1.0 - (cursor.y - sub.thd.top()) / sub.thd.height().max(1.0);
        let log_min = THD_MIN_PCT.log10();
        let log_max = THD_MAX_PCT.log10();
        let val = 10.0_f32.powf(log_min + ny.clamp(0.0, 1.0) * (log_max - log_min));
        Some((SweepHitPanel::Thd, val))
    } else if sub.gain.contains(cursor) {
        let ny = 1.0 - (cursor.y - sub.gain.top()) / sub.gain.height().max(1.0);
        let val = GAIN_MIN_DB + ny.clamp(0.0, 1.0) * (GAIN_MAX_DB - GAIN_MIN_DB);
        Some((SweepHitPanel::Gain, val))
    } else if let Some(spec) = sub.spectrum {
        if spec.contains(cursor) {
            let ny = 1.0 - (cursor.y - spec.top()) / spec.height().max(1.0);
            let val = SPEC_MIN_DB + ny.clamp(0.0, 1.0) * (SPEC_MAX_DB - SPEC_MIN_DB);
            Some((SweepHitPanel::SpectrumDetail, val))
        } else {
            None
        }
    } else {
        None
    }
}

pub fn nearest_point(
    rect: Rect,
    kind: SweepKind,
    state: &SweepState,
    cursor: Pos2,
) -> Option<usize> {
    if state.points.is_empty() {
        return None;
    }
    let sub = split_cell(rect, kind);
    let panel = sub.thd;
    if !panel.contains(cursor) && !sub.gain.contains(cursor) {
        if let Some(spec) = sub.spectrum {
            if !spec.contains(cursor) {
                return None;
            }
        } else {
            return None;
        }
    }

    let cx = cursor.x;
    let mut best_idx = 0;
    let mut best_dist = f32::INFINITY;

    for (i, pt) in state.points.iter().enumerate() {
        let x_val = match kind {
            SweepKind::Frequency => pt.fundamental_hz,
            SweepKind::Level => pt.out_dbu.unwrap_or(pt.drive_db),
        };
        let px = x_to_pixel(panel, kind, x_val);
        let dist = (px - cx).abs();
        if dist < best_dist {
            best_dist = dist;
            best_idx = i;
        }
    }
    Some(best_idx)
}

pub fn draw(
    painter: &Painter,
    rect: Rect,
    kind: SweepKind,
    state: &SweepState,
    selected_idx: Option<usize>,
) {
    let sub = split_cell(rect, kind);
    let label_color = Color32::from_rgb(
        theme::GRID_LABEL[0],
        theme::GRID_LABEL[1],
        theme::GRID_LABEL[2],
    );

    draw_thd_chrome(painter, sub.thd, kind, label_color);
    draw_gain_chrome(painter, sub.gain, kind, label_color);
    if let Some(spec) = sub.spectrum {
        draw_spec_chrome(painter, spec, label_color);
    }

    if state.points.is_empty() {
        let status = if state.done.is_some() {
            "No data received."
        } else {
            "Waiting for sweep data\u{2026}"
        };
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            status,
            FontId::monospace(14.0),
            label_color,
        );
        return;
    }

    draw_thd_traces(painter, sub.thd, kind, state);
    draw_gain_trace(painter, sub.gain, kind, state);

    if let Some(sel) = selected_idx {
        if sel < state.points.len() {
            let pt = &state.points[sel];
            let x_val = match kind {
                SweepKind::Frequency => pt.fundamental_hz,
                SweepKind::Level => pt.out_dbu.unwrap_or(pt.drive_db),
            };
            let px = x_to_pixel(sub.thd, kind, x_val);
            let cursor_stroke = Stroke::new(1.0, SELECT_COLOR);
            painter.line_segment(
                [
                    Pos2::new(px, rect.top()),
                    Pos2::new(px, rect.bottom()),
                ],
                cursor_stroke,
            );

            if let Some(spec_rect) = sub.spectrum {
                draw_spectrum_detail(painter, spec_rect, pt);
            }
        }
    }

    let status_text = match &state.done {
        Some(done) => {
            let xrun_s = if done.xruns > 0 {
                format!("  !! {} xrun(s)", done.xruns)
            } else {
                String::new()
            };
            format!("Complete: {} points{xrun_s}", done.n_points)
        }
        None => format!("Sweeping\u{2026} {} points", state.points.len()),
    };
    painter.text(
        Pos2::new(rect.right() - 8.0, rect.top() + 4.0),
        Align2::RIGHT_TOP,
        status_text,
        FontId::monospace(theme::GRID_LABEL_PX),
        label_color,
    );

    if let Some(sel) = selected_idx {
        if sel < state.points.len() {
            draw_readout(painter, rect, &state.points[sel], label_color);
        }
    }
}

fn x_to_pixel(panel: Rect, kind: SweepKind, val: f32) -> f32 {
    match kind {
        SweepKind::Frequency => {
            if val <= 0.0 {
                return panel.left();
            }
            let log_min = 20.0_f32.log10();
            let log_max = 20000.0_f32.log10();
            let t = (val.log10() - log_min) / (log_max - log_min);
            panel.left() + t.clamp(0.0, 1.0) * panel.width()
        }
        SweepKind::Level => {
            let x_min = -60.0_f32;
            let x_max = 10.0_f32;
            let t = (val - x_min) / (x_max - x_min);
            panel.left() + t.clamp(0.0, 1.0) * panel.width()
        }
    }
}

fn draw_thd_chrome(painter: &Painter, rect: Rect, kind: SweepKind, label_color: Color32) {
    let stroke = Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(255, 140, 80, (0.05 * 255.0) as u8),
    );
    let frame_stroke = Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(255, 140, 80, (0.18 * 255.0) as u8),
    );
    painter.rect_stroke(rect, egui::CornerRadius::same(0), frame_stroke, egui::StrokeKind::Inside);

    draw_x_grid(painter, rect, kind, stroke, label_color, false);

    let log_min = THD_MIN_PCT.log10();
    let log_max = THD_MAX_PCT.log10();
    let decades: [f32; 6] = [0.001, 0.01, 0.1, 1.0, 10.0, 100.0];
    for &d in &decades {
        let t = (d.log10() - log_min) / (log_max - log_min);
        if !(0.0..=1.0).contains(&t) {
            continue;
        }
        let y = rect.bottom() - t * rect.height();
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            stroke,
        );
        let label = if d >= 1.0 {
            format!("{:.0}%", d)
        } else if d >= 0.01 {
            format!("{:.2}%", d)
        } else {
            format!("{:.3}%", d)
        };
        painter.text(
            Pos2::new(rect.left() - 3.0, y),
            Align2::RIGHT_CENTER,
            label,
            FontId::monospace(theme::GRID_LABEL_PX),
            label_color,
        );
    }
    painter.text(
        Pos2::new(rect.left() + 6.0, rect.top() + 3.0),
        Align2::LEFT_TOP,
        "THD / THD+N",
        FontId::monospace(theme::GRID_LABEL_PX),
        label_color,
    );
}

fn draw_gain_chrome(painter: &Painter, rect: Rect, kind: SweepKind, label_color: Color32) {
    let stroke = Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(255, 140, 80, (0.05 * 255.0) as u8),
    );
    let frame_stroke = Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(255, 140, 80, (0.18 * 255.0) as u8),
    );
    painter.rect_stroke(rect, egui::CornerRadius::same(0), frame_stroke, egui::StrokeKind::Inside);

    draw_x_grid(painter, rect, kind, stroke, label_color, true);

    let y_span = GAIN_MAX_DB - GAIN_MIN_DB;
    let step = 5.0_f32;
    let mut y = (GAIN_MIN_DB / step).ceil() * step;
    while y <= GAIN_MAX_DB + 0.001 {
        let t = (y - GAIN_MIN_DB) / y_span;
        let py = rect.bottom() - t * rect.height();
        let s = if (y.abs()) < 0.001 {
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 140, 80, (0.12 * 255.0) as u8))
        } else {
            stroke
        };
        painter.line_segment(
            [Pos2::new(rect.left(), py), Pos2::new(rect.right(), py)],
            s,
        );
        painter.text(
            Pos2::new(rect.left() - 3.0, py),
            Align2::RIGHT_CENTER,
            format!("{:.0}", y),
            FontId::monospace(theme::GRID_LABEL_PX),
            label_color,
        );
        y += step;
    }
    painter.text(
        Pos2::new(rect.left() + 6.0, rect.top() + 3.0),
        Align2::LEFT_TOP,
        "Gain dB",
        FontId::monospace(theme::GRID_LABEL_PX),
        label_color,
    );
}

fn draw_spec_chrome(painter: &Painter, rect: Rect, label_color: Color32) {
    let stroke = Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(255, 140, 80, (0.05 * 255.0) as u8),
    );
    let frame_stroke = Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(255, 140, 80, (0.18 * 255.0) as u8),
    );
    painter.rect_stroke(rect, egui::CornerRadius::same(0), frame_stroke, egui::StrokeKind::Inside);

    let log_min = 20.0_f32.log10();
    let log_max = 20000.0_f32.log10();
    let span = log_max - log_min;
    for f in crate::render::grid::freq_ticks(20.0, 20000.0) {
        let t = (f.log10() - log_min) / span;
        if !(0.0..=1.0).contains(&t) {
            continue;
        }
        let x = rect.left() + t * rect.width();
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            stroke,
        );
        painter.text(
            Pos2::new(x, rect.bottom() + 3.0),
            Align2::CENTER_TOP,
            crate::render::grid::format_freq_tick(f),
            FontId::monospace(theme::GRID_LABEL_PX),
            label_color,
        );
    }

    let y_span = SPEC_MAX_DB - SPEC_MIN_DB;
    let step = 20.0_f32;
    let mut y = (SPEC_MIN_DB / step).ceil() * step;
    while y <= SPEC_MAX_DB + 0.001 {
        let t = (y - SPEC_MIN_DB) / y_span;
        let py = rect.bottom() - t * rect.height();
        painter.line_segment(
            [Pos2::new(rect.left(), py), Pos2::new(rect.right(), py)],
            stroke,
        );
        painter.text(
            Pos2::new(rect.left() - 3.0, py),
            Align2::RIGHT_CENTER,
            format!("{:.0}", y),
            FontId::monospace(theme::GRID_LABEL_PX),
            label_color,
        );
        y += step;
    }
    painter.text(
        Pos2::new(rect.left() + 6.0, rect.top() + 3.0),
        Align2::LEFT_TOP,
        "Spectrum (selected)",
        FontId::monospace(theme::GRID_LABEL_PX),
        label_color,
    );
}

fn draw_x_grid(
    painter: &Painter,
    rect: Rect,
    kind: SweepKind,
    stroke: Stroke,
    label_color: Color32,
    show_labels: bool,
) {
    match kind {
        SweepKind::Frequency => {
            let log_min = 20.0_f32.log10();
            let log_max = 20000.0_f32.log10();
            let span = log_max - log_min;
            for f in crate::render::grid::freq_ticks(20.0, 20000.0) {
                let t = (f.log10() - log_min) / span;
                if !(0.0..=1.0).contains(&t) {
                    continue;
                }
                let x = rect.left() + t * rect.width();
                painter.line_segment(
                    [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                    stroke,
                );
                if show_labels {
                    painter.text(
                        Pos2::new(x, rect.bottom() + 3.0),
                        Align2::CENTER_TOP,
                        crate::render::grid::format_freq_tick(f),
                        FontId::monospace(theme::GRID_LABEL_PX),
                        label_color,
                    );
                }
            }
        }
        SweepKind::Level => {
            let x_min = -60.0_f32;
            let x_max = 10.0_f32;
            let x_span = x_max - x_min;
            let step = 10.0_f32;
            let mut v = (x_min / step).ceil() * step;
            while v <= x_max + 0.001 {
                let t = (v - x_min) / x_span;
                let x = rect.left() + t * rect.width();
                painter.line_segment(
                    [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                    stroke,
                );
                if show_labels {
                    painter.text(
                        Pos2::new(x, rect.bottom() + 3.0),
                        Align2::CENTER_TOP,
                        format!("{:.0}", v),
                        FontId::monospace(theme::GRID_LABEL_PX),
                        label_color,
                    );
                }
                v += step;
            }
        }
    }
}

fn draw_thd_traces(painter: &Painter, rect: Rect, kind: SweepKind, state: &SweepState) {
    let n = state.points.len();
    if n < 1 {
        return;
    }

    let log_min = THD_MIN_PCT.log10();
    let log_max = THD_MAX_PCT.log10();
    let log_span = log_max - log_min;

    let mut thd_pts = Vec::with_capacity(n);
    let mut thdn_pts = Vec::with_capacity(n);

    for pt in &state.points {
        let px = x_to_pixel(rect, kind, match kind {
            SweepKind::Frequency => pt.fundamental_hz,
            SweepKind::Level => pt.out_dbu.unwrap_or(pt.drive_db),
        });

        let thd_t = (pt.thd_pct.max(THD_MIN_PCT).log10() - log_min) / log_span;
        let thd_y = rect.bottom() - thd_t.clamp(0.0, 1.0) * rect.height();
        thd_pts.push(Pos2::new(px, thd_y));

        let thdn_t = (pt.thdn_pct.max(THD_MIN_PCT).log10() - log_min) / log_span;
        let thdn_y = rect.bottom() - thdn_t.clamp(0.0, 1.0) * rect.height();
        thdn_pts.push(Pos2::new(px, thdn_y));

        if pt.clipping {
            let size = 4.0;
            let cx = px;
            let cy = thd_y;
            painter.line_segment(
                [
                    Pos2::new(cx - size, cy - size),
                    Pos2::new(cx + size, cy + size),
                ],
                Stroke::new(1.5, CLIP_COLOR),
            );
            painter.line_segment(
                [
                    Pos2::new(cx - size, cy + size),
                    Pos2::new(cx + size, cy - size),
                ],
                Stroke::new(1.5, CLIP_COLOR),
            );
        }
    }

    if thd_pts.len() >= 2 {
        painter.add(Shape::line(thd_pts, Stroke::new(TRACE_WIDTH, THD_COLOR)));
    }
    if thdn_pts.len() >= 2 {
        painter.add(Shape::line(thdn_pts, Stroke::new(TRACE_WIDTH, THDN_COLOR)));
    }
}

fn draw_gain_trace(painter: &Painter, rect: Rect, kind: SweepKind, state: &SweepState) {
    let n = state.points.len();
    if n < 1 {
        return;
    }
    let y_span = GAIN_MAX_DB - GAIN_MIN_DB;
    let mut pts = Vec::with_capacity(n);
    for pt in &state.points {
        let Some(gain) = pt.gain_db else { continue };
        let px = x_to_pixel(rect, kind, match kind {
            SweepKind::Frequency => pt.fundamental_hz,
            SweepKind::Level => pt.out_dbu.unwrap_or(pt.drive_db),
        });
        let t = (gain - GAIN_MIN_DB) / y_span;
        let y = rect.bottom() - t.clamp(0.0, 1.0) * rect.height();
        pts.push(Pos2::new(px, y));
    }
    if pts.len() >= 2 {
        painter.add(Shape::line(pts, Stroke::new(TRACE_WIDTH, GAIN_COLOR)));
    }
}

fn draw_spectrum_detail(
    painter: &Painter,
    rect: Rect,
    pt: &crate::data::types::SweepPoint,
) {
    if pt.spectrum.is_empty() || pt.freqs.is_empty() {
        return;
    }
    let n = pt.spectrum.len().min(pt.freqs.len());
    let log_min = 20.0_f32.log10();
    let log_max = 20000.0_f32.log10();
    let span = log_max - log_min;
    let y_span = SPEC_MAX_DB - SPEC_MIN_DB;

    let trace_color = Color32::from_rgb(80, 160, 255);
    let mut pts = Vec::with_capacity(n);
    for i in 0..n {
        let f = pt.freqs[i];
        if f <= 0.0 || !f.is_finite() {
            continue;
        }
        let tx = (f.log10() - log_min) / span;
        if !(0.0..=1.0).contains(&tx) {
            continue;
        }
        let v_db = 20.0 * pt.spectrum[i].max(1e-12).log10();
        let ty = (v_db - SPEC_MIN_DB) / y_span;
        let x = rect.left() + tx * rect.width();
        let y = rect.bottom() - ty.clamp(0.0, 1.0) * rect.height();
        pts.push(Pos2::new(x, y));
    }
    if pts.len() >= 2 {
        painter.add(Shape::line(pts, Stroke::new(1.2, trace_color)));
    }

    let harmonic_color = Color32::from_rgb(255, 180, 50);
    for &[hz, _amp] in &pt.harmonic_levels {
        if hz <= 0.0 {
            continue;
        }
        let tx = (hz.log10() - log_min) / span;
        if !(0.0..=1.0).contains(&tx) {
            continue;
        }
        let x = rect.left() + tx * rect.width();
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0, harmonic_color.gamma_multiply(0.4)),
        );
    }
}

fn draw_readout(
    painter: &Painter,
    rect: Rect,
    pt: &crate::data::types::SweepPoint,
    label_color: Color32,
) {
    let mut parts = Vec::new();
    parts.push(format!("{:.1} Hz", pt.fundamental_hz));
    parts.push(format!("THD {:.4}%", pt.thd_pct));
    parts.push(format!("THD+N {:.4}%", pt.thdn_pct));
    if let Some(g) = pt.gain_db {
        parts.push(format!("Gain {:+.2} dB", g));
    }
    parts.push(format!("Fund {:.1} dBFS", pt.fundamental_dbfs));
    if let Some(dbu) = pt.in_dbu {
        parts.push(format!("In {:+.2} dBu", dbu));
    }
    if let Some(dbu) = pt.out_dbu {
        parts.push(format!("Out {:+.2} dBu", dbu));
    }
    let text = parts.join("   ");
    painter.text(
        Pos2::new(rect.left() + 8.0, rect.bottom() - 4.0),
        Align2::LEFT_BOTTOM,
        text,
        FontId::monospace(theme::GRID_LABEL_PX),
        label_color,
    );
}
