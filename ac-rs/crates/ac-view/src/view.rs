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

pub struct SpectrumViewState {
    pub freq_range: FreqRange,
    pub db_range: DbRange,
    /// Reference-trace visibility (the one new spectrum toggle, `V`).
    /// Default on — the ref trace is a normal part of the display; the
    /// toggle exists to hide it when comparing against a snapshot.
    pub ref_trace_visible: bool,
    /// The cursor's current target frequency, if active. Plain Hz, not
    /// a column index — `ac-scene`'s `Scene::cursor_readout` already
    /// does nearest-column snapping internally (it holds the column
    /// list, which this crate deliberately never sees), so moving the
    /// cursor just needs to nudge this value; which column it lands on
    /// is `ac-scene`'s computation, not this crate's.
    pub cursor_freq_hz: Option<f64>,
}

impl Default for SpectrumViewState {
    fn default() -> Self {
        Self {
            freq_range: FreqRange::default(),
            db_range: DbRange::default(),
            ref_trace_visible: true,
            cursor_freq_hz: None,
        }
    }
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
    Transfer(TransferViewState),
}

impl ViewKind {
    pub fn id(&self) -> crate::keys::ViewId {
        match self {
            ViewKind::Spectrum(_) => crate::keys::ViewId::Spectrum,
            ViewKind::Transfer(_) => crate::keys::ViewId::Transfer,
        }
    }
}

