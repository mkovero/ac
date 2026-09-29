//! The eframe shell: input handling, session polling, and drawing via
//! [`crate::view::draw_view`]. This is the only file allowed to touch
//! `eframe`/`egui::Context` directly — everything else in the crate is
//! toolkit-agnostic and unit-testable without a window.

use std::time::{Duration, Instant};

use ac_core::visualize::weighting_curves::WeightingCurve;
use ac_scene::Scene;

use crate::keys::{bindings_for, Action};
use crate::session::{ConnectionState, PolledFrame, Session};
use crate::view::{draw_view_pointer, SpectrumViewState, StoredTrace, TransferViewState, ViewKind};
use crate::zmq_client::{Client, Endpoint};

/// Grace window a run of `TransferFrame`-parse failures must clear before the
/// status line flips from `live` to `malformed` (#193) — a single bad
/// frame in an otherwise-healthy stream must not flicker the status.
/// Same magnitude as `Session`'s `DISCONNECT_AFTER`, by the UX design's
/// reasoning (no second unexplained number); tracked as its own constant
/// because the two gate different failure classes — this one is the
/// `TransferFrame` schema boundary, that one is raw socket silence.
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
    // place. Exactly one of `scene` / the live pairs' scenes is populated,
    // decided by `view`. ---
    scene: Option<Scene>,
    /// The measured pairs, `(meas, ref)` in launch order — the daemon's
    /// pair index (#685). Empty for an app built without a session.
    pairs: Vec<(u32, u32)>,
    /// One [`LivePair`] per measured pair, index-aligned with `pairs`;
    /// always at least one, so a frame reaching an app built without a
    /// session still has somewhere to go.
    live: Vec<LivePair>,
    /// The unwrapped / group-delay pane's range (#695): the union of every
    /// drawn trace's span on the previous pass, so all share one scale. A
    /// pass behind by design: fitting it within the pass would build each
    /// live scene twice, feeding its meters and fault clock twice.
    ///
    /// Held with the view it was fitted in: on the first pass after
    /// `Shift+P` the previous view's range is in another unit (degrees
    /// against milliseconds), and applying it squeezed or stretched the
    /// traces for a frame. A range from another view is not used.
    shared_phase_range: Option<(ac_scene::transfer::PhaseView, (f64, f64))>,
    /// The ranges the current `scene` was last built with, so a
    /// range change alone (no new frame) is detected and triggers a
    /// rebuild from the first pair's held frame.
    last_scene_ranges: Option<((f64, f64), (f64, f64))>,
    /// One built `TransferScene` per `TransferViewState::loaded` entry
    /// (#321), index-aligned, rebuilt every pass the same "never mutate,
    /// always rebuild from held state" discipline each live scene itself
    /// follows — each run's `PairDerivation` is static, but the shared
    /// freq/db range and that run's own `Smoothing` are not, so a
    /// zoom/pan or an `N` press must reach every loaded run's curve, not
    /// just the live one.
    loaded_scenes: Vec<ac_scene::TransferScene>,
    /// Built only when the Transfer view is active AND its IR panel is
    /// open (`H`) — the accessory-panel cost should not be paid every
    /// frame just because a sidecar frame arrived.
    /// Live's display settings at the `Ctrl`+digit press (#702), applied
    /// to the run when its capture arrives — what the operator was looking
    /// at when they stored it, not what they changed to while it stored.
    capture_settings: Option<crate::view::SlotSettings>,
    ir_scene: Option<ac_scene::IrScene>,
    /// Every pair's IRs as they were when live was paused — `(1 s,
    /// arrival)` per pair. Pause freezes the IR panel on them (operator:
    /// "leave trace there, so you could see what was happening" — unlike
    /// the transfer trace, which pause hides to compare slots alone).
    paused_ir: Option<Vec<IrPair>>,

    // --- stream health, backing the status line's `malformed` state ---
    /// Consecutive DATA frames since the last one that parsed into a
    /// `TransferFrame` — resets to 0 on every successful parse (#193). Distinct
    /// from `Session::malformed_frames`, which counts a different failure
    /// class one layer down (wire/topic-level decode, `Recv::Malformed`) —
    /// this counts frames that decoded fine off the wire but failed the
    /// `TransferFrame` schema.
    frame_parse_failures: u32,
    /// When the current parse-failure streak started, so `MALFORMED_GRACE`
    /// can be measured from it. `None` while the streak is 0.
    first_malformed_since: Option<Instant>,
    /// The daemon's frames are being refused for their `wire_version`
    /// (#112): the last refused frame's error, and how many DATA frames have
    /// been refused since the state began. `None` while every frame's version
    /// is one this build reads.
    ///
    /// Beside the malformed streak, not inside it: a refused frame is never
    /// counted as malformed, and there is no grace window, because a version
    /// mismatch is a property of the daemon build rather than a transient.
    version_refusal: Option<VersionRefusal>,
    // --- UI chrome ---
    help_open: bool,
    /// The settings overlay (`G`, transfer view). `None` = closed.
    settings: Option<crate::settings::SettingsOverlay>,
    /// The typed-delay entry (`T`, transfer view, #669). `None` = closed.
    delay_entry: Option<crate::delay_entry::DelayEntry>,
    /// The typed trace-offset entry (`J`). `None` = closed.
    offset_entry: Option<crate::offset_entry::OffsetEntry>,
    /// A snapshot being taken on its own thread (`S`, #256). One at a time.
    capture_rx: Option<std::sync::mpsc::Receiver<Result<crate::capture::Captured, String>>>,
    /// The slot the running capture is for.
    capture_slot: Option<u8>,
    /// The slot average's scene and legend label (#671), rebuilt every pass
    /// with the stored runs. `None` when off or when it cannot be formed.
    average_scene: Option<(ac_scene::TransferScene, String)>,
    /// The saved-captures list (`F`, #256). `None` = closed.
    file_list: Option<crate::file_list::FileList>,
    /// The target-curve list (`Z`). `None` = closed.
    target_list: Option<crate::file_list::FileList>,
    /// The loaded target curve, and its trace built against the current
    /// axes every pass.
    target: Option<ac_scene::target::TargetCurve>,
    target_trace: Option<crate::view::TargetTrace>,
    /// A target file being read on its thread, with its name.
    target_rx: Option<(
        String,
        std::sync::mpsc::Receiver<Result<ac_scene::target::TargetCurve, String>>,
    )>,
    /// Where `Z` lists from: [`crate::capture::targets_dir`], held so a test
    /// can point it elsewhere.
    targets_dir: std::path::PathBuf,
    /// Where `F` lists from and `C` writes to: [`crate::capture::captures_dir`],
    /// held so a test can point it elsewhere.
    captures_dir: std::path::PathBuf,
    /// `Q` was pressed: close the window at the end of this pass. Set by
    /// the action (which has no `egui::Context`), acted on in `ui()`.
    quit_requested: bool,
    /// The one-line status message (#256): what `S` did. Expires at the
    /// instant held beside it; `None` = stays until replaced.
    toast: Option<(String, Option<Instant>)>,

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
    /// Every `set_delay` request relayed, for the same reason as
    /// `sent_drive`.
    #[cfg(test)]
    sent_delay: Vec<serde_json::Value>,
    /// `set_speed` requests sent (#714), for tests.
    #[cfg(test)]
    sent_speed: Vec<serde_json::Value>,
    /// The preset last asked for with `W`, until a frame reports it (#714,
    /// Codex review): two presses before the next frame must advance twice.
    speed_pending: Option<ac_core::visualize::mtw::ladder::Speed>,
}

