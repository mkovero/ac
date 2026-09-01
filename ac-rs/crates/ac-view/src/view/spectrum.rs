//! The spectrum view: one pane, meas over ref, axis labels, SPL and
//! cursor readouts. Every point and every string is `ac-scene`'s; this
//! file maps and paints.

use ac_scene::Scene;
use egui::{Align2, Stroke, Ui};

use crate::geometry::{scene_to_screen, Viewport};

use super::paint::{draw_trace_by_provenance, text};
use super::palette::{COLOR_LABEL, COLOR_SIGNAL, COLOR_STRUCTURAL, COLOR_VALUE};
use super::state::SpectrumViewState;

pub(super) fn draw_spectrum(state: &SpectrumViewState, ui: &mut Ui, scene: Option<&Scene>) {
    let rect = ui.available_rect_before_wrap();
    let viewport = Viewport::from(rect);
    let painter = ui.painter();

    let Some(scene) = scene else {
        text(
            painter,
            rect.center(),
            Align2::CENTER_CENTER,
            "no session — press S to snapshot, F to open a file",
            COLOR_STRUCTURAL,
        );
        return;
    };

    // Traces: polylines only, points already normalized by ac-scene —
    // this crate's only numeric act is the affine map (geometry.rs).
    // Colour/weight distinguish meas (the calibrated signal — ember,
    // full weight) from ref (recedes — structural grey, thinner);
    // stroke style distinguishes live (solid) from snapshot (dashed)
    // provenance (D15, deliverable 6) — two independent facts on two
    // independent non-colliding visual channels.
    // One draw per segment (see `paint::draw_trace`): a trace with a
    // coherence gap is several polylines, and the gap is the absence of
    // a segment — this crate never decides where a gap goes, it only
    // stops drawing where ac-scene stopped emitting.
    for trace in &scene.traces {
        let is_meas = trace.provenance.channel_role.starts_with("meas");
        // Ref-trace visibility (`V`): skip the reference trace when the
        // toggle is off. The meas trace is always drawn — the toggle
        // exists to clear the ref out of the way when comparing against a
        // snapshot, not to blank the display.
        if !is_meas && !state.ref_trace_visible {
            continue;
        }
        let stroke = if is_meas {
            Stroke::new(1.5, COLOR_SIGNAL)
        } else {
            Stroke::new(1.0, COLOR_STRUCTURAL)
        };
        draw_trace_by_provenance(painter, trace, viewport, stroke);
    }

    // Axis ticks: positions and labels delivered verbatim by ac-scene.
    for tick in &scene.freq_axis.ticks {
        let (x, _) = scene_to_screen((tick.position, 0.0), viewport);
        text(
            painter,
            egui::pos2(x, rect.max.y),
            Align2::CENTER_TOP,
            &tick.label,
            COLOR_LABEL,
        );
    }
    for tick in &scene.db_axis.ticks {
        let (_, y) = scene_to_screen((0.0, tick.position), viewport);
        text(
            painter,
            egui::pos2(rect.min.x, y),
            Align2::LEFT_CENTER,
            &tick.label,
            COLOR_LABEL,
        );
    }

    // SPL readout: verbatim string from ac-scene, no reformatting.
    if let Some(spl) = &scene.readouts.spl {
        text(
            painter,
            rect.right_top(),
            Align2::RIGHT_TOP,
            spl,
            COLOR_VALUE,
        );
    }

    // Cursor readout: verbatim string from ac-scene's own formatting —
    // this crate only supplies the target Hz, ac-scene does the
    // nearest-column lookup, the dB conversion, and the formatting.
    if let Some(freq_hz) = state.cursor_freq_hz {
        if let Some(readout) = scene.cursor_readout(freq_hz) {
            text(
                painter,
                rect.left_top(),
                Align2::LEFT_TOP,
                readout,
                COLOR_VALUE,
            );
        }
    }
}
