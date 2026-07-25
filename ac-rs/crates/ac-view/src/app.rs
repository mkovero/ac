//! The eframe shell: input handling, session polling, and drawing via
//! [`crate::view::draw_view`]. This is the only file allowed to touch
//! `eframe`/`egui::Context` directly — everything else in the crate is
//! toolkit-agnostic and unit-testable without a window.

use std::time::Duration;

use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_scene::Scene;

use crate::keys::{bindings_for, Action};
use crate::session::{ConnectionState, Session};
use crate::view::{draw_view, SpectrumViewState, TransferViewState, ViewKind};
use crate::zmq_client::{Client, Endpoint};

pub struct AcViewApp {
    session: Option<Session>,
    endpoint: Endpoint,
    view: ViewKind,
    scene: Option<Scene>,
    /// The last frame received, kept so the scene can be rebuilt on a
    /// zoom/pan (range change) without waiting for the next frame —
    /// otherwise zoom appears frozen on a paused or slow stream.
    last_frame: Option<ac_scene::WireFrame>,
    /// The ranges the current `scene` was last built with, so a
    /// range change alone (no new frame) is detected and triggers a
    /// rebuild from `last_frame`.
    last_scene_ranges: Option<((f64, f64), (f64, f64))>,
    /// Built when the active view is Transfer; the spectrum `scene` stays
    /// `None` then, and vice versa.
    transfer_scene: Option<ac_scene::TransferScene>,
    meters: (ac_scene::MeterState, ac_scene::MeterState),
    help_open: bool,
    weighting: WeightingCurve,
    integration: &'static str,
}