impl AcViewApp {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            session: None,
            endpoint,
            view: ViewKind::Spectrum(SpectrumViewState::default()),
            scene: None,
            pairs: Vec::new(),
            live: vec![LivePair::default()],
            shared_phase_range: None,
            last_scene_ranges: None,
            loaded_scenes: Vec::new(),
            capture_settings: None,
            ir_scene: None,
            paused_ir: None,
            frame_parse_failures: 0,
            first_malformed_since: None,
            version_refusal: None,
            help_open: false,
            settings: None,
            delay_entry: None,
            offset_entry: None,
            capture_rx: None,
            capture_slot: None,
            file_list: None,
            target_list: None,
            target: None,
            target_trace: None,
            target_rx: None,
            targets_dir: crate::capture::targets_dir(),
            average_scene: None,
            captures_dir: crate::capture::captures_dir(),
            quit_requested: false,
            toast: None,
            weighting: WeightingCurve::Z,
            integration: "fast",
            panic_keys_obstructed: false,
            #[cfg(test)]
            sent_drive: Vec::new(),
            #[cfg(test)]
            sent_delay: Vec::new(),
            #[cfg(test)]
            sent_speed: Vec::new(),
            speed_pending: None,
        }
    }

    /// Construct in the transfer view rather than the default spectrum
    /// view. `ac transfer` (M4d-CLI, #185) launches through this; the
    /// view is fixed at construction and never switches (§1).
    ///
    /// `drive_max_dbfs` is the fixed stimulus ceiling supplied by the
    /// caller so the editor cannot propose a value the server will refuse.
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
        self.live_selected().scene.as_ref()
    }

    /// Every live pair's built scene (#685), in launch order — `None` for a
    /// pair with no frame yet. The multi-pair analogue of
    /// [`Self::current_transfer_scene`], which is the selected pair's.
    pub fn current_live_scenes(&self) -> Vec<Option<&ac_scene::TransferScene>> {
        self.live.iter().map(|p| p.scene.as_ref()).collect()
    }

    /// Measure `pairs` (#685): one live state per pair, and the view told
    /// how many there are. An empty list keeps one unnamed pair.
    fn set_pairs(&mut self, pairs: Vec<(u32, u32)>) {
        let n = pairs.len().max(1);
        self.live = (0..n).map(|_| LivePair::default()).collect();
        // A new session: nothing held from the old one is its IR, and a
        // preset asked of the old one is not pending in it.
        self.paused_ir = None;
        self.speed_pending = None;
        self.pairs = pairs;
        self.with_transfer(|t| t.set_live_count(n));
    }

    /// The selected live pair's index — the view's choice in the transfer
    /// view, the first pair in the spectrum view.
    fn selected_pair(&self) -> usize {
        let chosen = match &self.view {
            ViewKind::Transfer(t) => t.live_pair,
            ViewKind::Spectrum(_) => 0,
        };
        chosen.min(self.live.len() - 1)
    }

    fn live_selected(&self) -> &LivePair {
        &self.live[self.selected_pair()]
    }

    /// The selected pair's held transfer frame.
    pub(crate) fn last_frame(&self) -> Option<&ac_core::wire::TransferFrame> {
        self.live_selected().frame.as_ref()
    }

    /// The selected pair's held IR sidecar frame.
    #[cfg(test)]
    pub(crate) fn last_ir_frame(&self) -> Option<&ac_core::wire::IrFrame> {
        self.live_selected().ir.as_ref()
    }

    /// Which live pair a frame belongs to, by its channels (#685). With
    /// one pair, or none, every frame is that pair's — a daemon frame and
    /// a test frame alike.
    fn route(&self, meas: i64, reference: i64) -> Option<usize> {
        if self.pairs.len() <= 1 {
            return Some(0);
        }
        self.pairs
            .iter()
            .position(|&(m, r)| i64::from(m) == meas && i64::from(r) == reference)
    }

    /// The name a live pair goes by on screen: `live`, or `live <meas>`
    /// when the session measures several (#685) — the channel number the
    /// operator typed to `ac transfer`.
    fn pair_label(&self, pair: usize) -> String {
        match self.pairs.get(pair) {
            Some((meas, _)) if self.pairs.len() > 1 => format!("live {meas}"),
            _ => "live".to_string(),
        }
    }

    /// Every live pair with a scene, for drawing (#685).
    fn live_traces(&self) -> Vec<crate::view::LiveTrace<'_>> {
        let selected = self.selected_pair();
        let on_live =
            matches!(&self.view, ViewKind::Transfer(t) if t.focus == crate::view::Focus::Live);
        self.live
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                p.scene.as_ref().map(|scene| crate::view::LiveTrace {
                    label: self.pair_label(i),
                    pair: i,
                    scene,
                    selected: on_live && i == selected,
                })
            })
            .collect()
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
    /// `IrFrame`. A parse failure is dropped silently rather than
    /// feeding `frame_parse_failures` (#193): that streak is specifically
    /// the `TransferFrame` schema boundary for the status line's `malformed`
    /// state, and the IR panel is an on-demand accessory with no status
    /// line of its own — losing one sidecar frame does not stop the
    /// transfer view from rendering.
    ///
    /// A version refusal is not dropped silently: it enters the same
    /// `version mismatch` state a refused `transfer_stream` frame does.
    fn ingest_raw_ir_frame(&mut self, frame: serde_json::Value) {
        if !self.admit_wire_version(&frame) {
            return;
        }
        if let Ok(ir_frame) = serde_json::from_value::<ac_core::wire::IrFrame>(frame) {
            if let Some(i) = self.route(ir_frame.meas_channel, ir_frame.ref_channel) {
                self.live[i].hold_ir(ir_frame);
            }
        }
    }

    /// Check one raw DATA frame's `wire_version` before it is parsed
    /// (#112). Returns `false` when the frame is refused.
    ///
    /// On a refusal everything drawn from earlier frames goes too: the held
    /// frames **and** the scenes built from them, because `rebuild_scenes`
    /// leaves a scene standing when its frame is `None`. Those frames came
    /// from a daemon build this client no longer reads, and a trace, banner
    /// or meter left up from them would look current under the new state.
    ///
    /// An accepted version ends the state and resets the count.
    fn admit_wire_version(&mut self, frame: &serde_json::Value) -> bool {
        let error = match ac_core::wire::check_wire_version(frame) {
            Ok(()) => {
                self.version_refusal = None;
                return true;
            }
            Err(error) => error,
        };
        match &mut self.version_refusal {
            Some(r) => {
                r.refused += 1;
                r.error = error;
            }
            None => {
                // Once per transition into the state, not once per frame.
                eprintln!(
                    "ac-view: refusing DATA frames from {}:{}: {}",
                    self.endpoint.host,
                    self.endpoint.ctrl_port,
                    version_detail(&error)
                );
                self.version_refusal = Some(VersionRefusal { error, refused: 1 });
            }
        }
        for pair in &mut self.live {
            pair.frame = None;
            pair.ir = None;
            pair.ir_arrival = None;
            pair.scene = None;
        }
        // Refused frames clear what is drawn, the held pause IR included.
        self.paused_ir = None;
        self.scene = None;
        self.last_scene_ranges = None;
        self.ir_scene = None;
        false
    }

    /// Rebuild `ir_scene` if the Transfer view's IR panel is open, else
    /// clear it — the one place this decision is made, called from both the
    /// live paint pass and the test helpers below so they can't drift apart.
    ///
    /// The panel follows focus (#702): a focused slot shows its stored IR,
    /// live shows the selected pair's sidecar — and nothing while live is
    /// paused, since pause holds the live trace off the screen.
    fn rebuild_ir_scene(&mut self) {
        // Held from the pause keypress (see its handler) until resume.
        if !matches!(&self.view, ViewKind::Transfer(t) if t.paused) {
            self.paused_ir = None;
        }
        let ViewKind::Transfer(t) = &self.view else {
            self.ir_scene = None;
            return;
        };
        if !t.ir_panel_open() {
            self.ir_scene = None;
            return;
        }
        let view = t.ir_view;
        self.ir_scene = match t.focus {
            // A stored run holds its 1 s IR only (#706).
            crate::view::Focus::Stored(i) => t.loaded.get(i).and_then(|run| {
                run.ir.as_ref().map(|ir| {
                    // The run's `←`/`→` nudge moves its arrival marker as it
                    // moves its phase (Codex review): the marker is the
                    // delay the trace is drawn against.
                    let mut ir = ir.clone();
                    ir.delay_ms += run.delay_offset_samples as f64 * 1000.0 / f64::from(run.sr);
                    ac_scene::IrScene::from_input_view(&ir, view)
                        .labelled(format!("{} \u{b7} {}", run.label, IR_LONG_LABEL))
                })
            }),
            crate::view::Focus::Live => {
                // The arrival IR by default (#706); `S` picks the 1 s one.
                // Until a pair has a delay there is no arrival IR, and the
                // panel says it is showing the 1 s one instead. Paused, the
                // IRs held at the pause.
                let i = self.selected_pair();
                let pair = self.live_selected();
                // Paused: only what was held — never a live frame, even when
                // a relaunch or refusal dropped the held one (Codex recheck).
                let (long, arrival) = if t.paused {
                    self.paused_ir
                        .as_ref()
                        .and_then(|held| held.get(i))
                        .map(|(l, a)| (l.as_ref(), a.as_ref()))
                        .unwrap_or((None, None))
                } else {
                    (pair.ir.as_ref(), pair.ir_arrival.as_ref())
                };
                let owner = self.pair_label(i);
                let owner = if t.paused {
                    format!("{owner} \u{b7} PAUSED")
                } else {
                    owner
                };
                let (frame, what) = match (t.ir_arrival, arrival, long) {
                    (true, Some(a), _) => (Some(a), IR_ARRIVAL_LABEL.to_string()),
                    (true, None, long) => {
                        (long, format!("{IR_LONG_LABEL} (no current arrival IR)"))
                    }
                    (false, _, long) => (long, IR_LONG_LABEL.to_string()),
                };
                frame.map(|f| {
                    ac_scene::IrScene::from_input_view(&ac_scene::IrInput::from_wire_frame(f), view)
                        .labelled(format!("{owner} \u{b7} {what}"))
                })
            }
        };
    }

    /// Rebuild the active view's scenes from the held frames — the one
    /// place this happens, called from both the live paint pass in
    /// `ui()` and the headless test hook below so the two cannot drift.
    /// The inactive view's scene is always cleared, so exactly one of
    /// `scene` / the live pairs' scenes is ever populated.
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
                if let Some(wire_frame) = &self.live[0].frame {
                    if got_new_frame || self.last_scene_ranges != Some(ranges) {
                        self.scene = Some(Scene::from_wire_frame(wire_frame, ranges.0, ranges.1));
                        self.last_scene_ranges = Some(ranges);
                    }
                }
                for pair in &mut self.live {
                    pair.scene = None;
                }
                self.loaded_scenes.clear();
            }
            ViewKind::Transfer(state) => {
                // dB range for the magnitude pane is fixed for now; the
                // phase pane is a fixed ±180° band inside ac-scene.
                let db_range = (-80.0, 20.0);
                let freq_range = (state.freq_range.min(), state.freq_range.max());
                let range = self
                    .shared_phase_range
                    .filter(|(view, _)| *view == state.phase_view)
                    .map(|(_, r)| r);
                let phase = (state.phase_view, range);
                let modes = with_phase(
                    ac_scene::DisplayModes::new(state.derot_mode(), state.smoothing)
                        .with_coherence_mask(state.coherence_mask)
                        .with_invert_offset(state.invert, state.offset_db),
                    phase,
                );
                // Every pair every pass, each through its own meter and
                // fault state (#685): both carry time between frames.
                for pair in &mut self.live {
                    let Some(wire_frame) = &pair.frame else {
                        continue;
                    };
                    let input = ac_scene::TransferInput::from_wire_frame(wire_frame);
                    // Eased toward the newest estimate (#716); readouts,
                    // meters and the mask are the frame's either way.
                    let input = if state.tween {
                        let opts = ac_scene::tween::TweenOptions {
                            ease_phase: state.phase_view == ac_scene::transfer::PhaseView::Wrapped,
                            coherence_mask: state.coherence_mask,
                        };
                        pair.tween.sample(&input, now_s, opts)
                    } else {
                        pair.tween.reset();
                        input
                    };
                    let mut live = ac_scene::TransferScene::from_input(
                        &input,
                        modes,
                        freq_range,
                        db_range,
                        &mut pair.meters,
                        &mut pair.fault,
                        now_s,
                    );
                    // Paused (#256) only hides the live trace, in the view;
                    // the scene keeps rolling so the meters, the fault
                    // indicator and the readouts stay live.
                    live.set_protection(wire_frame.protection.as_ref());
                    if pair
                        .track_pending
                        .is_some_and(|(_, at)| pair.frames_in >= at + TRACK_PENDING_FRAMES)
                    {
                        pair.track_pending = None;
                    }
                    pair.scene = Some(live);
                }
                // Every loaded run rebuilt every pass too (#321) — a
                // zoom/pan or an `N` press on a stored run must reach its
                // curve exactly as reliably as the live one's.
                self.loaded_scenes = rebuild_loaded_scenes(state, freq_range, db_range, phase);
                // The slot average (#671), drawn like a stored run in its
                // own colour, unsmoothed, under the same mask.
                self.average_scene = crate::snapshot_flow::average_input(state)
                    .ok()
                    .flatten()
                    .map(|(input, label)| {
                        let scene = ac_scene::TransferScene::from_input(
                            &input,
                            with_phase(
                                ac_scene::DisplayModes::new(
                                    ac_scene::DerotMode::Session,
                                    Default::default(),
                                )
                                .with_coherence_mask(state.coherence_mask),
                                phase,
                            ),
                            freq_range,
                            db_range,
                            &mut Default::default(),
                            &mut Default::default(),
                            0.0,
                        );
                        (scene, label)
                    });
                // The range the next pass draws every trace on: the union
                // of this pass's spans.
                // Only traces on screen count: a hidden run's span would
                // flatten the ones being looked at (Codex review).
                let live_shown = state.live_trace_shown();
                self.shared_phase_range = self
                    .live
                    .iter()
                    .filter(|_| live_shown)
                    .filter_map(|p| p.scene.as_ref())
                    .chain(
                        self.loaded_scenes
                            .iter()
                            .zip(state.loaded.iter())
                            .filter(|(_, run)| run.visible)
                            .map(|(s, _)| s),
                    )
                    .chain(self.average_scene.iter().map(|(s, _)| s))
                    .filter_map(|s| s.phase_span)
                    .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
                    .map(|r| (state.phase_view, r));
                self.scene = None;
                self.target_trace = self.target.as_ref().map(|t| crate::view::TargetTrace {
                    trace: t.trace(freq_range, db_range),
                    caption: t.caption(),
                });
            }
        }
        self.rebuild_ir_scene();
    }

    /// Parse one raw DATA-topic value into a `TransferFrame`, updating the
    /// consecutive-failure streak that backs the `malformed` status state
    /// (#193). This is the actual ingest boundary — both the live drain
    /// loop in `ui()` and the headless test below go through it, so a
    /// test exercises the same `serde_json::from_value` failure path a
    /// real malformed frame hits. Returns `true` if the frame was
    /// accepted (its pair's held frame updated).
    ///
    /// The version check runs first, on the raw value: a frame whose schema
    /// moved too far to parse is still reported as a version mismatch, and a
    /// refused frame never feeds the malformed streak.
    fn ingest_raw_frame(&mut self, frame: serde_json::Value, now: Instant) -> bool {
        if !self.admit_wire_version(&frame) {
            return false;
        }
        match serde_json::from_value::<ac_core::wire::TransferFrame>(frame) {
            Ok(wire_frame) => {
                self.frame_parse_failures = 0;
                self.first_malformed_since = None;
                // A frame for a pair this session did not launch is not
                // malformed, only not ours to draw.
                match self.route(wire_frame.meas_channel, wire_frame.ref_channel) {
                    Some(i) => {
                        self.live[i].hold_frame(wire_frame);
                        true
                    }
                    None => false,
                }
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
                // Same reasoning for a refusal: a reconnect may reach a
                // different daemon, and it starts clean.
                self.version_refusal = None;
                format!(
                    "disconnected — {}:{} not responding",
                    self.endpoint.host, self.endpoint.ctrl_port
                )
            }
            ConnectionState::Live => {
                // Precedence: a refusal explains the empty plot better than
                // a malformed streak, and refused frames never feed one.
                if let Some(r) = &self.version_refusal {
                    format!(
                        "version mismatch — {}:{} — {} — {} frames refused, not rendering",
                        self.endpoint.host,
                        self.endpoint.ctrl_port,
                        version_detail(&r.error),
                        r.refused
                    )
                } else if self.malformed_active(now) {
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
    pub(crate) fn ingest_frame_for_test(
        &mut self,
        frame: ac_core::wire::TransferFrame,
        now_s: f64,
    ) {
        let i = self
            .route(frame.meas_channel, frame.ref_channel)
            .expect("a test frame for a launched pair");
        self.live[i].hold_frame(frame);
        self.rebuild_scenes(true, now_s);
    }

    /// Feed one `visualize/ir` sidecar frame directly, bypassing the ZMQ
    /// session — the IR-panel analogue of [`Self::ingest_frame_for_test`].
    #[cfg(test)]
    pub(crate) fn ingest_ir_frame_for_test(&mut self, frame: ac_core::wire::IrFrame) {
        let i = self
            .route(frame.meas_channel, frame.ref_channel)
            .expect("a test frame for a launched pair");
        self.live[i].hold_ir(frame);
        self.rebuild_ir_scene();
    }

    /// Apply a keypress action in a test, then rebuild the active scene —
    /// so a test can assert the scene changed, not merely the state.
    #[cfg(test)]
    pub(crate) fn press_for_test(&mut self, action: Action, now_s: f64) {
        self.handle_action(action, false);
        self.rebuild_scenes(true, now_s);
        self.rebuild_ir_scene();
    }

    fn handle_action(&mut self, action: Action, shift: bool) {
        match action {
            // -- global --
            Action::ToggleHelp => self.help_open = !self.help_open,
            Action::OpenSnapshot => self.open_file_list(Instant::now()),
            Action::OpenTargets => {
                if shift {
                    self.target = None;
                    self.target_list = None;
                } else {
                    self.open_target_list(Instant::now());
                }
            }
            Action::ExportCsv => self.export_csv(shift, Instant::now()),
            Action::CycleCoherenceMask => self.with_transfer(|t| t.cycle_coherence_mask()),
            Action::ToggleAverage => self.toggle_average(shift, Instant::now()),
            Action::Quit => {
                // Best-effort drive-off on a clean quit (§5); the dead-man
                // is the guarantee if the process dies uncleanly.
                let off = self.transfer_stimulus(|m, _| m.on_quit());
                self.send_drive(off);
                if let Some(session) = &mut self.session {
                    session.stop();
                }
                // And actually exit: stopping the session alone left the
                // window open.
                self.quit_requested = true;
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
            Action::ToggleRawPhase => self.with_transfer(|t| {
                if shift {
                    // `Shift+P` (#695): wrapped → unwrapped → group delay.
                    t.phase_view = t.phase_view.next();
                } else {
                    t.toggle_raw_phase();
                }
            }),
            Action::CycleDerotReference => self.with_transfer(|t| t.cycle_derot()),
            Action::CycleSmoothing => self.with_transfer(|t| t.cycle_smoothing()),
            Action::OpenSettings => self.open_settings(),
            Action::ToggleIrPanel => self.with_transfer(|t| {
                if shift {
                    t.cycle_ir_view();
                } else {
                    t.toggle_ir_panel();
                }
            }),
            Action::CycleSpeed => {
                if shift {
                    // `Shift+W` (#716): ease the live trace, or step it.
                    self.with_transfer(|t| t.tween = !t.tween);
                } else {
                    self.cycle_speed();
                }
            }
            Action::ToggleIrSpan => self.with_transfer(|t| t.ir_arrival = !t.ir_arrival),
            Action::CycleFocus => self.with_transfer(|t| t.cycle_focus()),
            Action::CloseFocusedRun => self.with_transfer(|t| t.close_focused_stored_run()),
            // -- transfer view: the operator's delay (#669). The values
            // come from the scene; this crate adds nothing but the ±1 a
            // nudge is by definition. --
            Action::InsertDelay => {
                if shift {
                    self.send_delay(serde_json::json!({"samples": null}));
                } else if let Some(v) = self
                    .current_transfer_scene()
                    .and_then(|s| s.delay_insert_samples)
                {
                    self.send_delay(serde_json::json!({"samples": v}));
                }
            }
            // Relative, so two presses between frames move it twice: the
            // daemon applies each step to the delay it holds.
            // `←`/`→` (#256): the selected trace's delay, whichever it is —
            // live goes to the daemon as a relative step; a slot moves its
            // own offset, drawn as a phase rotation of its stored curve.
            Action::NudgeDelayEarlier | Action::NudgeDelayLater => {
                let sign = if action == Action::NudgeDelayLater {
                    1
                } else {
                    -1
                };
                let step = sign * if shift { 10 } else { 1 };
                let focus = match &self.view {
                    ViewKind::Transfer(t) => t.focus,
                    ViewKind::Spectrum(_) => return,
                };
                match focus {
                    crate::view::Focus::Live => {
                        if self
                            .current_transfer_scene()
                            .is_some_and(|s| s.delay_samples.is_some())
                        {
                            self.send_delay(serde_json::json!({"step": step}));
                        }
                    }
                    crate::view::Focus::Stored(i) => self.with_transfer(|t| {
                        if let Some(run) = t.loaded.get_mut(i) {
                            run.delay_offset_samples += step;
                        }
                    }),
                }
            }
            Action::TypeDelay => self.delay_entry = Some(Default::default()),
            // `Y` (#687): the daemon owns the rule; this only flips it,
            // from what was last asked if the frame has not caught up.
            Action::ToggleDelayTracking => {
                let i = self.selected_pair();
                let shown = self.live[i]
                    .scene
                    .as_ref()
                    .is_some_and(|s| s.delay_tracking);
                let on = !self.live[i].track_pending.map_or(shown, |(v, _)| v);
                self.live[i].track_pending = Some((on, self.live[i].frames_in));
                self.send_delay(serde_json::json!({"track": on}));
            }
            Action::ToggleInvert => self.with_transfer(|t| t.toggle_invert()),
            Action::TypeOffset => self.offset_entry = Some(Default::default()),
            Action::ToggleTraceVisible => self.with_transfer(|t| {
                if shift {
                    t.show_all();
                } else {
                    t.toggle_focused_visibility();
                }
            }),
            // -- transfer view: stimulus. Each key drives the safety
            // machine; the DriveCmd it emits (if any) goes to the daemon
            // via set_drive. The machine owns arm/fire/stop, auto-disarm,
            // clamp, and keepalive — the app only relays. --
            Action::StimulusArmOrStop => {
                let cmd = self.transfer_stimulus(|m, now| m.press_space(now));
                self.send_drive(cmd);
            }
            // Enter (#256): fire when armed — panic-first normally takes
            // that case before dispatch — and otherwise pause / resume the
            // live trace, including while driving (Space and Esc stop).
            Action::StimulusFireOrPause => {
                let armed = matches!(
                    &self.view,
                    ViewKind::Transfer(t) if t.stimulus.state() == crate::stimulus::StimState::Armed
                );
                if armed {
                    let cmd = self.transfer_stimulus(|m, now| m.press_enter(now));
                    self.send_drive(cmd);
                } else {
                    self.toggle_live_pause();
                }
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

    /// Whether the transfer view's live display is paused (`Z`, #256).
    fn transfer_paused(&self) -> bool {
        matches!(&self.view, ViewKind::Transfer(t) if t.paused)
    }

    /// Set the status message; `for_s: None` keeps it until replaced.
    fn set_toast(&mut self, text: String, now: Instant, for_s: Option<f64>) {
        let until = for_s.map(|s| now + std::time::Duration::from_secs_f64(s));
        self.toast = Some((text, until));
    }

    /// Enter while not armed (#256): hold the live trace, or let it roll.
    fn toggle_live_pause(&mut self) {
        self.with_transfer(|t| t.toggle_pause());
        // The IR panel freezes on the IRs of the keypress itself — not of
        // the next rebuild, by which a newer frame may have been drained
        // (Codex review).
        if self.transfer_paused() {
            self.paused_ir = Some(
                self.live
                    .iter()
                    .map(|p| (p.ir.clone(), p.ir_arrival.clone()))
                    .collect(),
            );
        } else {
            self.paused_ir = None;
        }
    }

    /// `M` / `Shift+M` (#671): the slot average on or off, or its weighting;
    /// the status line says what it averages, or why it cannot.
    fn toggle_average(&mut self, weighting: bool, now: Instant) {
        self.with_transfer(|t| {
            if weighting {
                t.toggle_average_weighting();
            } else {
                t.toggle_average();
            }
        });
        let ViewKind::Transfer(t) = &self.view else {
            return;
        };
        match crate::snapshot_flow::average_input(t) {
            Ok(Some((_, label))) => self.set_toast(label, now, Some(3.0)),
            Ok(None) => self.set_toast("average off".into(), now, Some(2.0)),
            Err(why) => self.set_toast(format!("no average \u{2014} {why}"), now, Some(4.0)),
        }
    }

    /// `F` (#256): open the saved-captures list, or close it.
    fn open_file_list(&mut self, now: Instant) {
        if self.file_list.take().is_some() {
            return;
        }
        let dir = self.captures_dir.clone();
        let list = crate::file_list::FileList::read(&dir);
        if list.entries().is_empty() {
            self.set_toast(
                format!("no saved captures in {}", dir.display()),
                now,
                Some(4.0),
            );
        } else {
            self.file_list = Some(list);
        }
    }

    /// `Z`: open the target-curve list, or close it.
    fn open_target_list(&mut self, now: Instant) {
        if self.target_list.take().is_some() {
            return;
        }
        let dir = self.targets_dir.clone();
        let list = crate::file_list::FileList::read_with(&dir, &crate::capture::TARGET_EXTENSIONS);
        if list.entries().is_empty() {
            self.set_toast(
                format!(
                    "no target curves in {} (.txt, .frd, .csv: freq_hz gain_db per line)",
                    dir.display()
                ),
                now,
                Some(5.0),
            );
        } else {
            self.target_list = Some(list);
        }
    }

    /// `Z` in the list: read the selected target on a thread
    /// ([`crate::capture::spawn_target`]); [`Self::poll_target`] draws it
    /// or says why not.
    fn load_selected_target(&mut self, now: Instant) {
        let Some(path) = self
            .target_list
            .take()
            .and_then(|l| l.selected_path().map(std::path::Path::to_path_buf))
        else {
            return;
        };
        let name = crate::file_list::FileList::name(&path);
        self.set_toast(format!("reading target {name}\u{2026}"), now, None);
        self.target_rx = Some((name, crate::capture::spawn_target(path)));
    }

    /// Collect a finished target read, if any.
    fn poll_target(&mut self, now: Instant) {
        let Some((_, rx)) = &self.target_rx else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("the read ended without a result".to_string())
            }
        };
        let (name, _) = self.target_rx.take().expect("checked above");
        self.finish_target(&name, result, now);
    }

    fn finish_target(
        &mut self,
        name: &str,
        result: Result<ac_scene::target::TargetCurve, String>,
        now: Instant,
    ) {
        match result {
            Ok(t) => {
                // A target is drawn on a trace's axes, in the magnitude
                // pane: say where it is when that is not on screen (Codex
                // review).
                let ir_open = matches!(&self.view, ViewKind::Transfer(v) if v.ir_panel_open());
                let on_screen =
                    self.current_transfer_scene().is_some() || !self.loaded_scenes.is_empty();
                let msg = if ir_open {
                    format!("target {name} loaded \u{2014} H closes the IR panel to show it")
                } else if on_screen {
                    format!("target {name} drawn")
                } else {
                    format!("target {name} loaded \u{2014} it draws with the first trace")
                };
                self.set_toast(msg, now, Some(3.0));
                self.target = Some(t);
            }
            Err(e) => self.set_toast(
                format!("target {name} not loaded \u{2014} {e}"),
                now,
                Some(6.0),
            ),
        }
    }

    /// Wait for a started target read, as a frame's poll would.
    #[cfg(test)]
    pub(crate) fn wait_target_for_test(&mut self) {
        if let Some((name, rx)) = self.target_rx.take() {
            let result = rx
                .recv()
                .unwrap_or_else(|_| Err("the read ended without a result".to_string()));
            self.finish_target(&name, result, Instant::now());
        }
    }

    /// A digit in the list (#256): load the selected file into slot `n`, on
    /// a thread like a live capture.
    fn load_selected_into_slot(&mut self, n: u8, now: Instant) {
        let Some(path) = self
            .file_list
            .as_ref()
            .and_then(|l| l.selected_path())
            .map(std::path::Path::to_path_buf)
        else {
            return;
        };
        if let Some(pending) = self.capture_slot {
            self.set_toast(
                format!("slot {pending} is still being stored"),
                now,
                Some(3.0),
            );
            return;
        }
        self.file_list = None;
        let name = crate::file_list::FileList::name(&path);
        self.capture_rx = Some(crate::capture::spawn_open(path, n));
        self.capture_slot = Some(n);
        self.set_toast(format!("loading {name} into slot {n}\u{2026}"), now, None);
    }

    /// `C` (#256): write the selected trace — live or a slot — to CSV next
    /// to the captures, and say where.
    fn export_csv(&mut self, average: bool, now: Instant) {
        let ViewKind::Transfer(t) = &self.view else {
            return;
        };
        let (name, input) = if average {
            // `Shift+C` (#671): the slot average.
            match crate::snapshot_flow::average_input(t) {
                Ok(Some((input, _))) => ("average".to_string(), input),
                Ok(None) => {
                    self.set_toast("no average shown (M)".into(), now, Some(3.0));
                    return;
                }
                Err(why) => {
                    self.set_toast(format!("no average \u{2014} {why}"), now, Some(4.0));
                    return;
                }
            }
        } else {
            match t.focus {
                crate::view::Focus::Live => match self.last_frame() {
                    Some(f) => (
                        self.pair_label(self.selected_pair()),
                        ac_scene::TransferInput::from_wire_frame(f),
                    ),
                    None => {
                        self.set_toast("no live trace to export".into(), now, Some(3.0));
                        return;
                    }
                },
                crate::view::Focus::Stored(i) => match t.loaded.get(i) {
                    Some(run) => (run.label.clone(), crate::snapshot_flow::run_input(run)),
                    None => return,
                },
            }
        };
        if input.freqs.is_empty() {
            self.set_toast(format!("{name} has no trace yet"), now, Some(3.0));
            return;
        }
        let dir = self.captures_dir.clone();
        let base = format!(
            "{}-{}.csv",
            name.replace(' ', ""),
            ac_core::shared::time::now_utc_filename_stamp()
        );
        // Never overwrite: a second export in the same second gets `-2`.
        let result = std::fs::create_dir_all(&dir)
            .map_err(anyhow::Error::from)
            .and_then(|_| crate::capture::write_new(&dir, &base, input.to_csv(&name).as_bytes()));
        match result {
            Ok((path, _)) => self.set_toast(
                format!("{name} written \u{2014} {}", path.display()),
                now,
                Some(5.0),
            ),
            Err(e) => self.set_toast(format!("CSV not written \u{2014} {e:#}"), now, Some(8.0)),
        }
    }

    /// Bare digit `n` (#256): show or hide slot `n`, or say it is empty.
    fn toggle_slot(&mut self, n: u8, now: Instant) {
        let mut found = true;
        self.with_transfer(|t| found = t.toggle_slot_visibility(n));
        if !found && matches!(self.view, ViewKind::Transfer(_)) {
            self.set_toast(
                format!("slot {n} is empty \u{2014} Ctrl+{n} stores the live trace there"),
                now,
                Some(3.0),
            );
        }
    }

    /// `Ctrl`+`n` (#256): store the live trace to slot `n`, on its own
    /// thread, and say so. Only while live rolls: a snapshot records what
    /// the daemon hears now, so storing while the picture is held would
    /// file a trace the operator is not looking at.
    fn store_slot_request(&mut self, n: u8, now: Instant) {
        if !matches!(self.view, ViewKind::Transfer(_)) {
            return;
        }
        if self.transfer_paused() {
            self.set_toast(
                format!("live is paused \u{2014} press Enter to resume, then store slot {n}"),
                now,
                Some(4.0),
            );
        } else if let Some(pending) = self.capture_slot {
            self.set_toast(
                format!("slot {pending} is still being stored"),
                now,
                Some(3.0),
            );
        } else if self.session.is_none() {
            self.set_toast(
                "no session \u{2014} nothing to store".into(),
                now,
                Some(3.0),
            );
        } else {
            // The selected live pair (#685): the snapshot holds every pair,
            // the slot keeps the one the operator is looking at.
            let settings = match &self.view {
                ViewKind::Transfer(t) => t.live_settings(),
                _ => unreachable!("checked above"),
            };
            self.capture_settings = Some(settings);
            self.capture_rx = Some(crate::capture::spawn(
                self.endpoint.clone(),
                n,
                self.selected_pair(),
                settings,
            ));
            self.capture_slot = Some(n);
            self.set_toast(format!("storing slot {n}\u{2026}"), now, None);
        }
    }

    /// Collect a finished capture, if any: overlay it and report where it
    /// went, or report why it failed.
    fn poll_capture(&mut self, now: Instant) {
        if self
            .toast
            .as_ref()
            .is_some_and(|(_, until)| until.is_some_and(|u| now >= u))
        {
            self.toast = None;
        }
        let Some(rx) = &self.capture_rx else { return };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("capture thread ended without a result".to_string())
            }
        };
        self.capture_rx = None;
        self.capture_slot = None;
        self.finish_capture(result, now);
    }

    fn finish_capture(&mut self, result: Result<crate::capture::Captured, String>, now: Instant) {
        match result {
            Ok(mut captured) => {
                let n = captured.slot;
                let path = captured.path.display().to_string();
                let opened = captured.opened;
                // A live store is drawn as live was at the key press (#702);
                // an opened file already carries its saved settings.
                let settings = self.capture_settings.take();
                let live_settings = match &self.view {
                    ViewKind::Transfer(t) => Some(t.live_settings()),
                    _ => None,
                };
                if !opened {
                    if let Some(s) = settings.or(live_settings) {
                        s.apply(&mut captured.run);
                    }
                }
                self.with_transfer(|t| {
                    t.store_slot(n, captured.run);
                    // A recalled slot is selected; a live store leaves the
                    // selection where it was (#256).
                    if opened {
                        t.select_slot(n);
                    }
                });
                let verb = if opened {
                    "loaded from"
                } else {
                    "stored \u{2014}"
                };
                match &captured.warning {
                    None => self.set_toast(format!("slot {n} {verb} {path}"), now, Some(5.0)),
                    Some(w) => {
                        self.set_toast(format!("slot {n} {verb} {path}; {w}"), now, Some(10.0))
                    }
                }
            }
            Err(e) => {
                self.capture_settings = None;
                self.set_toast(format!("storing failed \u{2014} {e}"), now, Some(8.0))
            }
        }
    }

    /// Feed a capture result directly, as the capture thread would.
    #[cfg(test)]
    pub(crate) fn finish_capture_for_test(
        &mut self,
        result: Result<crate::capture::Captured, String>,
    ) {
        self.finish_capture(result, Instant::now());
    }

    #[cfg(test)]
    pub(crate) fn toast_text(&self) -> Option<&str> {
        self.toast.as_ref().map(|(t, _)| t.as_str())
    }

    /// Relay a `set_delay` to the daemon (#669). Best-effort, same
    /// discipline as `set_drive`: a failed send is a delay that did not
    /// change, visible on the next frame, never a crash.
    ///
    /// Always names the selected pair (#685): without `pair` the daemon
    /// applies the change to every pair.
    /// A click on the panes pins the cursor at that frequency; a
    /// right-click clears it (#718).
    pub(crate) fn apply_pointer(&mut self, p: crate::view::TransferPointer) {
        if p.ir {
            // The IR panel's time cursor (#720), in ms on its own axis.
            let range = self.ir_scene.as_ref().map(|s| s.t_range);
            self.with_transfer(|t| {
                if p.clear {
                    t.ir_cursor_pin = None;
                } else if let (Some(x), Some((lo, hi))) = (p.click_x, range) {
                    t.ir_cursor_pin = Some(ac_scene::ticks::x_to_time(x, lo, hi));
                }
            });
            return;
        }
        self.with_transfer(|t| {
            if p.clear {
                t.cursor_pin = None;
            } else if let Some(x) = p.click_x {
                t.cursor_pin = Some(ac_scene::ticks::x_to_freq(
                    x,
                    t.freq_range.min(),
                    t.freq_range.max(),
                ));
            }
        });
    }

    /// `W` (#714): ask the daemon for the preset after the one the frame
    /// says it runs — the frame, not a copy held here, so a relaunch (which
    /// starts at Detail) cannot leave the key a step out.
    fn cycle_speed(&mut self) {
        use ac_core::visualize::mtw::ladder::Speed;
        let reported = self
            .live_selected()
            .frame
            .as_ref()
            .and_then(|f| f.mtw.as_ref())
            .and_then(|m| m.speed.as_deref())
            .and_then(Speed::from_tag);
        // A request the frames have not caught up with yet is the current
        // one; once a frame reports it, the frame is.
        if self.speed_pending.is_some() && self.speed_pending == reported {
            self.speed_pending = None;
        }
        let current = self.speed_pending.or(reported).unwrap_or_default();
        let next = current.next();
        self.speed_pending = Some(next);
        let request = serde_json::json!({"cmd": "set_speed", "speed": next.tag()});
        #[cfg(test)]
        self.sent_speed.push(request.clone());
        if let Some(why) = self.session.as_ref().and_then(|s| s.control(&request)) {
            self.speed_pending = None;
            self.set_toast(
                format!("speed not changed \u{2014} {why}"),
                Instant::now(),
                Some(6.0),
            );
        }
    }

    fn send_delay(&mut self, mut request: serde_json::Value) {
        request["cmd"] = serde_json::json!("set_delay");
        request["pair"] = serde_json::json!(self.selected_pair());
        #[cfg(test)]
        self.sent_delay.push(request.clone());
        if let Some(why) = self.session.as_ref().and_then(|s| s.control(&request)) {
            self.set_toast(
                format!("delay not changed \u{2014} {why}"),
                Instant::now(),
                Some(6.0),
            );
        }
    }

    /// Route a frame's typed-offset keypresses (`J`): `J` applies to the
    /// selected trace (an empty entry resets it to 0 dB, a value that does
    /// not parse or exceeds ±60 dB applies nothing and says so), Esc
    /// cancels.
    fn handle_offset_entry_keys(&mut self, chars: &str, backspace: bool, apply: bool, esc: bool) {
        let Some(entry) = &mut self.offset_entry else {
            return;
        };
        if esc {
            self.offset_entry = None;
            return;
        }
        chars.chars().for_each(|c| entry.push(c));
        if backspace {
            entry.backspace();
        }
        if apply {
            let value = entry.value();
            let text = entry.text().to_string();
            self.offset_entry = None;
            match value {
                Some(v) => self.with_transfer(|t| t.set_offset(v)),
                None => self.set_toast(
                    format!("offset {text:?} not applied \u{2014} a dB value within \u{b1}60"),
                    Instant::now(),
                    Some(4.0),
                ),
            }
        }
    }

    /// Route a frame's typed-delay keypresses. `T` applies (an empty entry
    /// cancels); Esc cancels — it only reaches here while the stimulus is
    /// idle, since panic-first owns it otherwise.
    fn handle_delay_entry_keys(&mut self, chars: &str, backspace: bool, apply: bool, esc: bool) {
        let Some(entry) = &mut self.delay_entry else {
            return;
        };
        if esc {
            self.delay_entry = None;
            return;
        }
        chars.chars().for_each(|c| entry.push(c));
        if backspace {
            entry.backspace();
        }
        if apply {
            let value = entry.value();
            self.delay_entry = None;
            if let Some(v) = value {
                self.send_delay(serde_json::json!({"samples": v}));
            }
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
        let state = match &self.view {
            ViewKind::Transfer(t) => t.stimulus.state(),
            ViewKind::Spectrum(_) => crate::stimulus::StimState::Idle,
        };
        let live = state != crate::stimulus::StimState::Idle;
        // Enter belongs to the stimulus only while armed — it fires. While
        // driving it pauses the live trace (#256); Space and Esc stop.
        // With a typed entry open (`J`, `T`) Enter applies the entry: firing
        // is a start, not a stop, and an operator finishing a number did
        // not ask to emit (Codex review of #710). Space and Esc still stop.
        let entry_open = self.offset_entry.is_some() || self.delay_entry.is_some();
        let enter = enter && state == crate::stimulus::StimState::Armed && !entry_open;
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
            // Never relaunch under a live stimulus (Codex review, #256):
            // Enter is no longer a stop while driving, so it can reach
            // here with the drive on. Space and Esc stop it first.
            let stim_live = matches!(
                &self.view,
                ViewKind::Transfer(t) if t.stimulus.state() != crate::stimulus::StimState::Idle
            );
            if stim_live {
                self.set_toast(
                    "stop the stimulus (Space or Esc) before applying settings".into(),
                    Instant::now(),
                    Some(4.0),
                );
                return;
            }
            // Several pairs (#685): the overlay edits one meas/ref, and
            // applying it would quietly drop the others. Refused, with
            // the way to change them.
            if self.pairs.len() > 1 {
                self.settings = None;
                self.set_toast(
                    "several pairs are measured \u{2014} relaunch with `ac transfer <channels>` to change them"
                        .into(),
                    Instant::now(),
                    Some(5.0),
                );
                return;
            }
            // Persist (last-writer-wins) then relaunch on the new
            // channels and reseed the stimulus start level.
            let Some(overlay) = &mut self.settings else {
                return;
            };
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
            let launched = session.launch(
                &[(applied.meas_channel, applied.ref_channel)],
                self.weighting,
                self.integration,
            );
            // #643: a refused relaunch leaves no session running; say so
            // until something replaces the message.
            // A relaunch that took replaces whatever failure was showing.
            match launched {
                Err(e) => {
                    self.set_toast(format!("no session \u{2014} {e:#}"), Instant::now(), None)
                }
                Ok(()) => self.set_toast("session relaunched".into(), Instant::now(), Some(2.0)),
            }
        }
        self.set_pairs(vec![(applied.meas_channel, applied.ref_channel)]);
        // Re-read rather than reuse a construction-time value: `apply`
        // has just persisted the overlay, so this is the one place the
        // ceiling is deliberately picked up fresh off disk.
        let drive_max_dbfs = ac_core::shared::emission_level::MAX_EMISSION_DBFS;
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

        if self.offset_entry.is_some() {
            let (chars, backspace, apply, esc) = ctx.input(|i| {
                let chars: String = i
                    .events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Text(t) => Some(t.as_str()),
                        _ => None,
                    })
                    .collect();
                (
                    chars,
                    i.key_pressed(Key::Backspace),
                    // Enter applies, as it would anywhere else; `J`
                    // again still does.
                    i.key_pressed(Key::Enter) || i.key_pressed(Key::J),
                    i.key_pressed(Key::Escape),
                )
            });
            // `J` arrives as text too; it is the apply key.
            let chars: String = chars
                .chars()
                .filter(|c| !c.eq_ignore_ascii_case(&'j'))
                .collect();
            self.handle_offset_entry_keys(&chars, backspace, apply, esc);
            return;
        }

        if self.delay_entry.is_some() {
            let (chars, backspace, apply, esc) = ctx.input(|i| {
                let chars: String = i
                    .events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Text(t) => Some(t.as_str()),
                        _ => None,
                    })
                    .collect();
                (
                    chars,
                    i.key_pressed(Key::Backspace),
                    i.key_pressed(Key::Enter) || i.key_pressed(Key::T),
                    i.key_pressed(Key::Escape),
                )
            });
            // `T` arrives as text too; it is the apply key, not a digit.
            let chars: String = chars
                .chars()
                .filter(|c| !c.eq_ignore_ascii_case(&'t'))
                .collect();
            self.handle_delay_entry_keys(&chars, backspace, apply, esc);
            return;
        }

        // The target list (`Z`) takes its keys: ↑/↓ select, `Z` loads, Esc
        // closes. Esc reaches here only while the stimulus is idle.
        if self.target_list.is_some() {
            let (up, down, load, clear, close) = ctx.input(|i| {
                let z = i.key_pressed(Key::Z);
                (
                    i.key_pressed(Key::ArrowUp),
                    i.key_pressed(Key::ArrowDown),
                    z && !i.modifiers.shift,
                    z && i.modifiers.shift,
                    i.key_pressed(Key::Escape),
                )
            });
            if clear {
                // Shift+Z clears here too (Codex review), as it does
                // outside the list.
                self.handle_action(Action::OpenTargets, true);
                return;
            }
            if close {
                self.target_list = None;
                return;
            }
            if let Some(list) = &mut self.target_list {
                if up {
                    list.move_selection(false);
                }
                if down {
                    list.move_selection(true);
                }
            }
            if load {
                self.load_selected_target(Instant::now());
            }
            return;
        }

        // The saved-captures list (`F`, #256) takes its keys: ↑/↓ select,
        // a digit loads into that slot, Esc or F closes. Esc reaches here
        // only while the stimulus is idle — panic-first owns it otherwise.
        if self.file_list.is_some() {
            let (up, down, close, digit) = ctx.input(|i| {
                // A bare digit loads; Ctrl+digit (store live) does nothing
                // here, read per key event as the slot keys are.
                let digit = i.events.iter().find_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } if !modifiers.ctrl => crate::keys::SLOT_KEYS
                        .iter()
                        .zip(1u8..)
                        .find(|(k, _)| **k == *key)
                        .map(|(_, n)| n),
                    _ => None,
                });
                (
                    i.key_pressed(Key::ArrowUp),
                    i.key_pressed(Key::ArrowDown),
                    i.key_pressed(Key::Escape) || i.key_pressed(Key::F),
                    digit,
                )
            });
            if close {
                self.file_list = None;
                return;
            }
            if let Some(list) = &mut self.file_list {
                if up {
                    list.move_selection(false);
                }
                if down {
                    list.move_selection(true);
                }
            }
            if let Some(n) = digit {
                self.load_selected_into_slot(n, Instant::now());
            }
            return;
        }

        // Digits (#256): `Ctrl`+digit stores the live trace to that slot;
        // a bare digit shows or hides it.
        // `Ctrl` is read from each key event, not from the frame's current
        // modifiers: a Ctrl released before the frame is processed must
        // still store, not toggle (Codex review).
        let slots: Vec<(bool, u8)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } => crate::keys::SLOT_KEYS
                        .iter()
                        .zip(1u8..)
                        .find(|(k, _)| **k == *key)
                        .map(|(_, n)| (modifiers.ctrl, n)),
                    _ => None,
                })
                .collect()
        });
        for (ctrl, n) in slots {
            if ctrl {
                self.store_slot_request(n, Instant::now());
            } else {
                self.toggle_slot(n, Instant::now());
            }
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
        // Drain to the newest queued frame rather than parsing one
        // per repaint: the daemon publishes faster than the UI
        // repaints, so a single `if let` would fall progressively
        // behind. Each pair's held frame is overwritten each iteration,
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
        // this loop silently stops draining again. Enforced by
        // `app_tests::one_drain_pass_over_a_mixed_backlog_keeps_the_newest_frames`.
        //
        // Collected first, then fed through `ingest_raw_frame` in
        // `ingest_drained`: that call needs `&mut self` for the
        // parse-failure streak (#193), which can't overlap `session`'s own
        // `&mut self.session` borrow here.
        let Some(session) = &mut self.session else {
            return false;
        };
        let drained = collect_drained(|| session.poll_frame(Duration::from_millis(0)));
        self.ingest_drained(drained, Instant::now())
    }

    /// Feed one pass's collected frames through the ingest boundary, in
    /// arrival order, so the last of each kind is what stays held. Returns
    /// whether any `transfer_stream` frame was accepted.
    ///
    /// One ordered pass, not transfer frames then IR frames: an arrival IR
    /// ages out by the transfer frames that follow it (#706), so an old one
    /// replayed after the frames that expired it would come back (Codex
    /// recheck).
    fn ingest_drained(&mut self, drained: Vec<PolledFrame>, now: Instant) -> bool {
        let mut got_new_frame = false;
        for frame in drained {
            match frame {
                PolledFrame::Transfer(v) => {
                    if self.ingest_raw_frame(v, now) {
                        got_new_frame = true;
                    }
                }
                PolledFrame::Ir(v) => self.ingest_raw_ir_frame(v),
                // #649: held until replaced — the session is gone.
                PolledFrame::Failed(why) => {
                    self.set_toast(format!("session stopped \u{2014} {why}"), now, None)
                }
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
    fn stored_run_refs(&self) -> Vec<StoredTrace<'_>> {
        match &self.view {
            ViewKind::Transfer(state) => state
                .loaded
                .iter()
                .zip(self.loaded_scenes.iter())
                .enumerate()
                .map(|(i, (run, scene))| StoredTrace {
                    label: run.label.as_str(),
                    captured_at_utc: run.captured_at_utc.as_str(),
                    scene,
                    focused: matches!(state.focus, crate::view::Focus::Stored(idx) if idx == i),
                    visible: run.visible,
                    color_slot: run.color_slot,
                    slot: run.slot,
                })
                .chain(self.average_scene.iter().map(|(scene, label)| StoredTrace {
                    label: label.as_str(),
                    captured_at_utc: "",
                    scene,
                    focused: false,
                    visible: true,
                    color_slot: crate::view::palette::AVERAGE_SLOT,
                    slot: None,
                }))
                .collect(),
            ViewKind::Spectrum(_) => Vec::new(),
        }
    }

    /// The floating windows drawn over the view: help (`?`) and the
    /// settings overlay (`G`). Both read already-built strings — this
    /// crate formats no measurement value of its own.
    fn draw_overlays(&self, ctx: &egui::Context) {
        if self.help_open {
            draw_help(ctx, self.view.id());
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

        if let Some((text, until)) = &self.toast {
            if until.is_some_and(|u| Instant::now() >= u) {
                // Expired; cleared on the next pass that has `&mut self`.
            } else {
                egui::Area::new(egui::Id::new("ac-view-toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -12.0))
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.label(text);
                        });
                    });
            }
        }

        if let Some(list) = &self.target_list {
            egui::Window::new("target curves")
                .collapsible(false)
                .show(ctx, |ui| {
                    for (i, path) in list.entries().iter().enumerate() {
                        let marker = if i == list.selected() {
                            "\u{25b8} "
                        } else {
                            "  "
                        };
                        ui.label(format!(
                            "{marker}{}",
                            crate::file_list::FileList::name(path)
                        ));
                    }
                    ui.separator();
                    ui.label("\u{2191}\u{2193} select   Z draw   Esc close   (Shift+Z clears)");
                });
        }

        if let Some(list) = &self.file_list {
            egui::Window::new("saved captures")
                .collapsible(false)
                .show(ctx, |ui| {
                    for (i, path) in list.entries().iter().enumerate() {
                        let marker = if i == list.selected() {
                            "\u{25b8} "
                        } else {
                            "  "
                        };
                        ui.label(format!(
                            "{marker}{}",
                            crate::file_list::FileList::name(path)
                        ));
                    }
                    ui.separator();
                    ui.label(
                        "\u{2191}\u{2193} select   1\u{2026}9 load into that slot   Esc close",
                    );
                });
        }

        if let Some(entry) = &self.offset_entry {
            egui::Window::new("offset")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(format!("offset (dB):  {}\u{258f}", entry.text()));
                    ui.separator();
                    ui.label("digits, -, .   Backspace   Enter apply (empty: none)   Esc cancel");
                });
        }

        if let Some(entry) = &self.delay_entry {
            egui::Window::new("delay")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(format!("delay (samples):  {}▏", entry.text()));
                    ui.separator();
                    ui.label("digits, -   Backspace   Enter apply   Esc cancel");
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
        self.poll_capture(std::time::Instant::now());
        self.poll_target(std::time::Instant::now());

        let got_new_frame = self.drain_frames();
        // Rebuild the scenes once per pass — never once per backlog
        // frame — either because a new frame arrived or because zoom/pan
        // changed the ranges. Shared with the headless test hook so the
        // paint pass and the tests cannot rebuild by different rules.
        self.rebuild_scenes(got_new_frame, ctx.input(|i| i.time));

        let status = self.status_line();
        ui.label(status);
        let stored_refs = self.stored_run_refs();
        let live_traces = self.live_traces();
        let pointer = draw_view_pointer(
            &self.view,
            ui,
            self.scene.as_ref(),
            self.current_transfer_scene(),
            &live_traces,
            &stored_refs,
            self.ir_scene.as_ref(),
            self.target_trace.as_ref(),
        );
        drop(live_traces);
        drop(stored_refs);
        if let Some(p) = pointer {
            self.apply_pointer(p);
        }

        self.draw_overlays(&ctx);
        if self.quit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
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
    phase: (ac_scene::transfer::PhaseView, Option<(f64, f64)>),
) -> Vec<ac_scene::TransferScene> {
    state
        .loaded
        .iter()
        .map(|run| {
            crate::snapshot_flow::rederive_transfer_scene(
                &run.derivation,
                &run.channel_role,
                run.sr,
                run.delay_offset_samples,
                with_phase(
                    ac_scene::DisplayModes::new(
                        if run.raw_phase {
                            ac_scene::DerotMode::Raw
                        } else {
                            ac_scene::DerotMode::Session
                        },
                        run.smoothing,
                    )
                    .with_coherence_mask(state.coherence_mask)
                    .with_invert_offset(run.invert, run.offset_db),
                    phase,
                ),
                freq_range,
                db_range,
            )
        })
        .collect()
}

