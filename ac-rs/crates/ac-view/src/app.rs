//! The eframe shell: input handling, session polling, and drawing via
//! [`crate::view::draw_view`]. This is the only file allowed to touch
//! `eframe`/`egui::Context` directly — everything else in the crate is
//! toolkit-agnostic and unit-testable without a window.

use std::time::{Duration, Instant};

use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_scene::Scene;

use crate::keys::{bindings_for, Action};
use crate::session::{ConnectionState, PolledFrame, Session};
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

/// The app's state, in four groups: what it is connected to, what it
/// has received and built from that, how healthy the stream is, and
/// the UI chrome on top. The groups are marked below because the field
/// list is long enough that which concern a field belongs to stops
/// being obvious from its name alone.
pub struct AcViewApp {
    // --- connection and active view ---
    session: Option<Session>,
    endpoint: Endpoint,
    view: ViewKind,

    // --- held frames and the scenes built from them. Rebuilt as a
    // group, once per pass, by `rebuild_scenes`; never mutated in
    // place. Exactly one of `scene` / `transfer_scene` is populated,
    // decided by `view`. ---
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
    /// One built `TransferScene` per `TransferViewState::loaded` entry
    /// (#321), index-aligned, rebuilt every pass the same "never mutate,
    /// always rebuild from held state" discipline `transfer_scene` itself
    /// follows — each run's `PairDerivation` is static, but the shared
    /// freq/db range and that run's own `Smoothing` are not, so a
    /// zoom/pan or an `N` press must reach every loaded run's curve, not
    /// just the live one.
    loaded_scenes: Vec<ac_scene::TransferScene>,
    /// The last `visualize/ir` sidecar frame received (#286), kept for
    /// the same zoom/pan-independent-of-new-frame reason `last_frame`
    /// is: today the IR panel has no zoom/pan of its own, but rebuilding
    /// from the held frame rather than only on arrival keeps the two
    /// frame types symmetric instead of one being a special case.
    last_ir_frame: Option<ac_scene::IrWireFrame>,
    /// Built only when the Transfer view is active AND its IR panel is
    /// open (`H`) — the accessory-panel cost should not be paid every
    /// frame just because a sidecar frame arrived.
    ir_scene: Option<ac_scene::IrScene>,
    meters: (ac_scene::MeterState, ac_scene::MeterState),
    /// The fault indicator's cross-frame state (#228) — the refusal clock
    /// and the lock-acquired transient. Lives beside `meters` for the same
    /// reason: it is time-dependent, so it cannot be rebuilt from the last
    /// frame alone.
    fault: ac_scene::FaultState,

    // --- stream health, backing the status line's `malformed` state ---
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
    // --- UI chrome ---
    help_open: bool,
    /// The settings overlay (`G`, transfer view). `None` = closed.
    settings: Option<crate::settings::SettingsOverlay>,

    // --- launch parameters, replayed verbatim on a settings relaunch ---
    weighting: WeightingCurve,
    integration: &'static str,