/// One dispatch function every future view (M4+) extends by adding a
/// match arm — never by the shell inlining a new drawing call. The two
/// scene options are mutually exclusive in practice: the app builds only
/// the one matching the active view (the other stays `None`).
pub fn draw_view(
    kind: &ViewKind,
    ui: &mut Ui,
    scene: Option<&Scene>,
    transfer: Option<&ac_scene::TransferScene>,
) {
    match kind {
        ViewKind::Spectrum(state) => draw_spectrum(state, ui, scene),
        ViewKind::Transfer(state) => draw_transfer(state, ui, transfer),
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
    // One draw per segment: a trace with a coherence gap is several
    // polylines, and the gap is the absence of a segment — this crate
    // never decides where a gap goes, it only stops drawing where
    // ac-scene stopped emitting.
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
        for segment in &trace.segments {
            let points: Vec<egui::Pos2> = segment
                .iter()
                .map(|&pt| {
                    let (x, y) = scene_to_screen(pt, viewport);
                    egui::pos2(x, y)
                })
                .collect();
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

/// Which de-rotation reference the phase pane uses (D3). `R` cycles
/// through these; `P` (raw-phase toggle) forces `Raw` and back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DerotChoice {
    Session,
    Snapshot,
    Raw,
}

impl DerotChoice {
    fn next(self) -> DerotChoice {
        match self {
            DerotChoice::Session => DerotChoice::Snapshot,
            DerotChoice::Snapshot => DerotChoice::Raw,
            DerotChoice::Raw => DerotChoice::Session,
        }
    }

    /// Map to the `ac-scene` mode. `snapshot_delay_ms` is the open
    /// snapshot's τ (0.0 when none is open — Snapshot then behaves like
    /// Session, which is the sensible fallback until M4c wires a real
    /// open-snapshot delay in).
    pub fn to_mode(self, snapshot_delay_ms: f64) -> ac_scene::DerotMode {
        match self {
            DerotChoice::Session => ac_scene::DerotMode::Session,
            DerotChoice::Raw => ac_scene::DerotMode::Raw,
            DerotChoice::Snapshot => ac_scene::DerotMode::Snapshot { snapshot_delay_ms },
        }
    }
}

/// Client-visible stimulus state. M4b holds the state and the level so
/// the keys are not dead and the banner can render; the full machine
/// (dead-man resend, 5 s auto-disarm, `set_drive` wire calls, entry-point
/// clamp) is M4c (#182). Level steps and the arm/fire/stop transitions
/// live here because the banner reads them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StimulusState {
    Idle,
    Armed,
    Driving,
}

pub struct TransferViewState {
    pub freq_range: FreqRange,
    pub derot: DerotChoice,
    /// `P` toggles raw phase; remembers the previous choice so a second
    /// press restores it rather than landing on Session unconditionally.
    prev_derot: DerotChoice,
    pub stimulus: StimulusState,
    pub level_dbfs: f64,
    /// The open snapshot's stored delay (ms), fed to `DerotChoice::Snapshot`.
    /// M4c wires the open-snapshot flow; until then it stays 0.
    pub snapshot_delay_ms: f64,
}

impl Default for TransferViewState {
    fn default() -> Self {
        Self {
            freq_range: FreqRange::default(),
            derot: DerotChoice::Session,
            prev_derot: DerotChoice::Session,
            stimulus: StimulusState::Idle,
            level_dbfs: -30.0,
            snapshot_delay_ms: 0.0,
        }
    }
}

impl TransferViewState {
    pub fn cycle_derot(&mut self) {
        self.derot = self.derot.next();
    }

    /// `P`: force raw phase, or restore the previous non-raw choice.
    pub fn toggle_raw_phase(&mut self) {
        if self.derot == DerotChoice::Raw {
            self.derot = self.prev_derot;
        } else {
            self.prev_derot = self.derot;
            self.derot = DerotChoice::Raw;
        }
    }

    /// `↑`/`↓`. `big` is the Shift-modified 3 dB step (D7); the
    /// authoritative clamp to `drive_max_dbfs` is the server's (M4d) and
    /// the client entry-point clamp is M4c — M4b only moves the number.
    pub fn nudge_level(&mut self, up: bool, big: bool) {
        let step = if big { 3.0 } else { 1.0 };
        self.level_dbfs += if up { step } else { -step };
    }

    /// Space: Idle→Armed, else stop.
    pub fn arm_or_stop(&mut self) {
        self.stimulus = match self.stimulus {
            StimulusState::Idle => StimulusState::Armed,
            _ => StimulusState::Idle,
        };
    }

    /// Enter: Armed→Driving, else stop.
    pub fn fire_or_stop(&mut self) {
        self.stimulus = match self.stimulus {
            StimulusState::Armed => StimulusState::Driving,
            _ => StimulusState::Idle,
        };
    }

    /// Esc: stop from any state.
    pub fn cancel(&mut self) {
        self.stimulus = StimulusState::Idle;
    }

    pub fn derot_mode(&self) -> ac_scene::DerotMode {
        self.derot.to_mode(self.snapshot_delay_ms)
    }
}

/// Transfer view (M4b): magnitude pane stacked over phase pane, shared
/// log-f axis, gap rendering for masked columns, delay readout, input
/// meters, and the stimulus banner — every string and coordinate from
/// `ac-scene`, drawn verbatim.
fn draw_transfer(state: &TransferViewState, ui: &mut Ui, scene: Option<&ac_scene::TransferScene>) {
    let rect = ui.available_rect_before_wrap();
    let painter = ui.painter();

    let Some(scene) = scene else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "no session — transfer view",
            egui::FontId::default(),
            COLOR_STRUCTURAL,
        );
        return;
    };

    // The stimulus banner (safety UI) owns a reserved top band that
    // nothing else draws into — UX finding: it must be top-center and
    // must not collide with the delay readout or the meter labels. Build
    // it first so its height carves the band out before anything else is
    // placed. DRIVING is louder than ARMED and uses the signal colour
    // (never green); strings are verbatim ac-scene (F5). Channel/port
    // come from config in M4c (#182) — placeholder channel for now, but
    // the STRING is ac-scene's, which is what F5 checks.
    let banner = match state.stimulus {
        StimulusState::Idle => None,
        StimulusState::Armed => Some((
            ac_scene::readout::format_armed_banner(0, None, state.level_dbfs),
            18.0,
            COLOR_VALUE,
        )),
        StimulusState::Driving => Some((
            ac_scene::readout::format_driving_banner(0, None, state.level_dbfs),
            24.0,
            COLOR_SIGNAL,
        )),
    };
    let banner_band = banner
        .as_ref()
        .map(|(_, size, _)| size + 8.0)
        .unwrap_or(0.0);
    if let Some((text, size, color)) = &banner {
        painter.text(
            egui::pos2(rect.center().x, rect.min.y + 4.0),
            egui::Align2::CENTER_TOP,
            text,
            egui::FontId::proportional(*size),
            *color,
        );
    }

    // Everything else lives below the banner band.
    let content =
        egui::Rect::from_min_max(egui::pos2(rect.min.x, rect.min.y + banner_band), rect.max);

    // Two stacked panes: magnitude (top), phase (bottom). Shared x
    // (log-f); each pane maps its own normalized y over its own band.
    let gap = 8.0;
    let pane_h = (content.height() - gap) / 2.0;
    let mag_vp = Viewport {
        x: content.min.x,
        y: content.min.y,
        width: content.width(),
        height: pane_h,
    };
    let phase_vp = Viewport {
        x: content.min.x,
        y: content.min.y + pane_h + gap,
        width: content.width(),
        height: pane_h,
    };

    draw_segments(
        painter,
        &scene.magnitude,
        mag_vp,
        Stroke::new(1.5, COLOR_SIGNAL),
    );
    draw_segments(
        painter,
        &scene.phase,
        phase_vp,
        Stroke::new(1.5, COLOR_SIGNAL),
    );

    // Delay readout: verbatim ac-scene string, top-left of the content
    // band (below the banner).
    painter.text(
        content.left_top(),
        egui::Align2::LEFT_TOP,
        &scene.delay_readout,
        egui::FontId::default(),
        COLOR_VALUE,
    );

    // Input-level meters: two thin bars at the right edge, M left of R
    // (UX standing requirement), heights normalized by ac-scene. Always
    // on (D6) — no toggle. `idx` counts from the right edge, so R (idx 0)
    // is rightmost and M (idx 1) sits to its left.
    draw_meter(painter, content, 0, &scene.ref_meter, "R");
    draw_meter(painter, content, 1, &scene.meas_meter, "M");
}

