//! The eframe shell: input handling, session polling, and drawing via
//! [`crate::view::draw_view`]. This is the only file allowed to touch
//! `eframe`/`egui::Context` directly — everything else in the crate is
//! toolkit-agnostic and unit-testable without a window.

use std::time::{Duration, Instant};

use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_scene::Scene;

use crate::keys::{bindings_for, Action};
use crate::session::{ConnectionState, Session};
use crate::view::{draw_view, SpectrumViewState, TransferViewState, ViewKind};
use crate::zmq_client::{Client, Endpoint};

/// Grace window a run of `WireFrame`-parse failures must clear before the
/// status line flips from `live` to `malformed` (#193) — a single bad
/// frame in an otherwise-healthy stream must not flicker the status.
/// Same magnitude as `Session`'s `DISCONNECT_AFTER`, by the UX design's
/// reasoning (no second unexplained number); tracked as its own constant
/// because the two gate different failure classes — this one is the
/// `WireFrame` schema boundary, that one is raw socket silence.
const MALFORMED_GRACE: Duration = Duration::from_secs(10);

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
    /// The fault indicator's cross-frame state (#228) — the refusal clock
    /// and the lock-acquired transient. Lives beside `meters` for the same
    /// reason: it is time-dependent, so it cannot be rebuilt from the last
    /// frame alone.
    fault: ac_scene::FaultState,
    /// Consecutive DATA frames since the last one that parsed into a
    /// `WireFrame` — resets to 0 on every successful parse (#193). Distinct
    /// from `Session::malformed_frames`, which counts a different failure
    /// class one layer down (wire/topic-level decode, `Recv::Malformed`) —
    /// this counts frames that decoded fine off the wire but failed the
    /// `WireFrame` schema.
    frame_parse_failures: u32,
    /// When the current parse-failure streak started, so `MALFORMED_GRACE`
    /// can be measured from it. `None` while the streak is 0.
    first_malformed_since: Option<Instant>,
    help_open: bool,
    /// The settings overlay (`G`, transfer view). `None` = closed.
    settings: Option<crate::settings::SettingsOverlay>,
    weighting: WeightingCurve,
    integration: &'static str,
    /// The single seam a future key-capturing UI mode flips to signal the
    /// panic cluster can't reach the machine this frame — gates the
    /// keepalive (`panic_reachable`). `false` in production today
    /// (panic-first keeps the panic keys reachable); a modal that ever
    /// swallows them sets it, and the keepalive goes silent so the
    /// dead-man takes over.
    panic_keys_obstructed: bool,
    /// Every `DriveCmd` relayed, recorded so app-adapter tests can assert
    /// what reached `set_drive` without a live daemon — the layer between
    /// the proven machine and the wire, which is where the drive-path
    /// hole lived.
    #[cfg(test)]
    sent_drive: Vec<crate::stimulus::DriveCmd>,
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
            fault: ac_scene::FaultState::default(),
            frame_parse_failures: 0,
            first_malformed_since: None,
            help_open: false,
            settings: None,
            weighting: WeightingCurve::Z,
            integration: "fast",
            panic_keys_obstructed: false,
            #[cfg(test)]
            sent_drive: Vec::new(),
        }
    }

    /// Construct in the transfer view rather than the default spectrum
    /// view. `ac transfer` (M4d-CLI, #185) launches through this; the
    /// view is fixed at construction and never switches (§1).
    pub fn new_transfer(endpoint: Endpoint) -> Self {
        let mut app = Self::new(endpoint);
        // Seed the stimulus ceiling from config's `drive_max_dbfs` so the
        // client clamp matches the server's authoritative one.
        let cfg = ac_core::config::load(None).unwrap_or_default();
        app.view = ViewKind::Transfer(TransferViewState::new(cfg.drive_max_dbfs, -30.0));
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

    /// Parse one raw DATA-topic value into a `WireFrame`, updating the
    /// consecutive-failure streak that backs the `malformed` status state
    /// (#193). This is the actual ingest boundary — both the live drain
    /// loop in `ui()` and the headless test below go through it, so a
    /// test exercises the same `serde_json::from_value` failure path a
    /// real malformed frame hits. Returns `true` if the frame was
    /// accepted (`self.last_frame` updated).
    fn ingest_raw_frame(&mut self, frame: serde_json::Value, now: Instant) -> bool {
        match serde_json::from_value::<ac_scene::WireFrame>(frame) {
            Ok(wire_frame) => {
                self.last_frame = Some(wire_frame);
                self.frame_parse_failures = 0;
                self.first_malformed_since = None;
                true
            }
            Err(_) => {
                if self.frame_parse_failures == 0 {
                    self.first_malformed_since = Some(now);
                }
                self.frame_parse_failures += 1;
                false
            }
        }
    }

    /// Whether the parse-failure streak has cleared `MALFORMED_GRACE` —
    /// the gate between "one bad frame" (ignored) and "not rendering"
    /// (reported).
    fn malformed_active(&self, now: Instant) -> bool {
        self.frame_parse_failures > 0
            && self
                .first_malformed_since
                .is_some_and(|t| now.duration_since(t) >= MALFORMED_GRACE)
    }

    /// Render the status line for a given raw connection state — split out
    /// from `ui()` so a headless test can drive the `malformed` branch
    /// with an explicit `ConnectionState::Live` instead of needing a real
    /// ZMQ session (`Session::connection_state` is not constructible
    /// without a socket).
    ///
    /// A real `Disconnected` transition clears the parse-failure streak
    /// (#301 review): `Session::connection_state()` only reports
    /// `Disconnected` once frames stop arriving entirely for
    /// `DISCONNECT_AFTER`, so a streak that was building pre-outage does
    /// not describe anything "consecutive" once the session actually
    /// dropped and came back — carrying it forward would report a stale
    /// count and skip `MALFORMED_GRACE` on the first frame of a new run.
    fn status_for_state(&mut self, state: ConnectionState, now: Instant) -> String {
        match state {
            ConnectionState::NoSession => "no session".to_string(),
            ConnectionState::Disconnected => {
                self.frame_parse_failures = 0;
                self.first_malformed_since = None;
                format!(
                    "disconnected — {}:{} not responding",
                    self.endpoint.host, self.endpoint.ctrl_port
                )
            }
            ConnectionState::Live => {
                if self.malformed_active(now) {
                    format!(
                        "malformed — {}:{} — {} consecutive frames dropped, not rendering",
                        self.endpoint.host, self.endpoint.ctrl_port, self.frame_parse_failures
                    )
                } else {
                    format!("live — {}:{}", self.endpoint.host, self.endpoint.ctrl_port)
                }
            }
        }
    }

    /// Test-only entry point onto [`Self::ingest_raw_frame`] — the raw-JSON
    /// seam the parse-failure path needs, since `ingest_frame_for_test`
    /// below takes an already-parsed `WireFrame` and so cannot exercise a
    /// malformed one.
    #[cfg(test)]
    pub(crate) fn ingest_raw_for_test(&mut self, frame: serde_json::Value, now: Instant) -> bool {
        self.ingest_raw_frame(frame, now)
    }

    /// Test-only entry point onto [`Self::status_for_state`].
    #[cfg(test)]
    pub(crate) fn status_for_test(&mut self, state: ConnectionState, now: Instant) -> String {
        self.status_for_state(state, now)
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
                        ac_scene::DisplayModes::new(state.derot_mode(), state.smoothing),
                        freq_range,
                        (-80.0, 20.0),
                        &mut self.meters,
                        &mut self.fault,
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
                // Best-effort drive-off on a clean quit (§5); the dead-man
                // is the guarantee if the process dies uncleanly.
                let off = self.transfer_stimulus(|m, _| m.on_quit());
                self.send_drive(off);
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
            Action::CycleSmoothing => {
                if let ViewKind::Transfer(t) = &mut self.view {
                    t.cycle_smoothing()
                }
            }
            Action::OpenSettings => self.open_settings(),
            Action::Relock => {
                // Best-effort, same discipline as `set_drive`: a failed
                // send is not a crash, it is a re-lock that did not
                // happen, and the daemon's own retry path is unaffected.
                if let Some(session) = &self.session {
                    let _ = session.client().call(&serde_json::json!({"cmd": "relock"}));
                }
            }
            // -- transfer view: stimulus. Each key drives the safety
            // machine; the DriveCmd it emits (if any) goes to the daemon
            // via set_drive. The machine owns arm/fire/stop, auto-disarm,
            // clamp, and keepalive — the app only relays. --
            Action::StimulusArmOrStop => {
                let cmd = self.transfer_stimulus(|m, now| m.press_space(now));
                self.send_drive(cmd);
            }
            Action::StimulusFireOrStop => {
                let cmd = self.transfer_stimulus(|m, now| m.press_enter(now));
                self.send_drive(cmd);
            }
            Action::StimulusCancel => {
                let cmd = self.transfer_stimulus(|m, now| m.press_esc(now));
                self.send_drive(cmd);
            }
            Action::StimulusLevelUp => {
                let cmd = self.transfer_stimulus(|m, now| m.press_up(now, shift));
                self.send_drive(cmd);
            }
            Action::StimulusLevelDown => {
                let cmd = self.transfer_stimulus(|m, now| m.press_down(now, shift));
                self.send_drive(cmd);
            }
        }
    }

    /// Apply `f` to the transfer view's stimulus machine with a fresh
    /// `now`, returning any command it emits. No-op (None) in the
    /// spectrum view.
    fn transfer_stimulus(
        &mut self,
        f: impl FnOnce(
            &mut crate::stimulus::StimulusMachine,
            std::time::Instant,
        ) -> Option<crate::stimulus::DriveCmd>,
    ) -> Option<crate::stimulus::DriveCmd> {
        if let ViewKind::Transfer(t) = &mut self.view {
            f(&mut t.stimulus, std::time::Instant::now())
        } else {
            None
        }
    }

    /// Relay a stimulus command to the daemon. Best-effort — a send
    /// failure surfaces as the dead-man dropping drive, never a crash.
    fn send_drive(&mut self, cmd: Option<crate::stimulus::DriveCmd>) {
        let Some(cmd) = cmd else { return };
        #[cfg(test)]
        self.sent_drive.push(cmd);
        if let Some(session) = &self.session {
            session.set_drive(cmd.on, cmd.level_dbfs);
        }
    }

    /// Open the settings overlay — but **auto-stop the drive first** if
    /// the stimulus is live (ratified fix, PR #197). Configuration is an
    /// idle-state activity and `apply()` relaunches the session anyway,
    /// so there is nothing to preserve by keeping the drive on under the
    /// menu — and leaving it on is exactly what trapped the panic-stop.
    /// Toggling closed (already open) is a cancel: no side effects.
    fn open_settings(&mut self) {
        if self.settings.is_some() {
            self.settings = None;
            return;
        }
        let stop = self.transfer_stimulus(|m, _| m.on_stop());
        self.send_drive(stop);
        let start = match &self.view {
            ViewKind::Transfer(t) => t.stimulus.level_dbfs(),
            _ => -30.0,
        };
        let cfg = ac_core::config::load(None).unwrap_or_default();
        self.settings = Some(crate::settings::SettingsOverlay::from_config(&cfg, start));
    }

    /// **Drive-precedes-modal-dispatch** (structural safety invariant,
    /// PR #197). While the stimulus is live (Armed/Driving), the panic
    /// cluster — Space / Enter / Esc — means STOP, and nothing intercepts
    /// it: not the settings modal, not any modal added later. Runs before
    /// modal or normal dispatch and consumes the frame's input when it
    /// fires. Returns whether it consumed the input.
    ///
    /// This holds by construction for every future modal — the class of
    /// bug that opened this hole was a modal (settings) added with no
    /// awareness of the stimulus invariant.
    fn panic_first(&mut self, space: bool, enter: bool, esc: bool) -> bool {
        let live = matches!(
            &self.view,
            ViewKind::Transfer(t) if t.stimulus.state() != crate::stimulus::StimState::Idle
        );
        if !live || !(space || enter || esc) {
            return false;
        }
        let cmd = self.transfer_stimulus(|m, now| {
            if space {
                m.press_space(now)
            } else if enter {
                m.press_enter(now)
            } else {
                m.press_esc(now)
            }
        });
        self.send_drive(cmd);
        true
    }

    /// Whether the panic path can reach the machine this frame — the gate
    /// on the keepalive (ratified backstop, PR #197). With
    /// [`Self::panic_first`] running unconditionally before every modal,
    /// the panic keys are always reachable, so this is `true` in
    /// production today. `panic_keys_obstructed` is the single seam a
    /// future key-capturing UI mode flips: set it, and the keepalive stops
    /// asserting the drive — handing the daemon's 1.5 s dead-man back its
    /// job instead of the UI's own tick keeping an un-stoppable drive
    /// alive (the exact mechanism that made the trapped-modal hole
    /// lethal).
    fn panic_reachable(&self) -> bool {
        !self.panic_keys_obstructed
    }

    /// Per-frame keepalive, gated on panic-reachability (PR #197). While
    /// Driving and reachable, re-sends the current state every 250 ms so
    /// the daemon's dead-man never trips on a live session; while the
    /// panic path is obstructed, it stays **silent** so the dead-man is
    /// the backstop. `now` is the frame clock (real time in the app, a
    /// controlled instant in tests).
    fn keepalive_tick(&mut self, now: std::time::Instant) {
        if !self.panic_reachable() {
            return;
        }
        let cmd = if let ViewKind::Transfer(t) = &mut self.view {
            t.stimulus.tick(now)
        } else {
            None
        };
        self.send_drive(cmd);
    }

    /// Route a frame's overlay keypresses. Enter applies (persist +
    /// relaunch); Esc/`G` cancels with zero side effects (just drops the
    /// overlay). Everything else edits in memory only.
    fn handle_settings_keys(&mut self, ev: SettingsKeys) {
        let Some(overlay) = &mut self.settings else {
            return;
        };
        if ev.esc {
            self.settings = None; // cancel — nothing written
            return;
        }
        if ev.up {
            overlay.move_row(false);
        }
        if ev.down {
            overlay.move_row(true);
        }
        if ev.left {
            overlay.adjust_value(false);
        }
        if ev.right {
            overlay.adjust_value(true);
        }
        if ev.enter {
            // Persist (last-writer-wins) then relaunch on the new
            // channels and reseed the stimulus start level.
            match overlay.apply(None) {
                Ok(applied) => {
                    self.settings = None;
                    self.relaunch(applied);
                }
                Err(e) => {
                    eprintln!("ac-view: settings apply failed: {e}");
                    self.settings = None;
                }
            }
        }
    }

    /// Relaunch the session on new channels and reseed the transfer
    /// view's stimulus (drive off, idle, new start level + ceiling). The
    /// session is stopped first so the daemon's busy guard accepts the
    /// new `transfer_stream`.
    fn relaunch(&mut self, applied: crate::settings::Applied) {
        if let Some(session) = &mut self.session {
            session.stop();
            let _ = session.launch(
                applied.meas_channel,
                applied.ref_channel,
                self.weighting,
                self.integration,
            );
        }
        if let ViewKind::Transfer(t) = &mut self.view {
            let cfg = ac_core::config::load(None).unwrap_or_default();
            t.stimulus =
                crate::stimulus::StimulusMachine::new(cfg.drive_max_dbfs, applied.start_level_dbfs);
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

/// One frame's overlay-navigation keypresses (M4c settings modal).
#[derive(Default)]
struct SettingsKeys {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    enter: bool,
    esc: bool,
}

impl eframe::App for AcViewApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        use egui::Key;

        // Drive-precedes-modal-dispatch: the panic cluster is checked and
        // consumed FIRST, before any modal or normal binding sees a key,
        // so a live stimulus can always be stopped from the keyboard —
        // holds for every present and future modal.
        let (space, enter, esc) = ctx.input(|i| {
            (
                i.key_pressed(Key::Space),
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::Escape),
            )
        });
        let panic_consumed = self.panic_first(space, enter, esc);

        if panic_consumed {
            // The panic keypress owns this frame — no further dispatch.
        } else if self.settings.is_some() {
            let mut ev = SettingsKeys::default();
            ctx.input(|i| {
                ev.up = i.key_pressed(Key::ArrowUp);
                ev.down = i.key_pressed(Key::ArrowDown);
                ev.left = i.key_pressed(Key::ArrowLeft);
                ev.right = i.key_pressed(Key::ArrowRight);
                ev.enter = i.key_pressed(Key::Enter);
                ev.esc = i.key_pressed(Key::Escape) || i.key_pressed(Key::G);
            });
            self.handle_settings_keys(ev);
        } else {
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
        }

        // Per-frame keepalive, gated on panic-reachability (see
        // keepalive_tick).
        self.keepalive_tick(std::time::Instant::now());

        let mut got_new_frame = false;
        if let Some(session) = &mut self.session {
            // Drain to the newest queued frame rather than parsing one
            // per repaint: the daemon publishes faster than the UI
            // repaints, so a single `if let` would fall progressively
            // behind. `self.last_frame` is overwritten each iteration,
            // so the backlog is discarded and only the freshest frame
            // survives — correct for a live display.
            //
            // This claim is only true because `poll_frame` skips frame types
            // this crate does not consume instead of reporting them as
            // end-of-stream. It did the latter until issue #219, and the
            // interleaved `visualize/ir` frame published behind every transfer
            // frame ended this loop after exactly one, whatever the backlog:
            // measured at 1 surfaced out of 75 available after a 2 s stall.
            // The comment was accurate about intent and wrong about behaviour
            // for as long as that held, so treat it as load-bearing rather
            // than descriptive — if `poll_frame`'s contract changes back,
            // this loop silently stops draining again.
            //
            // Collected first, then fed through `ingest_raw_frame` below:
            // that call needs `&mut self` for the parse-failure streak
            // (#193), which can't overlap `session`'s own `&mut self.session`
            // borrow above.
            let mut drained = Vec::new();
            while let Some(frame) = session.poll_frame(Duration::from_millis(0)) {
                drained.push(frame);
            }
            let now = std::time::Instant::now();
            for frame in drained {
                if self.ingest_raw_frame(frame, now) {
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
                        ac_scene::DisplayModes::new(state.derot_mode(), state.smoothing),
                        freq_range,
                        db_range,
                        &mut self.meters,
                        &mut self.fault,
                        now_s,
                    ));
                }
                self.scene = None;
            }
        }

        let status = match &self.session {
            None => "no session".to_string(),
            Some(s) => self.status_for_state(s.connection_state(), std::time::Instant::now()),
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

        if let Some(overlay) = &self.settings {
            let selected = overlay.selected_row();
            let rows = overlay.rows();
            egui::Window::new("settings")
                .collapsible(false)
                .show(&ctx, |ui| {
                    for (row, value) in &rows {
                        let marker = if *row == selected { "▸ " } else { "  " };
                        ui.label(format!("{marker}{}:  {value}", row.label()));
                    }
                    ui.separator();
                    ui.label("↑↓ row   ←→ value   Enter apply   Esc cancel");
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

/// Resolve the transfer session's measurement and reference channels
/// from config (M4c, #182). The hardcoded 0/1 is gone: `input_channel`
/// is the measurement leg, and `reference_channel` is **required** —
/// the transfer view is a two-channel H1 estimate with no meaningful
/// default reference, so a missing one is a fatal error carrying the
/// exact fix, not a silent fallback that would measure against the
/// wrong port.
pub fn resolve_transfer_channels(cfg: &ac_core::config::Config) -> Result<(u32, u32), String> {
    let reference = cfg.reference_channel.ok_or_else(|| {
        "reference channel not configured — run `ac setup reference <N>`".to_string()
    })?;
    Ok((cfg.input_channel, reference))
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
    connect_and_launch_view(
        endpoint,
        meas_channel,
        ref_channel,
        weighting,
        integration,
        false,
    )
}

/// Same as [`connect_and_launch`] but starts in the **transfer** view
/// (`ac transfer`, M4d-CLI #185). The launched session is identical — a
/// plain `transfer_stream`, drive **off**: `session.launch` sends no
/// `drive` param and this path adds none, so a CLI launch structurally
/// cannot bring a session up already driving (the load-bearing AC). The
/// only difference from the spectrum entry is which view renders the
/// frames.
pub fn connect_and_launch_transfer(
    endpoint: Endpoint,
    meas_channel: u32,
    ref_channel: u32,
    weighting: WeightingCurve,
    integration: &'static str,
) -> anyhow::Result<AcViewApp> {
    connect_and_launch_view(
        endpoint,
        meas_channel,
        ref_channel,
        weighting,
        integration,
        true,
    )
}

fn connect_and_launch_view(
    endpoint: Endpoint,
    meas_channel: u32,
    ref_channel: u32,
    weighting: WeightingCurve,
    integration: &'static str,
    transfer: bool,
) -> anyhow::Result<AcViewApp> {
    let client = Client::connect(&endpoint)?;
    let mut session = Session::new(client);
    // No `drive` param — neither entry (UI or CLI) ever launches driving.
    session.launch(meas_channel, ref_channel, weighting, integration)?;
    let mut app = if transfer {
        AcViewApp::new_transfer(endpoint)
    } else {
        AcViewApp::new(endpoint)
    };
    app.session = Some(session);
    app.weighting = weighting;
    app.integration = integration;
    Ok(app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Action;
    use crate::stimulus::StimState;

    fn transfer_app() -> AcViewApp {
        let mut app = AcViewApp::new(Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        });
        // Ceiling −10, start −20; no session (sends still record).
        app.view = ViewKind::Transfer(TransferViewState::new(-10.0, -20.0));
        app
    }

    fn stim_state(app: &AcViewApp) -> StimState {
        match &app.view {
            ViewKind::Transfer(t) => t.stimulus.state(),
            _ => panic!("not transfer view"),
        }
    }

    fn drive(app: &mut AcViewApp) {
        app.handle_action(Action::StimulusArmOrStop, false); // Idle -> Armed
        app.handle_action(Action::StimulusFireOrStop, false); // Armed -> Driving
        assert_eq!(stim_state(app), StimState::Driving);
    }

    // The fix's primary guarantee: opening settings while Driving stops
    // the drive first — it never stays on under the menu.
    #[test]
    fn opening_settings_while_driving_auto_stops_the_drive() {
        let mut app = transfer_app();
        drive(&mut app);
        app.sent_drive.clear();

        app.handle_action(Action::OpenSettings, false);

        assert_eq!(
            stim_state(&app),
            StimState::Idle,
            "drive not stopped on open"
        );
        assert!(app.settings.is_some(), "overlay did not open");
        let last = app.sent_drive.last().expect("an off must be relayed");
        assert!(
            !last.on,
            "opening settings while driving must relay set_drive off"
        );
    }

    // The structural invariant: even with a modal open (simulating a
    // future modal that does NOT auto-stop), the panic cluster stops a
    // live machine before the modal sees the key. This is the AC "panic
    // works from Driving regardless of UI modal state" through the adapter.
    #[test]
    fn panic_first_stops_a_live_machine_even_with_a_modal_open() {
        let mut app = transfer_app();
        drive(&mut app);
        // Force the overlay open WITHOUT auto-stop — a future modal might.
        app.settings = Some(crate::settings::SettingsOverlay::from_config(
            &ac_core::config::Config::default(),
            -20.0,
        ));
        app.sent_drive.clear();

        // Esc arrives. panic_first must consume it and stop the drive —
        // not let the overlay's Esc-cancel swallow it.
        let consumed = app.panic_first(false, false, true);

        assert!(consumed, "panic key must be consumed by the stop path");
        assert_eq!(stim_state(&app), StimState::Idle, "drive not stopped");
        let last = app.sent_drive.last().expect("an off must be relayed");
        assert!(!last.on, "panic must relay set_drive off");
    }

    // Each panic key (Space/Enter/Esc) stops from Driving through the
    // adapter — the machine's own test proves the transition; this proves
    // the app relays the off for each.
    #[test]
    fn every_panic_key_relays_off_from_driving() {
        for key in ["space", "enter", "esc"] {
            let mut app = transfer_app();
            drive(&mut app);
            app.sent_drive.clear();
            let consumed = app.panic_first(key == "space", key == "enter", key == "esc");
            assert!(consumed, "{key} not consumed");
            assert_eq!(stim_state(&app), StimState::Idle, "{key} did not stop");
            assert!(!app.sent_drive.last().unwrap().on, "{key} relayed no off");
        }
    }

    // Drive the machine directly to Driving at a controlled instant, so
    // the keepalive cadence can be exercised on logical time.
    fn drive_at(app: &mut AcViewApp, t0: std::time::Instant) {
        if let ViewKind::Transfer(t) = &mut app.view {
            t.stimulus.press_space(t0);
            t.stimulus.press_enter(t0); // last_send = t0
        }
        assert_eq!(stim_state(app), StimState::Driving);
        app.sent_drive.clear();
    }

    // Keepalive backstop, happy path: while Driving and reachable, a tick
    // past the 250 ms interval relays set_drive on.
    #[test]
    fn keepalive_relays_on_while_driving_and_reachable() {
        let mut app = transfer_app();
        let t0 = std::time::Instant::now();
        drive_at(&mut app, t0);
        assert!(app.panic_reachable());

        app.keepalive_tick(t0 + std::time::Duration::from_millis(300));

        let last = app
            .sent_drive
            .last()
            .expect("keepalive must relay while reachable");
        assert!(last.on, "reachable keepalive must assert the drive");
    }

    // Keepalive backstop, the hazard that matters: when the panic path is
    // obstructed, the keepalive stays SILENT — no set_drive on — so the
    // daemon's dead-man takes over instead of the UI's tick keeping an
    // un-stoppable drive alive.
    #[test]
    fn keepalive_stays_silent_when_panic_is_obstructed() {
        let mut app = transfer_app();
        let t0 = std::time::Instant::now();
        drive_at(&mut app, t0);

        app.panic_keys_obstructed = true; // a future capturing modal
        assert!(!app.panic_reachable());

        // Even well past the keepalive interval, nothing is relayed.
        app.keepalive_tick(t0 + std::time::Duration::from_millis(300));
        app.keepalive_tick(t0 + std::time::Duration::from_millis(600));

        assert!(
            app.sent_drive.is_empty(),
            "obstructed keepalive must not assert the drive — got {:?}",
            app.sent_drive
        );
    }

    // panic_first is a no-op when Idle (does not consume keys the normal
    // dispatch needs — e.g. Enter/Esc/Space in the settings overlay).
    #[test]
    fn panic_first_is_a_noop_when_idle() {
        let mut app = transfer_app();
        assert_eq!(stim_state(&app), StimState::Idle);
        assert!(
            !app.panic_first(true, true, true),
            "idle must not consume panic keys"
        );
        assert!(app.sent_drive.is_empty());
    }

    #[test]
    fn missing_reference_channel_is_a_fatal_error_with_the_setup_hint() {
        let cfg = ac_core::config::Config {
            input_channel: 2,
            reference_channel: None,
            ..ac_core::config::Config::default()
        };
        let err = resolve_transfer_channels(&cfg).unwrap_err();
        assert!(
            err.contains("ac setup reference"),
            "error must carry the fix hint: {err}"
        );
    }

    #[test]
    fn configured_channels_resolve_to_input_and_reference() {
        let cfg = ac_core::config::Config {
            input_channel: 2,
            reference_channel: Some(5),
            ..ac_core::config::Config::default()
        };
        assert_eq!(resolve_transfer_channels(&cfg).unwrap(), (2, 5));
    }

    fn transfer_frame() -> ac_scene::WireFrame {
        serde_json::from_value(transfer_frame_json()).expect("wire frame")
    }

    /// The raw wire JSON `transfer_frame` parses — split out so a test can
    /// mutate it (e.g. drop a required field) before it reaches
    /// `serde_json::from_value`, exercising the same boundary the app's
    /// ingest path does.
    fn transfer_frame_json() -> serde_json::Value {
        // A mis-estimated delay so the wire carries non-zero phase and
        // Session (τ_derot 0) differs from the other modes — otherwise
        // cycling would be a no-op and the test could not fail on the bug
        // it names.
        // Built separately: one `json!` deep enough to hold the stage
        // table blows the macro's recursion limit.
        let stages = serde_json::Value::Array(vec![
            serde_json::json!({"decim": 1, "rate": 96000.0, "df": 23.4375, "window_s": 0.042666666666666665, "hop_s": 0.021333333333333333, "f_valid": 1623.0, "settling_s": 0.10666666666666667}),
            serde_json::json!({"decim": 8, "rate": 12000.0, "df": 2.9296875, "window_s": 0.3413333333333333, "hop_s": 0.17066666666666666, "f_valid": 202.88, "settling_s": 0.8533333333333333}),
            serde_json::json!({"decim": 24, "rate": 4000.0, "df": 0.9765625, "window_s": 1.024, "hop_s": 0.512, "f_valid": 67.63, "settling_s": 2.56}),
        ]);
        let mtw = serde_json::json!({
            "freqs": [100.0, 1000.0, 10000.0],
            "magnitude_db": [-6.0, -6.0, -6.0],
            "phase_deg": [-18.0, -180.0, 60.0],
            "coherence": [0.9, 0.9, 0.9],
            "df": [0.9765625, 2.9296875, 23.4375],
            "window_s": [1.024, 0.3413333333333333, 0.042666666666666665],
            "n": [4, 4, 4],
            "stage": [2, 1, 0],
            "blend": [0.0, 0.0, 0.0],
            "bins": [1, 3, 21],
            "ppo": 48.0,
            "n_blocks": 4,
            "stages": stages,
        });
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
            // Full-rate Welch arrays. Still on the wire, deliberately NOT
            // the display's source since the three-stage switch — kept here
            // so this fixture stays a realistic frame, and so a regression
            // that started reading them again would show up as these values
            // appearing on screen instead of the `mtw` ones below.
            "freqs": [100.0, 1000.0, 10000.0],
            "magnitude_db": [-99.0, -99.0, -99.0],
            "phase_deg": [11.0, 22.0, 33.0],
            "coherence": [0.9, 0.9, 0.9],
            "delay_samples": 96,
            "delay_ms": 2.0,
            "meas_peak_dbfs": -6.0,
            "ref_peak_dbfs": -12.0,
            // What the display actually draws (built above).
            "mtw": mtw
        });
        json
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

    /// A frame whose columns are close enough together for a smoothing
    /// window to hold more than one of them (#229). `transfer_frame`'s three
    /// decade-apart columns cannot exercise smoothing: at 1/24 octave each is
    /// alone in its own window, so a real bug would pass.
    fn dense_transfer_frame() -> ac_scene::WireFrame {
        let n = 24;
        // 1/48-octave spacing, stepped by repeated multiplication with the
        // ratio written out as a literal. Raising two to a fractional power
        // here would trip this crate's own AC1 guard, which scans `src/`
        // including test code and is right to — the exception would be the
        // crack that lets real arithmetic back in.
        const RATIO: f64 = 1.014_545_334_9; // 2^(1/48)
        let freqs: Vec<f64> = (0..n)
            .scan(1000.0, |f, _| {
                let out = *f;
                *f *= RATIO;
                Some(out)
            })
            .collect();
        let mag: Vec<f64> = (0..n)
            .map(|i| if i % 2 == 0 { -14.0 } else { -26.0 })
            .collect();
        let mtw = serde_json::json!({
            "freqs": freqs,
            "magnitude_db": mag,
            "phase_deg": vec![0.0_f64; n],
            "coherence": vec![0.9_f64; n],
            "df": vec![1.0_f64; n],
            "window_s": vec![1.0_f64; n],
            "n": vec![4_u32; n],
            "stage": vec![0_usize; n],
            "blend": vec![0.0_f64; n],
            "bins": vec![1_u32; n],
            "ppo": 48.0,
            "n_blocks": 4,
            "stages": [],
        });
        let mut f = transfer_frame();
        f.mtw = serde_json::from_value(mtw).expect("mtw columns");
        f
    }

    // Scene-accessor AC, the same rule the derot keys are held to: the
    // smoothing key must change the BUILT magnitude pane, not merely the
    // state field.
    #[test]
    fn cycling_smoothing_changes_the_built_transfer_magnitude() {
        let mut app = AcViewApp::new_transfer(Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        });
        app.ingest_frame_for_test(dense_transfer_frame(), 0.0);

        let before = app
            .current_transfer_scene()
            .expect("scene built")
            .magnitude
            .segments
            .clone();
        assert_eq!(
            app.current_transfer_scene().unwrap().smoothing_readout,
            None,
            "a session must open unsmoothed"
        );

        app.press_for_test(Action::CycleSmoothing, 0.1);

        let after = app.current_transfer_scene().expect("scene rebuilt");
        assert_ne!(
            &before, &after.magnitude.segments,
            "cycling smoothing did not change the built magnitude pane"
        );
        assert_eq!(
            after.smoothing_readout,
            Some("smoothing 1/24 octave"),
            "the smoothed trace must say so on screen"
        );
    }

    /// A refusing frame, built from the healthy fixture so only the fields
    /// the indicator reads differ.
    fn refusing_frame() -> ac_scene::WireFrame {
        refusing_frame_at_attempt(3)
    }

    /// The same, with the attempt count set: escalation is the later of
    /// `PERSISTENT_REFUSAL_S` and `PERSISTENT_REFUSAL_ATTEMPTS` (#247), so a
    /// test that advances the clock has to advance the count with it.
    fn refusing_frame_at_attempt(attempts: u32) -> ac_scene::WireFrame {
        let mut f = transfer_frame();
        f.drive = Some(ac_scene::WireDrive {
            on: true,
            level_dbfs: Some(-30.0),
            drivable: true,
        });
        f.delay_locked = Some(false);
        // The estimator has answered and refused (#238), so this is a frame a
        // current daemon could publish. It is not what makes the row paint
        // here — `transfer_frame` carries `mtw`, so the settled gate already
        // covers it. The field-reachable no-ladder shape is pinned in
        // `ac-scene::fault` and in `it_transfer_geometry`.
        f.delay_attempts = attempts;
        f
    }

    /// #228: the refusal clock lives on the app, not on the frame, so a
    /// persistent refusal must be reachable by feeding identical frames as
    /// scene time advances. A per-frame implementation would show LOST LOCK
    /// forever and the operator would never be told to move the mic.
    #[test]
    fn a_refusal_becomes_persistent_as_scene_time_advances() {
        let mut app = AcViewApp::new_transfer(Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        });

        app.ingest_frame_for_test(refusing_frame(), 0.0);
        assert_eq!(
            app.current_transfer_scene().expect("scene built").fault,
            // Never locked in this session, so nothing was lost — the
            // transient row says NO LOCK without the instruction.
            Some(ac_scene::Fault::NoLockYet)
        );

        // The same refusal, retried at the daemon's 1 Hz, until both the
        // clock and the attempt count have passed their thresholds. The
        // frames still say nothing new — the row changes because the app
        // carries the state, which is what this test is for.
        let first = 3;
        for n in 1..ac_scene::fault::PERSISTENT_REFUSAL_ATTEMPTS {
            app.ingest_frame_for_test(refusing_frame_at_attempt(first + n), n as f64);
            assert_eq!(
                app.current_transfer_scene().expect("scene rebuilt").fault,
                Some(ac_scene::Fault::NoLockYet),
                "escalated at attempt {n} of the refusal, before either \
                 threshold was reached"
            );
        }
        app.ingest_frame_for_test(
            refusing_frame_at_attempt(first + ac_scene::fault::PERSISTENT_REFUSAL_ATTEMPTS),
            ac_scene::fault::PERSISTENT_REFUSAL_S,
        );
        assert_eq!(
            app.current_transfer_scene().expect("scene rebuilt").fault,
            Some(ac_scene::Fault::NoLock)
        );
    }

    /// And a lock ends it, with the transient confirmation on the way past.
    #[test]
    fn a_lock_clears_the_refusal_and_confirms_itself() {
        let mut app = AcViewApp::new_transfer(Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        });
        app.ingest_frame_for_test(refusing_frame(), 0.0);

        let mut locked = refusing_frame();
        locked.delay_locked = Some(true);
        app.ingest_frame_for_test(locked.clone(), 1.0);
        assert_eq!(
            app.current_transfer_scene().unwrap().fault,
            Some(ac_scene::Fault::LockAcquired)
        );

        app.ingest_frame_for_test(locked, 1.0 + ac_scene::fault::LOCK_ACQUIRED_HOLD_S);
        assert_eq!(app.current_transfer_scene().unwrap().fault, None);
    }

    /// The #225 session in one test: driving, reference leg dead, and the
    /// screen says which leg rather than leaving the operator to infer it
    /// from a wrong-looking top end.
    #[test]
    fn a_dead_reference_leg_names_itself_on_the_transfer_scene() {
        let mut app = AcViewApp::new_transfer(Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        });
        let mut frame = refusing_frame();
        frame.delay_locked = Some(true);
        frame.ref_peak_dbfs = None;
        app.ingest_frame_for_test(frame, 0.0);
        assert_eq!(
            app.current_transfer_scene().unwrap().fault,
            Some(ac_scene::Fault::NoReference)
        );
    }

    /// Today's daemon sends neither field. The indicator must stay silent
    /// rather than read absent levels as silence.
    #[test]
    fn a_frame_without_drive_state_shows_no_indicator() {
        let mut app = AcViewApp::new_transfer(Endpoint {
            host: "localhost".into(),
            ctrl_port: 0,
            data_port: 0,
        });
        app.ingest_frame_for_test(transfer_frame(), 0.0);
        assert_eq!(app.current_transfer_scene().unwrap().fault, None);
    }

    // #193: the status line must say `malformed`, with a count, once a run
    // of frames that fail the `WireFrame` schema clears the grace window —
    // driven through `ingest_raw_for_test` (the raw-JSON boundary), not
    // `ingest_frame_for_test`, so the test exercises the same
    // `serde_json::from_value` failure #192's blank-but-"live" view hid.
    #[test]
    fn a_sustained_run_of_malformed_frames_flips_status_to_malformed_with_a_count() {
        let mut app = AcViewApp::new(Endpoint {
            host: "localhost".into(),
            ctrl_port: 5556,
            data_port: 5557,
        });
        let t0 = Instant::now();
        // Missing `sr`, a required field — fails to deserialize into
        // `WireFrame` rather than being silently dropped and forgotten.
        let mut bad = transfer_frame_json();
        bad.as_object_mut().unwrap().remove("sr");

        for _ in 0..7 {
            assert!(
                !app.ingest_raw_for_test(bad.clone(), t0),
                "a frame missing `sr` must fail to parse"
            );
        }
        assert_eq!(app.frame_parse_failures, 7);

        // Before the grace window clears, the status must still read
        // `live` — a run of bad frames must not out-race the grace period.
        assert_eq!(
            app.status_for_test(
                ConnectionState::Live,
                t0 + MALFORMED_GRACE - Duration::from_millis(1)
            ),
            "live — localhost:5556",
            "status flipped before the grace window cleared"
        );

        // Once the grace window clears, `malformed` replaces `live` and
        // carries the streak count — `live` must not appear while every
        // frame is being dropped (acceptance criterion, verbatim).
        let status = app.status_for_test(ConnectionState::Live, t0 + MALFORMED_GRACE);
        assert_eq!(
            status,
            "malformed — localhost:5556 — 7 consecutive frames dropped, not rendering"
        );
    }

    // A single dropped frame in an otherwise-healthy stream must not
    // flicker the status: one bad frame followed by a good one, well
    // inside the grace window, must never read `malformed` — the good
    // frame clears the streak before the grace gate ever gets to fire.
    #[test]
    fn a_single_malformed_frame_followed_by_a_good_one_never_flips_the_status() {
        let mut app = AcViewApp::new(Endpoint {
            host: "localhost".into(),
            ctrl_port: 5556,
            data_port: 5557,
        });
        let t0 = Instant::now();
        let mut bad = transfer_frame_json();
        bad.as_object_mut().unwrap().remove("sr");

        app.ingest_raw_for_test(bad, t0);
        assert!(app.ingest_raw_for_test(transfer_frame_json(), t0 + Duration::from_millis(50)));

        // Checked at every point up to and past the grace window: the
        // streak was cleared by the good frame, so it never fires.
        for elapsed in [
            Duration::from_millis(50),
            MALFORMED_GRACE,
            MALFORMED_GRACE * 10,
        ] {
            assert_eq!(
                app.status_for_test(ConnectionState::Live, t0 + elapsed),
                "live — localhost:5556",
                "a single glitch flickered the status at t0+{elapsed:?}"
            );
        }
    }

    // The happy path (AC): a run of good frames reports live-and-rendering
    // with no false `malformed` indicator, and a parse success clears a
    // prior streak instead of leaving a stale failure count behind it.
    #[test]
    fn good_frames_report_live_and_clear_a_prior_malformed_streak() {
        let mut app = AcViewApp::new(Endpoint {
            host: "localhost".into(),
            ctrl_port: 5556,
            data_port: 5557,
        });
        let t0 = Instant::now();
        let mut bad = transfer_frame_json();
        bad.as_object_mut().unwrap().remove("sr");
        for _ in 0..3 {
            app.ingest_raw_for_test(bad.clone(), t0);
        }
        assert_eq!(app.frame_parse_failures, 3);

        assert!(app.ingest_raw_for_test(transfer_frame_json(), t0 + MALFORMED_GRACE));
        assert_eq!(
            app.frame_parse_failures, 0,
            "a good parse must reset the streak"
        );
        assert_eq!(
            app.status_for_test(ConnectionState::Live, t0 + MALFORMED_GRACE),
            "live — localhost:5556",
            "status must not stay malformed after a good frame"
        );
    }

    // connected-but-no-frames (the third AC state) is `Disconnected`,
    // already distinct from both `live` and the new `malformed` — this
    // pins that a malformed streak never masks it, since `Disconnected`
    // only happens once the raw socket itself has gone quiet.
    #[test]
    fn disconnected_state_is_unaffected_by_a_malformed_streak() {
        let mut app = AcViewApp::new(Endpoint {
            host: "localhost".into(),
            ctrl_port: 5556,
            data_port: 5557,
        });
        let t0 = Instant::now();
        let mut bad = transfer_frame_json();
        bad.as_object_mut().unwrap().remove("sr");
        for _ in 0..3 {
            app.ingest_raw_for_test(bad.clone(), t0);
        }
        assert_eq!(
            app.status_for_test(ConnectionState::Disconnected, t0 + MALFORMED_GRACE),
            "disconnected — localhost:5556 not responding"
        );
    }

    // A real disconnect must not let a stale streak fast-path the grace
    // window on the next session: a malformed streak from before an outage
    // must not survive it, and the first post-reconnect frame must not
    // skip MALFORMED_GRACE (#301 review).
    #[test]
    fn a_streak_does_not_survive_a_real_disconnect() {
        let mut app = AcViewApp::new(Endpoint {
            host: "localhost".into(),
            ctrl_port: 5556,
            data_port: 5557,
        });
        let t0 = Instant::now();
        let mut bad = transfer_frame_json();
        bad.as_object_mut().unwrap().remove("sr");

        // Streak builds and clears the grace window before the outage.
        for _ in 0..5 {
            app.ingest_raw_for_test(bad.clone(), t0);
        }
        assert_eq!(
            app.status_for_test(ConnectionState::Live, t0 + MALFORMED_GRACE),
            "malformed — localhost:5556 — 5 consecutive frames dropped, not rendering"
        );

        // The daemon actually goes away — real disconnect, no frames at all.
        let t_reconnect = t0 + MALFORMED_GRACE + Duration::from_secs(15);
        assert_eq!(
            app.status_for_test(ConnectionState::Disconnected, t_reconnect),
            "disconnected — localhost:5556 not responding"
        );

        // Session resumes; first frame back is bad again. This is a *new*
        // run — it must get its own grace window, not inherit the old one.
        assert!(!app.ingest_raw_for_test(bad, t_reconnect));
        assert_eq!(
            app.status_for_test(
                ConnectionState::Live,
                t_reconnect + Duration::from_millis(1)
            ),
            "live — localhost:5556",
            "post-reconnect streak reused the pre-outage grace timer"
        );
    }
}
