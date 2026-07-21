//! The eframe shell: input handling, session polling, and drawing via
//! [`crate::view::draw_view`]. This is the only file allowed to touch
//! `eframe`/`egui::Context` directly — everything else in the crate is
//! toolkit-agnostic and unit-testable without a window.

use std::time::Duration;

use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_scene::Scene;

use crate::keys::{Action, BINDINGS};
use crate::session::{ConnectionState, Session};
use crate::view::{draw_view, SpectrumViewState, ViewKind};
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
            help_open: false,
            weighting: WeightingCurve::Z,
            integration: "fast",
        }
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

    fn handle_action(&mut self, action: Action) {
        let ViewKind::Spectrum(state) = &mut self.view;
        match action {
            Action::ToggleHelp => self.help_open = !self.help_open,
            Action::MoveCursorLeft => state.move_cursor(0.95),
            Action::MoveCursorRight => state.move_cursor(1.05),
            Action::ZoomFreqIn => state.freq_range = state.freq_range.zoom(0.9),
            Action::ZoomFreqOut => state.freq_range = state.freq_range.zoom(1.1),
            Action::ZoomDbIn => state.db_range = state.db_range.zoom(0.9),
            Action::ZoomDbOut => state.db_range = state.db_range.zoom(1.1),
            Action::PanFreqLeft => state.freq_range = state.freq_range.pan(0.95),
            Action::PanFreqRight => state.freq_range = state.freq_range.pan(1.05),
            Action::TriggerSnapshot => {
                if let Some(session) = &self.session {
                    // Errors surface as a disconnected/no-op state,
                    // never a crash — snapshot trigger failing (e.g.
                    // no session) is an expected, recoverable UI path.
                    let _ = crate::snapshot_flow::trigger_and_fetch(session.client());
                }
            }
            Action::OpenSnapshot | Action::CycleWeighting | Action::CycleIntegration => {
                // File dialog / re-derivation wiring is UX-gated
                // (routing: "trace distinction... is UX's call") —
                // the orchestration functions themselves
                // (snapshot_flow::open_local/rederive_scene) are
                // implemented and tested; wiring a file picker is
                // deferred to the UX pass, not a numeric concern.
            }
            Action::Quit => {
                if let Some(session) = &mut self.session {
                    session.stop();
                }
            }
        }
    }
}

impl eframe::App for AcViewApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.input(|i| {
            for binding in BINDINGS {
                if i.key_pressed(binding.key) {
                    self.handle_action(binding.action);
                }
            }
        });

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

        let ViewKind::Spectrum(state) = &self.view;
        let ranges = (
            (state.freq_range.min(), state.freq_range.max()),
            (state.db_range.min(), state.db_range.max()),
        );
        // Rebuild the scene once per pass — never once per backlog
        // frame — either because a new frame arrived or because
        // zoom/pan changed the ranges. Range-only changes must also
        // rebuild, or zoom/pan appears frozen on a paused/snapshot
        // scene until the next frame happens to arrive.
        if let Some(wire_frame) = &self.last_frame {
            if got_new_frame || self.last_scene_ranges != Some(ranges) {
                self.scene = Some(Scene::from_wire_frame(wire_frame, ranges.0, ranges.1));
                self.last_scene_ranges = Some(ranges);
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
        draw_view(&self.view, ui, self.scene.as_ref());

        if self.help_open {
            egui::Window::new("help").show(&ctx, |ui| {
                ui.label(crate::keys::help_text());
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
