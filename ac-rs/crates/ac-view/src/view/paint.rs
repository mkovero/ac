//! The drawing primitives every view shares: the scene-point → polyline
//! mapping, a pane's gridlines and axis labels, the input meters, and the
//! one-line text helper the rest of this module tree writes its labels
//! through.
//!
//! Nothing here decides *what* to draw. Every point arrives already
//! normalized by `ac-scene` and every string arrives already formatted by
//! it; this module maps and paints, which is the whole of `ac-view`'s job
//! (`computes_nothing`).

use egui::{Align2, Color32, FontId, Painter, Pos2, Stroke};

use crate::geometry::{scene_to_screen, Viewport};

use super::palette::{COLOR_LABEL, COLOR_SIGNAL, COLOR_STRUCTURAL};

/// Dash geometry for every dashed polyline this crate paints — one
/// on/off pair in one place, so the live-solid/stored-dashed convention
/// cannot drift between the spectrum, transfer and IR panes the way it
/// could while each call site carried its own literals.
const DASH_ON: f32 = 6.0;
const DASH_OFF: f32 = 4.0;

/// A one-line label in the default font. The font choice is not a
/// per-call decision anywhere in this crate — only the banner and the
/// fault indicator size text deliberately, and they say so by calling
/// `painter.text` with an explicit [`FontId`] themselves.
pub fn text(painter: &Painter, pos: Pos2, align: Align2, text: impl ToString, color: Color32) {
    painter.text(pos, align, text, FontId::default(), color);
}

/// Map one `ac-scene` segment's normalized points into `vp`.
fn segment_points(segment: &[(f64, f64)], vp: Viewport) -> Vec<Pos2> {
    segment
        .iter()
        .map(|&pt| {
            let (x, y) = scene_to_screen(pt, vp);
            egui::pos2(x, y)
        })
        .collect()
}

/// Paint one already-mapped polyline, solid or dashed.
///
/// Fewer than two points paints nothing: a one-point "line" is invisible
/// either way, and dropping it keeps the shape list a faithful count of
/// what is actually on screen for the paint-level tests that read it.
/// This guard used to exist only in the transfer view's copy of this
/// loop; unifying it is the point of having one copy.
fn polyline(painter: &Painter, points: Vec<Pos2>, stroke: Stroke, dashed: bool) {
    if points.len() < 2 {
        return;
    }
    if dashed {
        let mut shapes = Vec::new();
        egui::Shape::dashed_line_many(&points, stroke, DASH_ON, DASH_OFF, &mut shapes);
        painter.extend(shapes);
    } else {
        painter.add(egui::Shape::line(points, stroke));
    }
}

/// Draw one trace's segments in a pane — one polyline per segment, so a
/// coherence gap is the absence of a segment (D5), never a line to the
/// floor. This crate decides nothing about where a gap goes; it stops
/// drawing where `ac-scene` stopped emitting. `dashed` distinguishes a
/// stored run's provenance (#321) from the live trace.
pub fn draw_trace(
    painter: &Painter,
    trace: &ac_scene::Trace,
    vp: Viewport,
    stroke: Stroke,
    dashed: bool,
) {
    for segment in &trace.segments {
        polyline(painter, segment_points(segment, vp), stroke, dashed);
    }
}

/// [`draw_trace`] with the dash decided by the trace's own provenance —
/// live solid, snapshot dashed (D15, deliverable 6). Colour/weight say
/// *which channel*; dash says *when it was captured*; two independent
/// facts on two independent, non-colliding visual channels.
pub fn draw_trace_by_provenance(
    painter: &Painter,
    trace: &ac_scene::Trace,
    vp: Viewport,
    stroke: Stroke,
) {
    let dashed = matches!(trace.provenance.source, ac_scene::Source::Snapshot);
    draw_trace(painter, trace, vp, stroke, dashed);
}

/// Draw a pane's horizontal gridlines + left-edge labels from an
/// `ac-scene` axis (#194). Positions and label strings are verbatim from
/// the scene — this crate maps the normalized y to screen and draws,
/// nothing more (no tick math, no formatting: `computes_nothing` holds).
pub fn draw_pane_grid(painter: &Painter, axis: &ac_scene::Axis, vp: Viewport) {
    for tick in &axis.ticks {
        let (_, y) = scene_to_screen((0.0, tick.position), vp);
        painter.line_segment(
            [egui::pos2(vp.x, y), egui::pos2(vp.x + vp.width, y)],
            Stroke::new(0.5, COLOR_STRUCTURAL),
        );
        text(
            painter,
            egui::pos2(vp.x + 2.0, y),
            Align2::LEFT_CENTER,
            &tick.label,
            COLOR_LABEL,
        );
    }
}

/// Shared log-frequency labels along the bottom of a pane (#194) — verbatim
/// from the scene's freq axis.
pub fn draw_freq_labels(painter: &Painter, axis: &ac_scene::Axis, vp: Viewport) {
    for tick in &axis.ticks {
        let (x, _) = scene_to_screen((tick.position, 0.0), vp);
        text(
            painter,
            egui::pos2(x, vp.y + vp.height),
            Align2::CENTER_BOTTOM,
            &tick.label,
            COLOR_LABEL,
        );
    }
}

/// One input-level meter: a thin bar at the right edge of `rect`, height
/// normalized by `ac-scene`. `idx` counts from the right edge.
fn draw_meter(
    painter: &Painter,
    rect: egui::Rect,
    idx: usize,
    meter: &ac_scene::Meter,
    label: &str,
) {
    let bar_w = 6.0;
    let x = rect.max.x - (idx as f32 + 1.0) * (bar_w + 4.0);
    let h = rect.height() * meter.height as f32;
    let color = if meter.clip_latch {
        COLOR_SIGNAL
    } else {
        COLOR_STRUCTURAL
    };
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(x, rect.max.y - h),
            egui::pos2(x + bar_w, rect.max.y),
        ),
        0.0,
        color,
    );
    text(
        painter,
        egui::pos2(x, rect.min.y),
        Align2::LEFT_TOP,
        label,
        COLOR_LABEL,
    );
}

/// Both input-level meters, M left of R (UX standing requirement).
/// Always on (D6) — no toggle. Drawn from the *live* scene only: there
/// is no input-level reading without a live frame, so a stored-run-only
/// comparison shows no meters rather than stale ones.
pub fn draw_input_meters(painter: &Painter, rect: egui::Rect, scene: &ac_scene::TransferScene) {
    draw_meter(painter, rect, 0, &scene.ref_meter, "R");
    draw_meter(painter, rect, 1, &scene.meas_meter, "M");
}