impl AcViewApp {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            session: None,
            endpoint,
            view: ViewKind::Spectrum(SpectrumViewState::default()),
            scene: None,
            last_frame: None,
            last_scene_ranges: None,
            transfer_scene: None,
            meters: (
                ac_scene::MeterState::default(),
                ac_scene::MeterState::default(),
            ),
            help_open: false,
            weighting: WeightingCurve::Z,
            integration: "fast",
        }
    }

    /// Construct in the transfer view rather than the default spectrum
    /// view. `ac transfer` (M4d-CLI, #185) launches through this; the
    /// view is fixed at construction and never switches (§1).
    pub fn new_transfer(endpoint: Endpoint) -> Self {
        let mut app = Self::new(endpoint);
        app.view = ViewKind::Transfer(TransferViewState::default());
        app
    }

    /// The scene currently being drawn, if a frame has been received —
    /// what a paint call would show verbatim (`view::draw_spectrum`
    /// never reformats it). Test-support accessor: lets integration
    /// tests confirm what's on screen without scraping painted shapes
    /// for a value already locked down structurally by the geometry
    /// test and `computes_nothing`'s no-`format!` check.
    pub fn current_scene(&self) -> Option<&Scene> {
        self.scene.as_ref()
    }

    /// The transfer scene currently being drawn, if in the transfer view
    /// and a frame has been received — the same test-support role
    /// `current_scene` plays for spectrum. Lets a test confirm a toggle
    /// reached the built scene (e.g. a derot-mode change moved the phase
    /// segments), closing the hole a state-only assertion (`derot_mode()`
    /// changed) cannot: that the changed mode failed to reach the scene.
    pub fn current_transfer_scene(&self) -> Option<&ac_scene::TransferScene> {
        self.transfer_scene.as_ref()
    }

    /// Feed one wire frame directly, bypassing the ZMQ session — the
    /// hook a headless test uses to drive `current_scene` /
    /// `current_transfer_scene` without a live daemon. Rebuilds the
    /// active view's scene the same way the paint pass does.
    #[cfg(test)]
    pub(crate) fn ingest_frame_for_test(&mut self, frame: ac_scene::WireFrame, now_s: f64) {
        self.last_frame = Some(frame);
        match &self.view {
            ViewKind::Spectrum(state) => {
                let ranges = (
                    (state.freq_range.min(), state.freq_range.max()),
                    (state.db_range.min(), state.db_range.max()),
                );
                self.scene = self
                    .last_frame
                    .as_ref()
                    .map(|f| Scene::from_wire_frame(f, ranges.0, ranges.1));
                self.transfer_scene = None;
            }
            ViewKind::Transfer(state) => {
                let freq_range = (state.freq_range.min(), state.freq_range.max());
                if let Some(f) = &self.last_frame {
                    let input = ac_scene::TransferInput::from_wire_frame(f);
                    self.transfer_scene = Some(ac_scene::TransferScene::from_input(
                        &input,
                        state.derot_mode(),
                        freq_range,
                        (-80.0, 20.0),
                        &mut self.meters,
                        now_s,
                    ));
                }
                self.scene = None;
            }
        }
    }

    /// Apply a keypress action in a test, then rebuild the active scene —
    /// so a test can assert the scene changed, not merely the state.
    #[cfg(test)]
    pub(crate) fn press_for_test(&mut self, action: Action, now_s: f64) {
        self.handle_action(action, false);
        if let Some(frame) = self.last_frame.clone() {
            self.ingest_frame_for_test(frame, now_s);
        }
    }

    fn handle_action(&mut self, action: Action, shift: bool) {
        match action {
            // -- global --
            Action::ToggleHelp => self.help_open = !self.help_open,
            Action::TriggerSnapshot => {
                if let Some(session) = &self.session {
                    // Errors surface as a disconnected/no-op state,
                    // never a crash — snapshot trigger failing (e.g.
                    // no session) is an expected, recoverable UI path.
                    let _ = crate::snapshot_flow::trigger_and_fetch(session.client());
                }
            }
            Action::OpenSnapshot => {
                // File-picker wiring is UX-gated; the orchestration
                // (snapshot_flow::open_local) is implemented and tested.
            }
            Action::Quit => {
                if let Some(session) = &mut self.session {
                    session.stop();
                }
            }
            Action::MoveCursorLeft => {
                if let ViewKind::Spectrum(s) = &mut self.view {
                    s.move_cursor(0.95)
                }
            }
            Action::MoveCursorRight => {
                if let ViewKind::Spectrum(s) = &mut self.view {
                    s.move_cursor(1.05)
                }
            }
            Action::ZoomFreqIn => self.zoom_freq(0.9),
            Action::ZoomFreqOut => self.zoom_freq(1.1),
            Action::PanFreqLeft => self.pan_freq(0.95),
            Action::PanFreqRight => self.pan_freq(1.05),
            Action::ZoomDbIn => {
                if let ViewKind::Spectrum(s) = &mut self.view {
                    s.db_range = s.db_range.zoom(0.9)
                }
            }
            Action::ZoomDbOut => {
                if let ViewKind::Spectrum(s) = &mut self.view {
                    s.db_range = s.db_range.zoom(1.1)
                }
            }
            // -- spectrum view --
            Action::CycleWeighting | Action::CycleIntegration => {
                // Snapshot re-derivation wiring is UX-gated; the
                // rederive_scene orchestration is implemented and tested.
            }
            Action::ToggleRefTrace => {
                if let ViewKind::Spectrum(s) = &mut self.view {
                    s.ref_trace_visible = !s.ref_trace_visible
                }
            }
            // -- transfer view: toggles --
            Action::ToggleRawPhase => {
                if let ViewKind::Transfer(t) = &mut self.view {
                    t.toggle_raw_phase()
                }
            }
            Action::CycleDerotReference => {
                if let ViewKind::Transfer(t) = &mut self.view {
                    t.cycle_derot()
                }
            }
            Action::OpenSettings => {
                // Settings overlay is M4c (#182); M4b binds the key.
            }
            // -- transfer view: stimulus (local state; M4c wires the
            // machine + set_drive) --
            Action::StimulusArmOrStop => {
                if let ViewKind::Transfer(t) = &mut self.view {
                    t.arm_or_stop()
                }
            }
            Action::StimulusFireOrStop => {
                if let ViewKind::Transfer(t) = &mut self.view {
                    t.fire_or_stop()
                }
            }
            Action::StimulusCancel => {
                if let ViewKind::Transfer(t) = &mut self.view {
                    t.cancel()
                }
            }
            Action::StimulusLevelUp => {
                if let ViewKind::Transfer(t) = &mut self.view {
                    t.nudge_level(true, shift)
                }
            }
            Action::StimulusLevelDown => {
                if let ViewKind::Transfer(t) = &mut self.view {
                    t.nudge_level(false, shift)
                }
            }
        }
    }

    fn zoom_freq(&mut self, factor: f64) {
        match &mut self.view {
            ViewKind::Spectrum(s) => s.freq_range = s.freq_range.zoom(factor),
            ViewKind::Transfer(t) => t.freq_range = t.freq_range.zoom(factor),
        }
    }

    fn pan_freq(&mut self, factor: f64) {
        match &mut self.view {
            ViewKind::Spectrum(s) => s.freq_range = s.freq_range.pan(factor),
            ViewKind::Transfer(t) => t.freq_range = t.freq_range.pan(factor),
        }
    }
}

