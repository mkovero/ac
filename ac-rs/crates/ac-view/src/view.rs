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
    stored: &[(&str, &ac_scene::TransferScene, bool)],
    ir: Option<&ac_scene::IrScene>,
) {
    // Reserve the half line the top y-axis tick label hangs into (#245).
    // Every pane's tick labels are drawn vertically centred on their
    // gridline, so the topmost one — the tick sitting exactly on the pane's
    // top edge — puts half its glyph height above the rect the view was
    // given. egui's item spacing is narrower than that, so the shell's
    // connection banner on the row above ended up struck through by the
    // `20` of the +20 dB tick. Taking the space here, before either view
    // reads `available_rect_before_wrap`, keeps the top edge clear without
    // the panes needing to know why.
    //
    // It reserves the top only. The frequency labels are drawn at
    // `rect.max.y` with `Align2::CENTER_TOP`, so a full line still hangs
    // below the rect; nothing is drawn under a view today, so it overlaps
    // nothing. Anything stacked below one needs the same reserve at the
    // bottom.
    let tick_line_h = ui
        .painter()
        .layout_no_wrap("0".to_string(), egui::FontId::default(), COLOR_LABEL)
        .size()
        .y;
    ui.add_space(tick_line_h / 2.0);
    match kind {
        ViewKind::Spectrum(state) => draw_spectrum(state, ui, scene),
        ViewKind::Transfer(state) => draw_transfer(state, ui, transfer, stored, ir),
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
/// clamp) is the [`crate::stimulus::StimulusMachine`], which this state
/// owns as of M4c. The banner reads the machine's state and level.
pub use crate::stimulus::StimState;

/// One stored run loaded for comparison (#321) — a snapshot's H1
/// derivation plus enough identity to attribute an on-screen trace to the
/// file it came from, and its own [`ac_scene::Smoothing`] so two overlaid
/// runs can be smoothed differently. Built once by
/// [`crate::snapshot_flow::open_stored_transfer_run`]; its
/// [`ac_scene::TransferScene`] is rebuilt from `derivation` every pass
/// (never mutated in place), the same "rebuild from held state" discipline
/// `last_frame` → `transfer_scene` already follows for zoom/pan.
///
/// Always drawn self-compensated (`DerotMode::Session`, τ_derot 0) — a
/// stored run has no notion of "this session's" delay to de-rotate
/// against, and `transfer.rs`'s own module doc states this is the correct
/// reading for a stored capture. There is deliberately no per-run derot
/// field: only the live trace's phase reference is a choice the operator
/// makes.
pub struct LoadedRun {
    /// Attribution (acceptance criterion 1) — the file's own name, never
    /// a friendlier fabricated one; the operator can go check it on disk.
    pub label: String,
    /// RFC3339 UTC capture instant, straight from `SnapshotMeta` —
    /// disambiguates two runs against the same DUT captured minutes
    /// apart, where the filename alone might not.
    pub captured_at_utc: String,
    pub derivation: ac_core::visualize::pair_derivation::PairDerivation,
    pub channel_role: String,
    pub sr: u32,
    /// This run's own fractional-octave smoothing (acceptance criterion
    /// 2) — starts off, cycled by `N` while this run has focus. Same
    /// non-persistence reasoning as the live view's `smoothing` field:
    /// every load opens at the honest, unsmoothed default.
    pub smoothing: ac_scene::Smoothing,
}

impl LoadedRun {
    pub fn new(
        label: String,
        captured_at_utc: String,
        derivation: ac_core::visualize::pair_derivation::PairDerivation,
        channel_role: String,
        sr: u32,
    ) -> Self {
        Self {
            label,
            captured_at_utc,
            derivation,
            channel_role,
            sr,
            smoothing: ac_scene::Smoothing::Off,
        }
    }
}

/// Which trace a single-owner readout (delay, per-run smoothing edits)
/// currently names (#321) — the live trace, or one of `loaded`'s stored
/// runs by index. `Tab` cycles it; it is the one piece of state that
/// decides both which trace `N` edits and which trace the delay readout
/// describes, so neither is ever ambiguous about which curve it belongs
/// to (acceptance criterion 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Live,
    Stored(usize),
}

pub struct TransferViewState {
    pub freq_range: FreqRange,
    pub derot: DerotChoice,
    /// `P` toggles raw phase; remembers the previous choice so a second
    /// press restores it rather than landing on Session unconditionally.
    prev_derot: DerotChoice,
    /// The full drive-path safety machine (M4c) — arm/fire/stop,
    /// auto-disarm, clamp, keepalive. The app drives it and sends the
    /// [`crate::stimulus::DriveCmd`]s it emits.
    pub stimulus: crate::stimulus::StimulusMachine,
    /// The open snapshot's stored delay (ms), fed to `DerotChoice::Snapshot`.
    /// M4c wires the open-snapshot flow; until then it stays 0.
    pub snapshot_delay_ms: f64,
    /// Fractional-octave smoothing of the trace (#229), cycled by `N`.
    ///
    /// Starts off, and is deliberately **not persisted**. A setting that
    /// survives a restart is a setting someone forgets is on: they measure
    /// next week, screenshot it, and the caption is the only thing standing
    /// between that screenshot and a resolution claim the data does not
    /// support. Non-persistence means every session opens at the honest
    /// default. The cost is one keypress, on a control the operator changes
    /// while looking at the screen anyway.
    pub smoothing: ac_scene::Smoothing,
    /// IR panel visibility (#286), toggled by `H`. Off by default — the
    /// mag/phase panes are the resting state of the transfer view; the
    /// panel is an on-demand accessory, not a third pane always fighting
    /// the other two for the same screen.
    ir_panel_open: bool,
    /// Stored runs loaded for comparison (#321), in load order. Unbounded
    /// on purpose — triage and the architect both left the comparison-set
    /// size open as a later UX-driven constraint, not an architectural one.
    pub loaded: Vec<LoadedRun>,
    /// Which trace `N` (smoothing) and the delay readout currently name.
    /// Starts on `Live` — a session that has loaded nothing yet reads
    /// exactly as it did before this issue.
    pub focus: Focus,
}

impl Default for TransferViewState {
    fn default() -> Self {
        // Fallback ceiling/start; the app rebuilds with config's
        // `drive_max_dbfs` via [`TransferViewState::new`].
        Self::new(-10.0, -30.0)
    }
}

impl TransferViewState {
    pub fn new(drive_max_dbfs: f64, start_level_dbfs: f64) -> Self {
        Self {
            freq_range: FreqRange::default(),
            derot: DerotChoice::Session,
            prev_derot: DerotChoice::Session,
            stimulus: crate::stimulus::StimulusMachine::new(drive_max_dbfs, start_level_dbfs),
            snapshot_delay_ms: 0.0,
            smoothing: ac_scene::Smoothing::Off,
            ir_panel_open: false,
            loaded: Vec::new(),
            focus: Focus::Live,
        }
    }

    pub fn cycle_derot(&mut self) {
        self.derot = self.derot.next();
    }

    /// `N`: cycle the *focused* trace's smoothing (#321) — the live
    /// trace's `smoothing` field if focus is `Live`, or the focused
    /// `LoadedRun`'s own field otherwise. The order and the labels are
    /// `ac-scene`'s — this crate holds the choice, it does not define
    /// what any setting means. Editing one trace's field never touches
    /// another's (acceptance criterion 4) — each lives in its own struct.
    pub fn cycle_smoothing(&mut self) {
        match self.focus {
            Focus::Live => self.smoothing = self.smoothing.next(),
            Focus::Stored(idx) => {
                if let Some(run) = self.loaded.get_mut(idx) {
                    run.smoothing = run.smoothing.next();
                }
            }
        }
    }

    /// Add a newly opened stored run and move focus onto it — the
    /// operator just opened it, so it is naturally what `N` and the delay
    /// readout should name next, the same "just arrived" precedence the
    /// live view already gives the newest frame.
    pub fn add_loaded_run(&mut self, run: LoadedRun) {
        self.loaded.push(run);
        self.focus = Focus::Stored(self.loaded.len() - 1);
    }

    /// `Tab`: move focus to the next trace — live, then each stored run
    /// in load order, wrapping back to live. A no-op (`Live` stays
    /// `Live`) when nothing is loaded.
    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Live if self.loaded.is_empty() => Focus::Live,
            Focus::Live => Focus::Stored(0),
            Focus::Stored(idx) if idx + 1 < self.loaded.len() => Focus::Stored(idx + 1),
            Focus::Stored(_) => Focus::Live,
        };
    }

    /// `X`: close the focused stored run. A no-op when focus is `Live` —
    /// the live trace is not something the operator "closes". Focus
    /// moves to whichever run slides into the closed index, or back to
    /// `Live` if that was the last one.
    pub fn close_focused_stored_run(&mut self) {
        if let Focus::Stored(idx) = self.focus {
            if idx < self.loaded.len() {
                self.loaded.remove(idx);
            }
            self.focus = if idx < self.loaded.len() {
                Focus::Stored(idx)
            } else {
                Focus::Live
            };
        }
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

    /// The live trace's de-rotation reference. `DerotChoice::Snapshot`
    /// can only carry one delay at a time — it tracks whichever stored
    /// run currently has focus (#321; the architect's brief names this a
    /// consequence of `DerotMode`'s existing shape, not a new field), so
    /// changing focus among stored runs changes what the live phase is
    /// compared against. Falls back to `snapshot_delay_ms` (0.0 until a
    /// run is loaded, or while focus is `Live`) so behaviour is unchanged
    /// for a session with nothing loaded.
    pub fn derot_mode(&self) -> ac_scene::DerotMode {
        let snapshot_delay_ms = match self.focus {
            Focus::Stored(idx) => self
                .loaded
                .get(idx)
                .map(|run| ac_scene::TransferInput::stored_delay_ms(&run.derivation))
                .unwrap_or(self.snapshot_delay_ms),
            Focus::Live => self.snapshot_delay_ms,
        };
        self.derot.to_mode(snapshot_delay_ms)
    }

    /// `H`: toggle the IR panel.
    pub fn toggle_ir_panel(&mut self) {
        self.ir_panel_open = !self.ir_panel_open;
    }

    pub fn ir_panel_open(&self) -> bool {
        self.ir_panel_open
    }
}