/// `modes` with the phase pane's view and, outside the wrapped view, the
/// shared range (#695).
fn with_phase(
    modes: ac_scene::DisplayModes,
    (view, range): (ac_scene::transfer::PhaseView, Option<(f64, f64)>),
) -> ac_scene::DisplayModes {
    let modes = modes.with_phase_view(view);
    match range {
        Some(r) if view != ac_scene::transfer::PhaseView::Wrapped => modes.with_phase_range(r),
        _ => modes,
    }
}

/// `--meas 0,4` (#685): one or more measurement channels, comma
/// separated, each once. Anything else is refused with the reason — a
/// channel list that quietly lost an entry would measure less than asked.
pub fn parse_meas_list(s: &str) -> Result<Vec<u32>, String> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let ch: u32 = part
            .trim()
            .parse()
            .map_err(|_| format!("{s:?} is not a list of channel numbers"))?;
        if out.contains(&ch) {
            return Err(format!("channel {ch} is listed twice"));
        }
        out.push(ch);
    }
    Ok(out)
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
        &[(meas_channel, ref_channel)],
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
///
/// `pairs` is every `(meas, ref)` to measure (#685), in launch order.
pub fn connect_and_launch_transfer(
    endpoint: Endpoint,
    pairs: &[(u32, u32)],
    weighting: WeightingCurve,
    integration: &'static str,
    drive_max_dbfs: f64,
) -> anyhow::Result<AcViewApp> {
    connect_and_launch_view(
        endpoint,
        pairs,
        weighting,
        integration,
        Some(drive_max_dbfs),
    )
}