impl eframe::App for AcViewApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let view_id = self.view.id();
        let mut pressed: Vec<(Action, bool)> = Vec::new();
        ctx.input(|i| {
            for binding in bindings_for(view_id) {
                if i.key_pressed(binding.key) {
                    pressed.push((binding.action, i.modifiers.shift));
                }
            }
        });
        for (action, shift) in pressed {
            self.handle_action(action, shift);
        }

        let mut got_new_frame = false;
        if let Some(session) = &mut self.session {
            // Drain to the newest queued frame rather than parsing one
            // per repaint: the daemon publishes faster than the UI
            // repaints, so a single `if let` would fall progressively
            // behind. `self.last_frame` is overwritten each iteration,
            // so the backlog is discarded and only the freshest frame
            // survives — correct for a live display.
            while let Some(frame) = session.poll_frame(Duration::from_millis(0)) {
                if let Ok(wire_frame) = serde_json::from_value::<ac_scene::WireFrame>(frame) {
                    self.last_frame = Some(wire_frame);
                    got_new_frame = true;
                }
            }
        }

        // Rebuild the scene once per pass — never once per backlog frame
        // — either because a new frame arrived or because zoom/pan
        // changed the ranges. Range-only changes must also rebuild, or
        // zoom/pan appears frozen on a paused/snapshot scene until the
        // next frame happens to arrive. Only the active view's scene is
        // built; the other stays `None`.
        match &self.view {
            ViewKind::Spectrum(state) => {
                let ranges = (
                    (state.freq_range.min(), state.freq_range.max()),
                    (state.db_range.min(), state.db_range.max()),
                );
                if let Some(wire_frame) = &self.last_frame {
                    if got_new_frame || self.last_scene_ranges != Some(ranges) {
                        self.scene = Some(Scene::from_wire_frame(wire_frame, ranges.0, ranges.1));
                        self.last_scene_ranges = Some(ranges);
                    }
                }
                self.transfer_scene = None;
            }
            ViewKind::Transfer(state) => {
                // dB range for the magnitude pane is fixed for now; the
                // phase pane is a fixed ±180° band inside ac-scene.
                let db_range = (-80.0, 20.0);
                let freq_range = (state.freq_range.min(), state.freq_range.max());
                let now_s = ctx.input(|i| i.time);
                if let Some(wire_frame) = &self.last_frame {
                    let input = ac_scene::TransferInput::from_wire_frame(wire_frame);
                    self.transfer_scene = Some(ac_scene::TransferScene::from_input(
                        &input,
                        state.derot_mode(),
                        freq_range,
                        db_range,
                        &mut self.meters,
                        now_s,
                    ));
                }
                self.scene = None;
            }
        }

        let status = match &self.session {
            None => "no session".to_string(),
            Some(s) => match s.connection_state() {
                ConnectionState::NoSession => "no session".to_string(),
                ConnectionState::Live => {
                    format!("live — {}:{}", self.endpoint.host, self.endpoint.ctrl_port)
                }
                ConnectionState::Disconnected => {
                    format!(
                        "disconnected — {}:{} not responding",
                        self.endpoint.host, self.endpoint.ctrl_port
                    )
                }
            },
        };
        ui.label(status);
        draw_view(
            &self.view,
            ui,
            self.scene.as_ref(),
            self.transfer_scene.as_ref(),
        );

        if self.help_open {
            let help = crate::keys::help_text(self.view.id());
            egui::Window::new("help").show(&ctx, |ui| {
                ui.label(help);
            });
        }

        // Continuous repaint (paced to vsync by egui/eframe) while a
        // session is live, so the display updates every frame without
        // needing mouse-move input events to force a repaint — the
        // sluggish-at-rest bug this replaces. Lazy repaint when idle so
        // a static "no session" screen doesn't burn a CPU core.
        if self.session.is_some() {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

/// Construct an `AcViewApp` already connected to `endpoint` and with a
/// `transfer_stream` session launched — the path `main.rs` uses; kept
/// separate from `AcViewApp::new` so tests can construct an
/// unconnected app (geometry/keys/range tests never need a socket).
pub fn connect_and_launch(
    endpoint: Endpoint,
    meas_channel: u32,
    ref_channel: u32,
    weighting: WeightingCurve,
    integration: &'static str,
) -> anyhow::Result<AcViewApp> {
    let client = Client::connect(&endpoint)?;
    let mut session = Session::new(client);
    session.launch(meas_channel, ref_channel, weighting, integration)?;
    let mut app = AcViewApp::new(endpoint);
    app.session = Some(session);
    app.weighting = weighting;
    app.integration = integration;
    Ok(app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Action;

    fn transfer_frame() -> ac_scene::WireFrame {
        // A mis-estimated delay so the wire carries non-zero phase and
        // Session (τ_derot 0) differs from the other modes — otherwise
        // cycling would be a no-op and the test could not fail on the bug
        // it names.
        let json = serde_json::json!({
            "type": "transfer_stream",
            "sr": 48000,
            "meas_channel": 0,
            "ref_channel": 1,
            "spec_freqs": [100.0, 1000.0, 10000.0],
            "meas_spectrum": [0.1, 0.1, 0.1],
            "ref_spectrum": [0.1, 0.1, 0.1],
            "spl": null,
            "spl_weighting": "Z",
            "spl_integration": "fast",
            "freqs": [100.0, 1000.0, 10000.0],
            "magnitude_db": [-6.0, -6.0, -6.0],
            "phase_deg": [-18.0, -180.0, 60.0],
            "coherence": [0.9, 0.9, 0.9],
            "delay_samples": 96,
            "delay_ms": 2.0,
            "meas_peak_dbfs": -6.0,
            "ref_peak_dbfs": -12.0
        });
        serde_json::from_value(json).expect("wire frame")
    }

    // Scene-accessor AC (no shape scraping): a derot keypress must change
    // the BUILT transfer scene's phase segments — not merely the
    // `derot_mode()` state field. Closes the hole a state-only assertion
    // cannot: a mode change that fails to reach the scene.
    #[test]
    fn cycling_derot_changes_the_built_transfer_scene_phase() {
        let mut app = AcViewApp::new_transfer(Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        });
        app.ingest_frame_for_test(transfer_frame(), 0.0);

        let before: Vec<Vec<(f64, f64)>> = app
            .current_transfer_scene()
            .expect("scene built")
            .phase
            .segments
            .clone();

        app.press_for_test(Action::CycleDerotReference, 0.1);

        let after = &app
            .current_transfer_scene()
            .expect("scene rebuilt")
            .phase
            .segments;
        assert_ne!(
            &before, after,
            "cycling de-rotation reference did not change the built phase pane"
        );
    }

    // The magnitude pane must NOT move when only the de-rotation
    // reference changes — de-rotation is a phase-only operation.
    #[test]
    fn cycling_derot_leaves_the_magnitude_pane_unchanged() {
        let mut app = AcViewApp::new_transfer(Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        });
        app.ingest_frame_for_test(transfer_frame(), 0.0);
        let before = app
            .current_transfer_scene()
            .unwrap()
            .magnitude
            .segments
            .clone();
        app.press_for_test(Action::CycleDerotReference, 0.1);
        let after = &app.current_transfer_scene().unwrap().magnitude.segments;
        assert_eq!(&before, after, "de-rotation moved the magnitude pane");
    }
}