/// Transfer view (M4b): magnitude pane stacked over phase pane, shared
/// log-f axis, gap rendering for masked columns, delay readout, input
/// meters, and the stimulus banner — every string and coordinate from
/// `ac-scene`, drawn verbatim.
fn draw_transfer(
    state: &TransferViewState,
    ui: &mut Ui,
    scene: Option<&ac_scene::TransferScene>,
    stored: &[(&str, &ac_scene::TransferScene, bool)],
    ir: Option<&ac_scene::IrScene>,
) {
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
    let level = state.stimulus.level_dbfs();
    let banner = match state.stimulus.state() {
        StimState::Idle => None,
        StimState::Armed => Some((
            ac_scene::readout::format_armed_banner(0, None, level),
            18.0,
            COLOR_VALUE,
        )),
        StimState::Driving => Some((
            ac_scene::readout::format_driving_banner(0, None, level),
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

    // The IR panel (`H`, #286) replaces the mag/phase panes rather than
    // sharing the content band with them — it is an on-demand accessory
    // view of the same session, not a third pane the other two must make
    // room for. Input meters stay: gain staging is still relevant while
    // looking at h(t). The fault indicator does not — it is drawn
    // relative to the magnitude pane's geometry, which does not exist in
    // this branch.
    if state.ir_panel_open() {
        match ir {
            Some(ir_scene) => draw_ir_panel(painter, content, ir_scene),
            None => {
                painter.text(
                    content.center(),
                    egui::Align2::CENTER_CENTER,
                    "no IR frame yet",
                    egui::FontId::default(),
                    COLOR_STRUCTURAL,
                );
            }
        }
        draw_meter(painter, content, 0, &scene.ref_meter, "R");
        draw_meter(painter, content, 1, &scene.meas_meter, "M");
        return;
    }

    // Comparison legend (#321): one row per stored run, below the panes.
    // Reserved only when a stored run is loaded — an operator who has
    // opened nothing sees exactly the layout this issue found (zero
    // height cost for the common case). `ROW_H` is declared below the
    // panes historically; declared here too since the legend band needs
    // it before the panes are sized.
    const ROW_H: f32 = 16.0;
    let legend_band = if stored.is_empty() {
        0.0
    } else {
        (stored.len() as f32 + 1.0) * ROW_H + 4.0
    };

    // Two stacked panes: magnitude (top), phase (bottom). Shared x
    // (log-f); each pane maps its own normalized y over its own band.
    let gap = 8.0;
    let pane_h = (content.height() - gap - legend_band) / 2.0;
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

    // Axis context (#194): gridlines + labels behind the traces, drawn
    // from ac-scene ticks verbatim — positions are ac-scene's normalized
    // coordinates, this crate only maps them (no tick math here). dB grid
    // on the magnitude pane, ±180° grid on the phase pane (with the 0°
    // reference), shared freq labels along the bottom.
    draw_pane_grid(painter, &scene.mag_axis, mag_vp);
    draw_pane_grid(painter, &scene.phase_axis, phase_vp);
    draw_freq_labels(painter, &scene.freq_axis, phase_vp);

    // Trace weight/colour is focus, not identity (#321 UX ruling: the
    // palette has one signal hue on purpose, so N traces cannot each get
    // their own colour). The focused trace — live or one stored run —
    // draws full weight in the signal colour; every other trace recedes
    // to structural grey, dashed for a stored run's existing
    // live-vs-stored provenance convention (unchanged from before this
    // trace could ever collide with another).
    let live_focused = matches!(state.focus, Focus::Live);
    let live_stroke = if live_focused {
        Stroke::new(1.5, COLOR_SIGNAL)
    } else {
        Stroke::new(1.0, COLOR_STRUCTURAL)
    };
    draw_segments(painter, &scene.magnitude, mag_vp, live_stroke, false);
    draw_segments(painter, &scene.phase, phase_vp, live_stroke, false);

    // Stored runs (#321): dashed overlay, drawn after the live trace so a
    // focused stored run's full-weight curve is not occluded by it.
    for (_, stored_scene, focused) in stored {
        let stroke = if *focused {
            Stroke::new(1.5, COLOR_SIGNAL)
        } else {
            Stroke::new(1.0, COLOR_STRUCTURAL)
        };
        draw_segments(painter, &stored_scene.magnitude, mag_vp, stroke, true);
        draw_segments(painter, &stored_scene.phase, phase_vp, stroke, true);
    }

    // The top of the magnitude pane carries three rows, in this order and
    // for this reason (#224 + #229, ruled on when the two were reviewed as
    // a pair):
    //
    //   row 0   band labels        what the ANALYSER resolved, per rung
    //   row 1   smoothing caption  what is actually ON SCREEN
    //   row 2   delay readout      a measured value
    //
    // Rows 0 and 1 are both statements about resolution and are adjacent
    // with nothing between them, so the pane reads as one statement rather
    // than two competing ones. The caption is authoritative for the drawn
    // trace: at 1/1 octave the curve is smoothed far wider than any band's
    // Δf, and a screenshot showing "0.98 Hz" over it would overclaim.
    //
    // The delay readout is on row 2 rather than sharing row 0, which is not
    // cosmetic: at a 3-digit delay its laid-out width runs past the deepest
    // band label's left edge, and the two overlap. Moving it down removes
    // the collision outright, with no width arbitration to re-verify when
    // the ladder or the font changes. Anchoring the caption *on* row 0 was
    // rejected for the same class of reason — the only gaps wide enough sit
    // between band labels, and those move with sample rate and zoom.
    // (`ROW_H` is declared above, before the panes are sized — the legend
    // band needs it first.)

    // Positions and strings are verbatim ac-scene; this crate maps the
    // normalized x and draws, as with every other label. Structural grey
    // with no rule or box: findable when sought, invisible when not.
    for band in &scene.band_labels {
        let (x, _) = scene_to_screen((band.position, 0.0), mag_vp);
        painter.text(
            egui::pos2(x, mag_vp.y),
            egui::Align2::CENTER_TOP,
            &band.text,
            egui::FontId::default(),
            COLOR_STRUCTURAL,
        );
    }

    // Absent when smoothing is off — an unaltered trace is the resting
    // state and needs no caption. The string is ac-scene's.
    if let Some(label) = scene.smoothing_readout {
        painter.text(
            content.left_top() + egui::vec2(0.0, ROW_H),
            egui::Align2::LEFT_TOP,
            label,
            egui::FontId::default(),
            COLOR_LABEL,
        );
    }

    // Delay readout. With nothing loaded this is `scene.delay_readout`
    // verbatim, unchanged from before this trace could collide with
    // another. Once a stored run exists, the value switches to whichever
    // trace is focused and gains an owner tag (acceptance criterion 5:
    // no readout naming a single measurement may leave its owner
    // ambiguous) — `delay (<owner>)` is this crate's own chrome text, not
    // a reformatted measurement, so it does not cross the `computes_nothing`
    // boundary; the number after it is still ac-scene's string, untouched.
    let (focused_label, focused_delay): (&str, &str) = match state.focus {
        Focus::Live => ("live", scene.delay_readout.as_str()),
        Focus::Stored(idx) => stored
            .get(idx)
            .map(|(label, s, _)| (*label, s.delay_readout.as_str()))
            .unwrap_or(("live", scene.delay_readout.as_str())),
    };
    let delay_text = if stored.is_empty() {
        scene.delay_readout.clone()
    } else {
        format!("delay ({focused_label})  {focused_delay}")
    };
    painter.text(
        content.left_top() + egui::vec2(0.0, 2.0 * ROW_H),
        egui::Align2::LEFT_TOP,
        delay_text,
        egui::FontId::default(),
        COLOR_VALUE,
    );

    // Comparison legend (#321): one row for the live trace, one per
    // stored run, in the band reserved above the panes. Filename +
    // timestamp (attribution, criterion 1) and that run's own smoothing
    // caption (criterion 2) — verbatim `ac-scene` string, blank when that
    // run is unsmoothed, the same "absent means off" convention the
    // single-trace caption above already uses. `▸` marks focus, the one
    // signal that also selects what `N` edits and what the delay readout
    // above names.
    if !stored.is_empty() {
        let legend_top = content.max.y - legend_band;
        let live_marker = if live_focused { "▸ " } else { "  " };
        painter.text(
            egui::pos2(content.min.x, legend_top),
            egui::Align2::LEFT_TOP,
            format!("{live_marker}live"),
            egui::FontId::default(),
            if live_focused {
                COLOR_VALUE
            } else {
                COLOR_LABEL
            },
        );
        for (i, (label, stored_scene, focused)) in stored.iter().enumerate() {
            let marker = if *focused { "▸ " } else { "  " };
            let smoothing = stored_scene.smoothing_readout.unwrap_or("");
            painter.text(
                egui::pos2(content.min.x, legend_top + ROW_H * (i as f32 + 1.0)),
                egui::Align2::LEFT_TOP,
                format!("{marker}{label}  {smoothing}"),
                egui::FontId::default(),
                if *focused { COLOR_VALUE } else { COLOR_LABEL },
            );
        }
    }

    // Input-level meters: two thin bars at the right edge, M left of R
    // (UX standing requirement), heights normalized by ac-scene. Always
    // on (D6) — no toggle. `idx` counts from the right edge, so R (idx 0)
    // is rightmost and M (idx 1) sits to its left.
    draw_meter(painter, content, 0, &scene.ref_meter, "R");
    draw_meter(painter, content, 1, &scene.meas_meter, "M");

    // Fault indicator (#228), last so it is over the traces rather than
    // under them. Centred on the magnitude pane: the delay readout owns
    // the content band's top-left corner, the meters own the right edge,
    // and the stimulus banner owns its own reserved band above all of
    // this, so nothing collides. Both strings are verbatim ac-scene.
    if let Some(fault) = scene.fault {
        let color = match fault.severity() {
            // Never green, and never the trace colour: a fault must not
            // read as measurement.
            ac_scene::Severity::Fault => COLOR_SIGNAL,
            ac_scene::Severity::Confirmation => COLOR_VALUE,
        };
        let centre = egui::pos2(
            mag_vp.x + mag_vp.width / 2.0,
            mag_vp.y + mag_vp.height / 2.0,
        );
        painter.text(
            centre,
            egui::Align2::CENTER_CENTER,
            fault.label(),
            egui::FontId::proportional(28.0),
            color,
        );
        if let Some(detail) = fault.detail() {
            painter.text(
                egui::pos2(centre.x, centre.y + 22.0),
                egui::Align2::CENTER_CENTER,
                detail,
                egui::FontId::proportional(14.0),
                COLOR_LABEL,
            );
        }
    }
}

/// Draw a pane's horizontal gridlines + left-edge labels from an
/// `ac-scene` axis (#194). Positions and label strings are verbatim from
/// the scene — this crate maps the normalized y to screen and draws,
/// nothing more (no tick math, no formatting: `computes_nothing` holds).
fn draw_pane_grid(painter: &egui::Painter, axis: &ac_scene::Axis, vp: Viewport) {
    for tick in &axis.ticks {
        let (_, y) = scene_to_screen((0.0, tick.position), vp);
        painter.line_segment(
            [egui::pos2(vp.x, y), egui::pos2(vp.x + vp.width, y)],
            Stroke::new(0.5, COLOR_STRUCTURAL),
        );
        painter.text(
            egui::pos2(vp.x + 2.0, y),
            egui::Align2::LEFT_CENTER,
            &tick.label,
            egui::FontId::default(),
            COLOR_LABEL,
        );
    }
}

/// Shared log-frequency labels along the bottom of a pane (#194) — verbatim
/// from the scene's freq axis.
fn draw_freq_labels(painter: &egui::Painter, axis: &ac_scene::Axis, vp: Viewport) {
    for tick in &axis.ticks {
        let (x, _) = scene_to_screen((tick.position, 0.0), vp);
        painter.text(
            egui::pos2(x, vp.y + vp.height),
            egui::Align2::CENTER_BOTTOM,
            &tick.label,
            egui::FontId::default(),
            COLOR_LABEL,
        );
    }
}

/// Draw one trace's segments in a pane — one polyline per segment, so a
/// coherence gap is the absence of a segment (D5), never a line to the
/// floor. This crate decides nothing about where a gap goes; it stops
/// drawing where ac-scene stopped emitting. `dashed` distinguishes a
/// stored run's provenance (#321) from the live trace, the same
/// live-solid/snapshot-dashed convention `draw_spectrum` already draws.
fn draw_segments(
    painter: &egui::Painter,
    trace: &ac_scene::Trace,
    vp: Viewport,
    stroke: Stroke,
    dashed: bool,
) {
    for segment in &trace.segments {
        let pts: Vec<egui::Pos2> = segment
            .iter()
            .map(|&pt| {
                let (x, y) = scene_to_screen(pt, vp);
                egui::pos2(x, y)
            })
            .collect();
        if pts.len() > 1 {
            if dashed {
                let mut shapes = Vec::new();
                egui::Shape::dashed_line_many(&pts, stroke, 6.0, 4.0, &mut shapes);
                painter.extend(shapes);
            } else {
                painter.add(egui::Shape::line(pts, stroke));
            }
        }
    }
}

/// The IR panel (#286): header, h(t) trace, time axis, and the arrival
/// marker — every string and coordinate `ac-scene`'s, this crate only
/// maps the normalized `[0,1]²` points into `rect` and draws (the same
/// contract every other pane in this file holds).
fn draw_ir_panel(painter: &egui::Painter, rect: egui::Rect, scene: &ac_scene::IrScene) {
    draw_ir_header(painter, rect, scene.header);

    if scene.trace.segments.is_empty() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "no samples yet",
            egui::FontId::default(),
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
/// Not yet reachable from [`draw_view`]'s dispatch — the file-open UI
/// that would call it is #256's picker, sequenced after this issue (the
/// architect review's explicit call). This function is the orchestration
/// side of that: implemented and tested now, wired once the picker
/// exists — same pattern `Action::OpenSnapshot` already documents for
/// `.acsnap`.
pub fn draw_sweep_ir_panel(
    painter: &egui::Painter,
    rect: egui::Rect,
    result: Result<&ac_scene::SweepIrScene, ac_scene::SweepIrFault>,
) {
    match result {
        Ok(scene) => {
            draw_ir_header(painter, rect, &scene.header);
            if scene.trace.segments.is_empty() {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "no samples in gate window",
                    egui::FontId::default(),
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
            draw_ir_header(painter, rect, fault.header());
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                fault.detail(),
                egui::FontId::default(),
                COLOR_STRUCTURAL,
            );
        }
    }
}

/// The header line both IR panel variants draw at `rect`'s top-left —
/// verbatim `ac-scene` string, this crate only positions it.
fn draw_ir_header(painter: &egui::Painter, rect: egui::Rect, header: &str) {
    painter.text(
        rect.left_top(),
        egui::Align2::LEFT_TOP,
        header,
        egui::FontId::default(),
        COLOR_LABEL,
    );
}

/// The part both IR panel variants share once there's a non-empty trace
/// to draw: h(t), the time axis, and the arrival marker — every string
/// and coordinate `ac-scene`'s, this crate only maps the normalized
/// `[0,1]²` points into `rect`.
fn draw_ir_trace_and_arrival(
    painter: &egui::Painter,
    rect: egui::Rect,
    trace: &ac_scene::Trace,
    time_axis: &ac_scene::Axis,
    arrival: &ac_scene::ArrivalMarker,
) {
    let vp = Viewport {
        x: rect.min.x,
        y: rect.min.y,
        width: rect.width(),
        height: rect.height(),
    };

    // h(t): one polyline per segment, same drawing rule `draw_segments`
    // uses for the mag/phase traces — there is always exactly one
    // segment here (no coherence mask over a time-domain sample), but
    // the loop stays generic rather than assuming that shape.
    for segment in &trace.segments {
        let points: Vec<egui::Pos2> = segment
            .iter()
            .map(|&pt| {
                let (x, y) = scene_to_screen(pt, vp);
                egui::pos2(x, y)
            })
            .collect();
        match trace.provenance.source {
            ac_scene::Source::Live => {
                painter.add(egui::Shape::line(points, Stroke::new(1.5, COLOR_SIGNAL)));
            }
            ac_scene::Source::Snapshot => {
                let mut shapes = Vec::new();
                egui::Shape::dashed_line_many(
                    &points,
                    Stroke::new(1.5, COLOR_SIGNAL),
                    6.0,
                    4.0,
                    &mut shapes,
                );
                painter.extend(shapes);
            }
        }
    }

    // Time axis: bottom labels, verbatim ac-scene ticks — same mapping
    // `draw_freq_labels` uses for the mag/phase panes' shared frequency
    // axis.
    for tick in &time_axis.ticks {
        let (x, _) = scene_to_screen((tick.position, 0.0), vp);
        painter.text(
            egui::pos2(x, rect.max.y),
            egui::Align2::CENTER_BOTTOM,
            &tick.label,
            egui::FontId::default(),
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
    painter.text(
        egui::pos2(ax, bottom + 2.0),
        egui::Align2::CENTER_TOP,
        &arrival.text,
        egui::FontId::default(),
        COLOR_VALUE,
    );
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
    fn ir_panel_starts_closed_and_toggles() {
        let mut t = TransferViewState::default();
        assert!(!t.ir_panel_open());
        t.toggle_ir_panel();
        assert!(t.ir_panel_open());
        t.toggle_ir_panel();
        assert!(!t.ir_panel_open());
    }

    // Stimulus transitions, level steps, arm-expiry, clamp, and keepalive
    // are the StimulusMachine's contract now (M4c) — exhaustively tested
    // in `stimulus.rs`, not duplicated here against a placeholder.
}