/// Shared body of the two entry points. `drive_max_dbfs` doubles as the
/// view selector — `Some` is the transfer view and carries the stimulus
/// ceiling it needs, `None` is the spectrum view, which has no stimulus.
/// Encoding it this way rather than as a
/// separate `transfer: bool` means the spectrum path structurally
/// cannot be handed a ceiling it would silently drop.
fn connect_and_launch_view(
    endpoint: Endpoint,
    pairs: &[(u32, u32)],
    weighting: WeightingCurve,
    integration: &'static str,
    drive_max_dbfs: Option<f64>,
) -> anyhow::Result<AcViewApp> {
    let client = Client::connect(&endpoint)?;
    let mut session = Session::new(client);
    // No `drive` param — neither entry (UI or CLI) ever launches driving.
    session.launch(pairs, weighting, integration)?;
    let mut app = match drive_max_dbfs {
        Some(max) => AcViewApp::new_transfer(endpoint, max),
        None => AcViewApp::new(endpoint),
    };
    app.session = Some(session);
    app.set_pairs(pairs.to_vec());
    app.weighting = weighting;
    app.integration = integration;
    Ok(app)
}

/// One pair's `(1 s, arrival)` IR frames.
type IrPair = (
    Option<ac_core::wire::IrFrame>,
    Option<ac_core::wire::IrFrame>,
);