    // --- stimulus safety ---
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
            loaded_scenes: Vec::new(),
            last_ir_frame: None,
            ir_scene: None,
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
    ///
    /// `drive_max_dbfs` is the stimulus ceiling, seeded from config by
    /// the caller so the client clamp matches the server's
    /// authoritative one. Passed in rather than loaded here: `main` has
    /// already read the config to resolve the channels, so re-reading
    /// it inside a constructor was a second disk hit that could observe
    /// a different file than the first, and it made the constructor
    /// untestable without touching the real user config.
    pub fn new_transfer(endpoint: Endpoint, drive_max_dbfs: f64) -> Self {
        let mut app = Self::new(endpoint);
        app.view = ViewKind::Transfer(TransferViewState::new(drive_max_dbfs, -30.0));
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

    /// Every loaded stored run's currently built scene (#321),
    /// index-aligned with the active view's `TransferViewState::loaded` —
    /// the same test-support role `current_transfer_scene` plays for the
    /// live trace, closing the same hole: a `loaded[i].smoothing` change
    /// that fails to reach the built curve.
    pub fn current_loaded_scenes(&self) -> &[ac_scene::TransferScene] {
        &self.loaded_scenes
    }

    /// The IR scene currently being drawn (#286), if the IR panel is
    /// open and a sidecar frame has been received — the same
    /// test-support role `current_transfer_scene` plays.
    pub fn current_ir_scene(&self) -> Option<&ac_scene::IrScene> {
        self.ir_scene.as_ref()
    }

    /// Parse one raw DATA-topic `visualize/ir` value into an
    /// `IrWireFrame`. A parse failure is dropped silently rather than
    /// feeding `frame_parse_failures` (#193): that streak is specifically
    /// the `WireFrame` schema boundary for the status line's `malformed`
    /// state, and the IR panel is an on-demand accessory with no status
    /// line of its own — losing one sidecar frame does not stop the
    /// transfer view from rendering.
    fn ingest_raw_ir_frame(&mut self, frame: serde_json::Value) {
        if let Ok(ir_frame) = serde_json::from_value::<ac_scene::IrWireFrame>(frame) {
            self.last_ir_frame = Some(ir_frame);
        }
    }

    /// Rebuild `ir_scene` from `last_ir_frame` if the Transfer view's IR
    /// panel is open, else clear it — the one place this decision is
    /// made, called from both the live paint pass and the test helpers
    /// below so they can't drift apart.
    fn rebuild_ir_scene(&mut self) {
        let open = matches!(&self.view, ViewKind::Transfer(t) if t.ir_panel_open());
        self.ir_scene = if open {
            self.last_ir_frame
                .as_ref()
                .map(|f| ac_scene::IrScene::from_input(&ac_scene::IrInput::from_wire_frame(f)))
        } else {
            None
        };
    }

    /// Rebuild the active view's scenes from the held frames — the one
    /// place this happens, called from both the live paint pass in
    /// `ui()` and the headless test hook below so the two cannot drift.
    /// The inactive view's scene is always cleared, so exactly one of
    /// `scene` / `transfer_scene` is ever populated.
    ///
    /// `got_new_frame` forces a spectrum rebuild; without it the
    /// spectrum scene is rebuilt only when the ranges moved, which is
    /// what keeps zoom/pan live on a paused or slow stream instead of
    /// appearing frozen until the next frame happens to arrive. The
    /// transfer scene has no such gate — its build folds in the
    /// time-dependent meter and fault state, so it must run every pass.
    ///
    /// `now_s` is the scene clock the fault indicator's refusal timer
    /// and lock transient are measured against: `egui`'s frame time in
    /// the app, a controlled value in tests.
    fn rebuild_scenes(&mut self, got_new_frame: bool, now_s: f64) {
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
                self.loaded_scenes.clear();
            }
            ViewKind::Transfer(state) => {
                // dB range for the magnitude pane is fixed for now; the
                // phase pane is a fixed ±180° band inside ac-scene.
                let db_range = (-80.0, 20.0);
                let freq_range = (state.freq_range.min(), state.freq_range.max());
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
                // Every loaded run rebuilt every pass too (#321) — a
                // zoom/pan or an `N` press on a stored run must reach its
                // curve exactly as reliably as the live one's.
                self.loaded_scenes = rebuild_loaded_scenes(state, freq_range, db_range);
                self.scene = None;
            }
        }
        self.rebuild_ir_scene();
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

    /// Feed one wire frame directly, bypassing the ZMQ session — the
    /// hook a headless test uses to drive `current_scene` /
    /// `current_transfer_scene` without a live daemon. Rebuilds the
    /// active view's scene the same way the paint pass does.
    #[cfg(test)]
    pub(crate) fn ingest_frame_for_test(&mut self, frame: ac_scene::WireFrame, now_s: f64) {
        self.last_frame = Some(frame);
        self.rebuild_scenes(true, now_s);
    }