/// Draw one trace's segments in a pane — one polyline per segment, so a
/// coherence gap is the absence of a segment (D5), never a line to the
/// floor. This crate decides nothing about where a gap goes; it stops
/// drawing where ac-scene stopped emitting.
fn draw_segments(painter: &egui::Painter, trace: &ac_scene::Trace, vp: Viewport, stroke: Stroke) {
    for segment in &trace.segments {
        let pts: Vec<egui::Pos2> = segment
            .iter()
            .map(|&pt| {
                let (x, y) = scene_to_screen(pt, vp);
                egui::pos2(x, y)
            })
            .collect();
        if pts.len() > 1 {
            painter.add(egui::Shape::line(pts, stroke));
        }
    }
}

fn draw_meter(
    painter: &egui::Painter,
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
    painter.text(
        egui::pos2(x, rect.min.y),
        egui::Align2::LEFT_TOP,
        label,
        egui::FontId::default(),
        COLOR_LABEL,
    );
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

    // --- transfer view state (M4b) ---

    // Each toggle changes the value that feeds the scene — the
    // scene-accessor intent at the state level (a different derot_mode is
    // a different phase pane; ac-scene's F1′/F2′ prove the numeric
    // consequence).
    #[test]
    fn cycle_derot_visits_all_three_references_and_wraps() {
        let mut t = TransferViewState::default();
        assert_eq!(t.derot, DerotChoice::Session);
        t.cycle_derot();
        assert_eq!(t.derot, DerotChoice::Snapshot);
        t.cycle_derot();
        assert_eq!(t.derot, DerotChoice::Raw);
        t.cycle_derot();
        assert_eq!(t.derot, DerotChoice::Session);
    }

    #[test]
    fn raw_phase_toggle_forces_raw_then_restores_the_previous_choice() {
        let mut t = TransferViewState::default();
        t.cycle_derot(); // Snapshot
        t.toggle_raw_phase();
        assert_eq!(t.derot, DerotChoice::Raw);
        // Second press restores Snapshot, not Session.
        t.toggle_raw_phase();
        assert_eq!(t.derot, DerotChoice::Snapshot);
    }

    #[test]
    fn derot_choice_maps_to_the_ac_scene_mode() {
        assert_eq!(
            DerotChoice::Session.to_mode(3.0),
            ac_scene::DerotMode::Session
        );
        assert_eq!(DerotChoice::Raw.to_mode(3.0), ac_scene::DerotMode::Raw);
        assert_eq!(
            DerotChoice::Snapshot.to_mode(3.0),
            ac_scene::DerotMode::Snapshot {
                snapshot_delay_ms: 3.0
            }
        );
    }

    #[test]
    fn stimulus_transitions_follow_arm_fire_stop() {
        let mut t = TransferViewState::default();
        assert_eq!(t.stimulus, StimulusState::Idle);
        t.arm_or_stop();
        assert_eq!(t.stimulus, StimulusState::Armed);
        t.fire_or_stop();
        assert_eq!(t.stimulus, StimulusState::Driving);
        // Stop works from Driving (Space).
        t.arm_or_stop();
        assert_eq!(t.stimulus, StimulusState::Idle);
        // And Esc stops from Armed.
        t.arm_or_stop();
        t.cancel();
        assert_eq!(t.stimulus, StimulusState::Idle);
    }

    #[test]
    fn level_nudges_use_1db_and_3db_steps() {
        let mut t = TransferViewState::default();
        let base = t.level_dbfs;
        t.nudge_level(true, false);
        assert!((t.level_dbfs - (base + 1.0)).abs() < 1e-9);
        t.nudge_level(true, true);
        assert!((t.level_dbfs - (base + 4.0)).abs() < 1e-9);
        t.nudge_level(false, false);
        assert!((t.level_dbfs - (base + 3.0)).abs() < 1e-9);
    }
}