/// The IR panel's name for the arrival IR (#706).
const IR_ARRIVAL_LABEL: &str = "arrival IR 250 ms";
/// The IR panel's name for the 1 s Welch IR.
const IR_LONG_LABEL: &str = "IR 1 s";
/// Transfer frames (50 ms each) without a new arrival IR after which the
/// held one is dropped as stale: 200 ms, three missed 62.5 ms answers.
const ARRIVAL_STALE_FRAMES: u32 = 4;

/// One measured pair's live state (#685). The meters and the fault state
/// carry time from one frame to the next — ballistics, the refusal clock —
/// so each pair keeps its own: fed through one shared state, two pairs'
/// frames would interleave into a reading that belongs to neither.
#[derive(Default)]
struct LivePair {
    /// The last frame received, kept so the scene can be rebuilt on a
    /// zoom/pan (range change) without waiting for the next frame —
    /// otherwise zoom appears frozen on a paused or slow stream.
    frame: Option<ac_core::wire::TransferFrame>,
    /// The last `visualize/ir` sidecar frame (#286), held for the same
    /// reason — the 1 s Welch IR.
    ir: Option<ac_core::wire::IrFrame>,
    /// The last arrival IR (#706, `span: "arrival"`): 250 ms, a new one
    /// every 62.5 ms once the pair has a delay.
    ir_arrival: Option<ac_core::wire::IrFrame>,
    /// Transfer frames since the last arrival IR (#706).
    frames_since_arrival: u32,
    meters: (ac_scene::MeterState, ac_scene::MeterState),
    /// The fault indicator's cross-frame state (#228).
    fault: ac_scene::FaultState,
    /// Built from `frame` every pass in the transfer view.
    scene: Option<ac_scene::TransferScene>,
    /// Frames received for this pair, ever.
    frames_in: u64,
    /// The tracking state `Y` last asked for (#687), with `frames_in` at
    /// the press. Two presses before the frames catch up then toggle twice
    /// instead of sending the same value (Codex review).
    track_pending: Option<(bool, u64)>,
    /// Motion easing toward this pair's newest estimate (#716).
    tween: ac_scene::tween::Tween,
}

