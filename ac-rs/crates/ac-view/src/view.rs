//! View dispatch (architect review, decision 4): a `ViewKind` enum with
//! a single variant today, drawn through one dispatch function, so a
//! future waterfall/H-view (M4+) is a new match arm — not a shell
//! restructure. Session management and keyboard routing stay
//! view-agnostic; they call [`draw_view`], never a spectrum-specific
//! drawing function directly.

use ac_scene::{Scene, Source};
use egui::{Color32, Stroke, Ui};

use crate::geometry::{scene_to_screen, Viewport};
use crate::range::{DbRange, FreqRange};

/// The signal colour (UX review: "the ember" — the one thing on screen
/// that should glow). Never green/blue (this project's own palette
/// rule: they recede in dark environments and carry status/success
/// baggage that conflicts with a neutral signal indicator).
const COLOR_SIGNAL: Color32 = Color32::from_rgb(0xd7, 0x87, 0x5f);
/// Reference channel: recedes via weight, not a second competing hue.
const COLOR_STRUCTURAL: Color32 = Color32::from_rgb(0x62, 0x62, 0x62);
/// Axis tick labels: mid grey, one step brighter than
/// [`COLOR_STRUCTURAL`]'s "inactive/context" register.
const COLOR_LABEL: Color32 = Color32::from_rgb(0x9e, 0x9e, 0x9e);
/// Readout text: near-white, not pure white — pure white reads harsher
/// than the palette calls for and competes with the ember trace.
const COLOR_VALUE: Color32 = Color32::from_rgb(0xe4, 0xe4, 0xe4);

#[derive(Default)]
pub struct SpectrumViewState {
    pub freq_range: FreqRange,
    pub db_range: DbRange,
    /// The cursor's current target frequency, if active. Plain Hz, not
    /// a column index — `ac-scene`'s `Scene::cursor_readout` already
    /// does nearest-column snapping internally (it holds the column
    /// list, which this crate deliberately never sees), so moving the
    /// cursor just needs to nudge this value; which column it lands on
    /// is `ac-scene`'s computation, not this crate's.
    pub cursor_freq_hz: Option<f64>,
}

impl SpectrumViewState {
    /// Move the cursor by a log-space step (matching the frequency
    /// axis's own log mapping) — `factor > 1.0` moves right/up in
    /// frequency, `factor < 1.0` moves left/down. Activates the cursor
    /// at the range's centre if it wasn't active yet.
    pub fn move_cursor(&mut self, factor: f64) {
        let cur = self
            .cursor_freq_hz
            .unwrap_or_else(|| (self.freq_range.min() * self.freq_range.max()).sqrt());
        let moved = (cur * factor).clamp(self.freq_range.min(), self.freq_range.max());
        self.cursor_freq_hz = Some(moved);
    }
}

pub enum ViewKind {
    Spectrum(SpectrumViewState),
}

/// One dispatch function every future view (M4+) extends by adding a
/// match arm — never by the shell inlining a new drawing call.
pub fn draw_view(kind: &ViewKind, ui: &mut Ui, scene: Option<&Scene>) {
    match kind {
        ViewKind::Spectrum(state) => draw_spectrum(state, ui, scene),
    }
}

fn draw_spectrum(state: &SpectrumViewState, ui: &mut Ui, scene: Option<&Scene>) {
    let rect = ui.available_rect_before_wrap();
    let viewport = Viewport {
        x: rect.min.x,
        y: rect.min.y,
        width: rect.width(),
        height: rect.height(),
    };
    let painter = ui.painter();

    let Some(scene) = scene else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "no session — press S to snapshot, F to open a file",
            egui::FontId::default(),
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
    for trace in &scene.traces {
        let points: Vec<egui::Pos2> = trace
            .points
            .iter()
            .map(|&pt| {
                let (x, y) = scene_to_screen(pt, viewport);
                egui::pos2(x, y)
            })
            .collect();
        let is_meas = trace.provenance.channel_role.starts_with("meas");
        let stroke = if is_meas {
            Stroke::new(1.5, COLOR_SIGNAL)
        } else {
            Stroke::new(1.0, COLOR_STRUCTURAL)
        };
        match trace.provenance.source {
            Source::Live => {
                painter.add(egui::Shape::line(points, stroke));
            }
            Source::Snapshot => {
                let mut shapes = Vec::new();
                egui::Shape::dashed_line_many(&points, stroke, 6.0, 4.0, &mut shapes);
                painter.extend(shapes);
            }
        }
    }

    // Axis ticks: positions and labels delivered verbatim by ac-scene.
    for tick in &scene.freq_axis.ticks {
        let (x, _) = scene_to_screen((tick.position, 0.0), viewport);
        painter.text(
            egui::pos2(x, rect.max.y),
            egui::Align2::CENTER_TOP,
            &tick.label,
            egui::FontId::default(),
            COLOR_LABEL,
        );
    }
    for tick in &scene.db_axis.ticks {
        let (_, y) = scene_to_screen((0.0, tick.position), viewport);
        painter.text(
            egui::pos2(rect.min.x, y),
            egui::Align2::LEFT_CENTER,
            &tick.label,
            egui::FontId::default(),
            COLOR_LABEL,
        );
    }

    // SPL readout: verbatim string from ac-scene, no reformatting.
    if let Some(spl) = &scene.readouts.spl {
        painter.text(
            rect.right_top(),
            egui::Align2::RIGHT_TOP,
            spl,
            egui::FontId::default(),
            COLOR_VALUE,
        );
    }

    // Cursor readout: verbatim string from ac-scene's own formatting —
    // this crate only supplies the target Hz, ac-scene does the
    // nearest-column lookup, the dB conversion, and the formatting.
    if let Some(freq_hz) = state.cursor_freq_hz {
        if let Some(readout) = scene.cursor_readout(freq_hz) {
            painter.text(
                rect.left_top(),
                egui::Align2::LEFT_TOP,
                readout,
                egui::FontId::default(),
                COLOR_VALUE,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_spectrum_state_has_no_cursor() {
        let state = SpectrumViewState::default();
        assert_eq!(state.cursor_freq_hz, None);
    }

    #[test]
    fn move_cursor_activates_and_stays_within_range() {
        let mut state = SpectrumViewState::default();
        state.move_cursor(1.1);
        assert!(state.cursor_freq_hz.is_some());
        let f = state.cursor_freq_hz.unwrap();
        assert!(f >= state.freq_range.min() && f <= state.freq_range.max());
    }

    #[test]
    fn move_cursor_clamps_at_range_edges() {
        let mut state = SpectrumViewState::default();
        for _ in 0..500 {
            state.move_cursor(10.0);
        }
        assert!(state.cursor_freq_hz.unwrap() <= state.freq_range.max());
    }
}
