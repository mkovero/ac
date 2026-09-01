//! The two impulse-response panels: the live one (`H`, #286) and the
//! sweep-derived, gated one read back off disk (#308). They share a
//! header line and the h(t)/axis/arrival drawing; they differ only in
//! how a missing scene is explained.

use egui::{Align2, Painter, Rect, Stroke};

use crate::geometry::{scene_to_screen, Viewport};

use super::paint::{draw_trace_by_provenance, text};
use super::palette::{COLOR_LABEL, COLOR_SIGNAL, COLOR_STRUCTURAL, COLOR_VALUE};

/// The IR panel (#286): header, h(t) trace, time axis, and the arrival
/// marker — every string and coordinate `ac-scene`'s, this crate only
/// maps the normalized `[0,1]²` points into `rect` and draws (the same
/// contract every other pane in this module tree holds).
pub(super) fn draw_ir_panel(painter: &Painter, rect: Rect, scene: &ac_scene::IrScene) {
    draw_ir_header(painter, rect, scene.header);

    if scene.trace.segments.is_empty() {
        text(
            painter,
            rect.center(),
            Align2::CENTER_CENTER,
            "no samples yet",
            COLOR_STRUCTURAL,
        );
        return;
    }

    draw_ir_trace_and_arrival(
        painter,
        rect,
        &scene.trace,
        &scene.time_axis,
        &scene.arrival,
    );
}

/// Frame C (#308): the sweep-derived, gated IR panel — a
/// `MeasurementReport` read back off disk by `report_flow::open_sweep_ir`
/// rather than fed live from a wire frame, so this takes the already-
/// decided `Result` rather than an `Option` (#286's live panel has
/// "no frame yet"; this one has two distinct, named reasons a file
/// didn't produce a scene, per [`ac_scene::SweepIrFault`]).
///
/// Not yet reachable from [`super::draw_view`]'s dispatch — the file-open
/// UI that would call it is #256's picker, sequenced after this issue (the
/// architect review's explicit call). This function is the orchestration
/// side of that: implemented and tested now, wired once the picker
/// exists — same pattern `Action::OpenSnapshot` already documents for
/// `.acsnap`.
pub fn draw_sweep_ir_panel(
    painter: &Painter,
    rect: Rect,
    result: Result<&ac_scene::SweepIrScene, ac_scene::SweepIrFault>,
) {
    match result {
        Ok(scene) => {
            draw_ir_header(painter, rect, &scene.header);
            if scene.trace.segments.is_empty() {
                text(
                    painter,
                    rect.center(),
                    Align2::CENTER_CENTER,
                    "no samples in gate window",
                    COLOR_STRUCTURAL,
                );
                return;
            }
            draw_ir_trace_and_arrival(
                painter,
                rect,
                &scene.trace,
                &scene.time_axis,
                &scene.arrival,
            );
        }
        Err(fault) => {
            // Same panel-geometry slot the success frame's header
            // occupies (UX comment: "nothing jumps when a bad file
            // replaces a good one"), plus the fault's own detail text —
            // names what to check, never a cause (`SweepIrFault::detail`'s
            // doc).
            draw_ir_header(painter, rect, &fault.header());
            text(
                painter,
                rect.center(),
                Align2::CENTER_CENTER,
                fault.detail(),
                COLOR_STRUCTURAL,
            );
        }
    }
}

/// The header line both IR panel variants draw at `rect`'s top-left —
/// verbatim `ac-scene` string, this crate only positions it.
fn draw_ir_header(painter: &Painter, rect: Rect, header: &str) {
    text(
        painter,
        rect.left_top(),
        Align2::LEFT_TOP,
        header,
        COLOR_LABEL,
    );
}

/// The part both IR panel variants share once there's a non-empty trace
/// to draw: h(t), the time axis, and the arrival marker — every string
/// and coordinate `ac-scene`'s, this crate only maps the normalized
/// `[0,1]²` points into `rect`.
fn draw_ir_trace_and_arrival(
    painter: &Painter,
    rect: Rect,
    trace: &ac_scene::Trace,
    time_axis: &ac_scene::Axis,
    arrival: &ac_scene::ArrivalMarker,
) {
    let vp = Viewport::from(rect);

    // h(t): one polyline per segment, same drawing rule the mag/phase
    // traces use — there is always exactly one segment here (no coherence
    // mask over a time-domain sample), but the shared helper stays
    // generic rather than assuming that shape. Solid live, dashed for a
    // snapshot, the same provenance convention the spectrum view draws.
    draw_trace_by_provenance(painter, trace, vp, Stroke::new(1.5, COLOR_SIGNAL));

    // Time axis: bottom labels, verbatim ac-scene ticks — same mapping
    // `paint::draw_freq_labels` uses for the mag/phase panes' shared
    // frequency axis.
    for tick in &time_axis.ticks {
        let (x, _) = scene_to_screen((tick.position, 0.0), vp);
        text(
            painter,
            egui::pos2(x, rect.max.y),
            Align2::CENTER_BOTTOM,
            &tick.label,
            COLOR_LABEL,
        );
    }

    // Arrival marker: a vertical line at the frame's own delay position,
    // plus the verbatim readout string beneath it. Not clamped to the
    // pane — `ac-scene` leaves an out-of-range position as-is (see
    // `IrScene`'s doc), and egui simply draws it off-canvas, which is
    // honest rather than a fabricated in-range position.
    let (ax, top) = scene_to_screen((arrival.position, 1.0), vp);
    let (_, bottom) = scene_to_screen((arrival.position, 0.0), vp);
    painter.line_segment(
        [egui::pos2(ax, top), egui::pos2(ax, bottom)],
        Stroke::new(1.0, COLOR_VALUE),
    );
    text(
        painter,
        egui::pos2(ax, bottom + 2.0),
        Align2::CENTER_TOP,
        &arrival.text,
        COLOR_VALUE,
    );
}