impl LivePair {
    /// Hold a transfer frame. An arrival IR aligned at a delay the pair no
    /// longer holds is dropped with it (#706, Codex review): after a
    /// `set_delay`, a re-find or a lost lock the daemon sends nothing until
    /// the new ladder settles, and the old IR would sit on screen centred
    /// on the old delay. The panel falls back to the 1 s IR and says so.
    ///
    /// A delay check alone cannot see a re-find or a restart at the same
    /// delay (Codex recheck), so an arrival IR is also dropped once
    /// [`ARRIVAL_STALE_FRAMES`] transfer frames pass without a new one: it
    /// normally comes every 62.5 ms, and its absence means the daemon is
    /// rebuilding it.
    fn hold_frame(&mut self, frame: ac_core::wire::TransferFrame) {
        self.frames_since_arrival = self.frames_since_arrival.saturating_add(1);
        let stale = self.ir_arrival.as_ref().is_some_and(|a| {
            frame.delay_locked != Some(true)
                || a.delay_samples != frame.delay_samples
                || self.frames_since_arrival > ARRIVAL_STALE_FRAMES
        });
        if stale {
            self.ir_arrival = None;
        }
        // The arrival IR is the ladder's (#706): a frame with no ladder
        // columns (rebuilt — a delay or preset change — or not yet settled)
        // has none to show, and a new preset retired the one it ran in
        // (#714, Codex review). Dropped rather than left to age out.
        let speed = |f: &ac_core::wire::TransferFrame| f.mtw.as_ref().and_then(|m| m.speed.clone());
        let preset_changed = matches!(
            (self.frame.as_ref().and_then(speed), speed(&frame)),
            (Some(old), Some(new)) if old != new
        );
        if frame.mtw.is_none() || preset_changed {
            self.ir_arrival = None;
        }
        self.frame = Some(frame);
        self.frames_in += 1;
    }