    /// Feed one `visualize/ir` sidecar frame directly, bypassing the ZMQ
    /// session — the IR-panel analogue of [`Self::ingest_frame_for_test`].
    #[cfg(test)]
    pub(crate) fn ingest_ir_frame_for_test(&mut self, frame: ac_scene::IrWireFrame) {
        self.last_ir_frame = Some(frame);
        self.rebuild_ir_scene();
    }

    /// Apply a keypress action in a test, then rebuild the active scene —
    /// so a test can assert the scene changed, not merely the state.
    #[cfg(test)]
    pub(crate) fn press_for_test(&mut self, action: Action, now_s: f64) {
        self.handle_action(action, false);
        if let Some(frame) = self.last_frame.clone() {
            self.ingest_frame_for_test(frame, now_s);
        }
        self.rebuild_ir_scene();
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
            Action::MoveCursorLeft => self.with_spectrum(|s| s.move_cursor(0.95)),
            Action::MoveCursorRight => self.with_spectrum(|s| s.move_cursor(1.05)),
            Action::ZoomFreqIn => self.zoom_freq(0.9),
            Action::ZoomFreqOut => self.zoom_freq(1.1),
            Action::PanFreqLeft => self.pan_freq(0.95),
            Action::PanFreqRight => self.pan_freq(1.05),
            Action::ZoomDbIn => self.with_spectrum(|s| s.db_range = s.db_range.zoom(0.9)),
            Action::ZoomDbOut => self.with_spectrum(|s| s.db_range = s.db_range.zoom(1.1)),
            // -- spectrum view --
            Action::CycleWeighting | Action::CycleIntegration => {
                // Snapshot re-derivation wiring is UX-gated; the
                // rederive_scene orchestration is implemented and tested.
            }
            Action::ToggleRefTrace => {
                self.with_spectrum(|s| s.ref_trace_visible = !s.ref_trace_visible)
            }
            // -- transfer view: toggles --
            Action::ToggleRawPhase => self.with_transfer(|t| t.toggle_raw_phase()),
            Action::CycleDerotReference => self.with_transfer(|t| t.cycle_derot()),
            Action::CycleSmoothing => self.with_transfer(|t| t.cycle_smoothing()),
            Action::OpenSettings => self.open_settings(),
            Action::ToggleIrPanel => self.with_transfer(|t| t.toggle_ir_panel()),
            Action::CycleFocus => self.with_transfer(|t| t.cycle_focus()),
            Action::CloseFocusedRun => self.with_transfer(|t| t.close_focused_stored_run()),
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

    /// Apply `f` to the transfer view's state, if that is the active
    /// view. No-op in the spectrum view.
    ///
    /// Exists so a view-specific key binding reads as the one line of
    /// intent it is, rather than four lines of pattern match around it:
    /// the bindings are already filtered per view by `bindings_for`, so
    /// the match here is a type-level formality on all but a stray
    /// dispatch, and spelling it out nine times buried what each arm
    /// actually did.
    fn with_transfer(&mut self, f: impl FnOnce(&mut TransferViewState)) {
        if let ViewKind::Transfer(t) = &mut self.view {
            f(t);
        }
    }

    /// Spectrum-view counterpart of [`Self::with_transfer`].
    fn with_spectrum(&mut self, f: impl FnOnce(&mut SpectrumViewState)) {
        if let ViewKind::Spectrum(s) = &mut self.view {
            f(s);
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
        // Re-read rather than reuse a construction-time value: `apply`
        // has just persisted the overlay, so this is the one place the
        // ceiling is deliberately picked up fresh off disk.
        let drive_max_dbfs = ac_core::config::load(None)
            .unwrap_or_default()
            .drive_max_dbfs;
        self.with_transfer(|t| {
            t.stimulus =
                crate::stimulus::StimulusMachine::new(drive_max_dbfs, applied.start_level_dbfs);
        });
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

impl AcViewApp {
    /// Route this frame's keypresses. The **order is the safety
    /// invariant**, not an implementation detail: the panic cluster is
    /// checked and consumed first, before any modal or normal binding
    /// sees a key, so a live stimulus can always be stopped from the
    /// keyboard. That holds for every present and future modal by
    /// construction — see [`Self::panic_first`].
    fn dispatch_input(&mut self, ctx: &egui::Context) {
        use egui::Key;

        let (space, enter, esc) = ctx.input(|i| {
            (
                i.key_pressed(Key::Space),
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::Escape),
            )
        });

        if self.panic_first(space, enter, esc) {
            // The panic keypress owns this frame — no further dispatch.
            return;
        }

        if self.settings.is_some() {
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
            return;
        }

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

    /// Drain every queued frame from the session into the held-frame
    /// fields. Returns whether any `transfer_stream` frame was accepted
    /// this pass, which is what gates the spectrum scene rebuild.
    fn drain_frames(&mut self) -> bool {
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
            // borrow above. Split by the tagged `PolledFrame` (#286) rather
            // than merged — a `transfer_stream` frame and its `visualize/ir`
            // sidecar are independent JSON objects and go to independent
            // ingest paths.
            let mut drained = Vec::new();
            let mut drained_ir = Vec::new();
            while let Some(frame) = session.poll_frame(Duration::from_millis(0)) {
                match frame {
                    PolledFrame::Transfer(v) => drained.push(v),
                    PolledFrame::Ir(v) => drained_ir.push(v),
                }
            }
            let now = std::time::Instant::now();
            for frame in drained {
                if self.ingest_raw_frame(frame, now) {
                    got_new_frame = true;
                }
            }
            // Same "drain to the newest" discipline as the transfer frame
            // above — the last one in the backlog wins.
            for frame in drained_ir {
                self.ingest_raw_ir_frame(frame);
            }
        }
        got_new_frame
    }

    /// The status line for the current session state.
    fn status_line(&mut self) -> String {
        match &self.session {
            None => "no session".to_string(),
            Some(s) => self.status_for_state(s.connection_state(), std::time::Instant::now()),
        }
    }

    /// Stored-run legend rows (#321): label + captured-at timestamp (QA
    /// #336 correctness issue 2 — two runs sharing a basename stay
    /// distinguishable) + built scene + whether it currently has focus,
    /// index-aligned with `loaded_scenes` and `TransferViewState::loaded`.
    /// Empty outside the transfer view or when nothing is loaded —
    /// `draw_view` renders the pre-#321 layout unchanged in that case.
    fn stored_run_refs(&self) -> Vec<(&str, &str, &ac_scene::TransferScene, bool)> {
        match &self.view {
            ViewKind::Transfer(state) => state
                .loaded
                .iter()
                .zip(self.loaded_scenes.iter())
                .enumerate()
                .map(|(i, (run, scene))| {
                    (
                        run.label.as_str(),
                        run.captured_at_utc.as_str(),
                        scene,
                        matches!(state.focus, crate::view::Focus::Stored(idx) if idx == i),
                    )
                })
                .collect(),
            ViewKind::Spectrum(_) => Vec::new(),
        }
    }

    /// The floating windows drawn over the view: help (`?`) and the
    /// settings overlay (`G`). Both read already-built strings — this
    /// crate formats no measurement value of its own.
    fn draw_overlays(&self, ctx: &egui::Context) {
        if self.help_open {
            let help = crate::keys::help_text(self.view.id());
            egui::Window::new("help").show(ctx, |ui| {
                ui.label(help);
            });
        }

        if let Some(overlay) = &self.settings {
            let selected = overlay.selected_row();
            let rows = overlay.rows();
            egui::Window::new("settings")
                .collapsible(false)
                .show(ctx, |ui| {
                    for (row, value) in &rows {
                        let marker = if *row == selected { "▸ " } else { "  " };
                        ui.label(format!("{marker}{}:  {value}", row.label()));
                    }
                    ui.separator();
                    ui.label("↑↓ row   ←→ value   Enter apply   Esc cancel");
                });
        }
    }

    /// Continuous repaint (paced to vsync by egui/eframe) while a
    /// session is live, so the display updates every frame without
    /// needing mouse-move input events to force a repaint — the
    /// sluggish-at-rest bug this replaces. Lazy repaint when idle so a
    /// static "no session" screen doesn't burn a CPU core.
    fn request_next_repaint(&self, ctx: &egui::Context) {
        if self.session.is_some() {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

impl eframe::App for AcViewApp {
    /// One frame. Deliberately kept to the sequence itself — the order
    /// of these steps carries the invariants (panic-before-modal input
    /// dispatch, keepalive before any early return, exactly one scene
    /// rebuild per pass rather than one per backlog frame), so it
    /// should be readable end to end without scrolling.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        self.dispatch_input(&ctx);
        // Per-frame keepalive, gated on panic-reachability (see
        // keepalive_tick).
        self.keepalive_tick(std::time::Instant::now());

        let got_new_frame = self.drain_frames();
        // Rebuild the scenes once per pass — never once per backlog
        // frame — either because a new frame arrived or because zoom/pan
        // changed the ranges. Shared with the headless test hook so the
        // paint pass and the tests cannot rebuild by different rules.
        self.rebuild_scenes(got_new_frame, ctx.input(|i| i.time));

        let status = self.status_line();
        ui.label(status);
        let stored_refs = self.stored_run_refs();
        draw_view(
            &self.view,
            ui,
            self.scene.as_ref(),
            self.transfer_scene.as_ref(),
            &stored_refs,
            self.ir_scene.as_ref(),
        );

        self.draw_overlays(&ctx);
        self.request_next_repaint(&ctx);
    }
}

/// Rebuild every loaded stored run's `TransferScene` (#321) from its held
/// `PairDerivation` and its own `Smoothing`, under the shared freq/db
/// range every trace in the transfer view is drawn against — the
/// loaded-runs analogue of the live scene rebuild `rebuild_scenes` does. A
/// free function (not a method) so it can be called with `state`
/// borrowed from `&self.view` and its result assigned into a different
/// field (`self.loaded_scenes`) without a whole-self borrow conflict.
fn rebuild_loaded_scenes(
    state: &crate::view::TransferViewState,
    freq_range: (f64, f64),
    db_range: (f64, f64),
) -> Vec<ac_scene::TransferScene> {
    state
        .loaded
        .iter()
        .map(|run| {
            crate::snapshot_flow::rederive_transfer_scene(
                &run.derivation,
                &run.channel_role,
                run.sr,
                ac_scene::DisplayModes::new(ac_scene::DerotMode::Session, run.smoothing),
                freq_range,
                db_range,
            )
        })
        .collect()
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
        None,
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
    drive_max_dbfs: f64,
) -> anyhow::Result<AcViewApp> {
    connect_and_launch_view(
        endpoint,
        meas_channel,
        ref_channel,
        weighting,
        integration,
        Some(drive_max_dbfs),
    )
}

/// Shared body of the two entry points. `drive_max_dbfs` doubles as the
/// view selector — `Some` is the transfer view and carries the stimulus
/// ceiling it needs, `None` is the spectrum view, which has no stimulus
/// and so has nothing to clamp. Encoding it this way rather than as a
/// separate `transfer: bool` means the spectrum path structurally
/// cannot be handed a ceiling it would silently drop.
fn connect_and_launch_view(
    endpoint: Endpoint,
    meas_channel: u32,
    ref_channel: u32,
    weighting: WeightingCurve,
    integration: &'static str,
    drive_max_dbfs: Option<f64>,
) -> anyhow::Result<AcViewApp> {
    let client = Client::connect(&endpoint)?;
    let mut session = Session::new(client);
    // No `drive` param — neither entry (UI or CLI) ever launches driving.
    session.launch(meas_channel, ref_channel, weighting, integration)?;
    let mut app = match drive_max_dbfs {
        Some(max) => AcViewApp::new_transfer(endpoint, max),
        None => AcViewApp::new(endpoint),
    };
    app.session = Some(session);
    app.weighting = weighting;
    app.integration = integration;
    Ok(app)
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