    /// Hold an IR sidecar frame in the place its `span` names (#706).
    fn hold_ir(&mut self, frame: ac_core::wire::IrFrame) {
        if frame.span.as_deref() == Some(ac_core::wire::IR_SPAN_ARRIVAL) {
            self.frames_since_arrival = 0;
            self.ir_arrival = Some(frame);
        } else {
            self.ir = Some(frame);
        }
    }
}

/// New frames of a pair after which a `Y` press is no longer pending: the
/// frames then say what the daemon holds. The daemon applies a queued
/// `set_delay` at the start of its next 50 ms tick, so the second frame
/// after a press already reflects it; five leaves room for frames already
/// in flight. Assumed, not measured. Counting frames rather than matching
/// values: a stored frame, or one in flight from before the press, can
/// match the pending value by accident (Codex recheck).
const TRACK_PENDING_FRAMES: u64 = 5;

/// The version-mismatch state's cross-frame record (#112).
struct VersionRefusal {
    /// The most recent refused frame's version.
    error: ac_core::wire::WireVersionError,
    /// DATA frames refused since the state began. Its rise is what says the
    /// daemon is still publishing and only the version is wrong.
    refused: u64,
}

/// `daemon sends wire v2, ac-view reads v1` — the two versions side by side,
/// which says which build is older without the text asserting a cause.
fn version_detail(error: &ac_core::wire::WireVersionError) -> String {
    format!(
        "daemon sends wire {}, ac-view reads {}",
        error.found_label(),
        ac_core::wire::WireVersionError::supported_label()
    )
}

/// The help overlay (`/`): the view's keys in labelled sections, each an
/// aligned key / action grid, split over two columns so the whole table
/// usually fits; a short window scrolls it. Every string is [`crate::keys::help_sections`]'s.
fn draw_help(ctx: &egui::Context, view: crate::keys::ViewId) {
    let sections = crate::keys::help_sections(view);
    // Split where the running row count passes half, so the columns are
    // about the same height; a section is never split.
    let total: usize = sections.iter().map(|(_, rows)| rows.len() + 2).sum();
    let mut acc = 0;
    let split = sections
        .iter()
        .position(|(_, rows)| {
            acc += rows.len() + 2;
            acc * 2 >= total
        })
        .map_or(sections.len(), |i| i + 1);
    let title = match view {
        crate::keys::ViewId::Transfer => "keys \u{2014} transfer view",
        crate::keys::ViewId::Spectrum => "keys \u{2014} spectrum view",
    };
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            // Scrolls when the window is shorter than the table (the
            // transfer view's left column alone is ~600 pt; the app opens at
            // 800×600 — Codex review).
            let max_h = (ctx.content_rect().height() - 90.0).max(120.0);
            egui::ScrollArea::vertical()
                .max_height(max_h)
                .show(ui, |ui| {
                    ui.columns(2, |cols| {
                        for (col, part) in cols
                            .iter_mut()
                            .zip([&sections[..split], &sections[split..]])
                        {
                            for (group, rows) in part {
                                col.label(egui::RichText::new(group.title()).strong());
                                egui::Grid::new(("help", group.title()))
                                    .num_columns(2)
                                    .spacing([14.0, 3.0])
                                    .show(col, |g| {
                                        for r in rows {
                                            g.label(
                                                egui::RichText::new(&r.keys)
                                                    .monospace()
                                                    .color(crate::view::palette::COLOR_VALUE),
                                            );
                                            g.label(r.text);
                                            g.end_row();
                                        }
                                    });
                                col.add_space(8.0);
                            }
                        }
                    });
                });
            ui.separator();
            ui.label("/ closes this help");
        });
}

/// Pull frames from `poll` until it reports empty, in arrival order, each
/// tagged `PolledFrame` (#286) — a `transfer_stream` frame and its
/// `visualize/ir` sidecar are independent JSON objects and go to
/// independent ingest paths, but in the order they came (#706). The socket read is passed in so the drain test can inject a
/// queue instead (#219 Part B); `drain_frames` passes `Session::poll_frame`.
fn collect_drained(poll: impl FnMut() -> Option<PolledFrame>) -> Vec<PolledFrame> {
    std::iter::from_fn(poll).collect()
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
