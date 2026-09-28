//! View state: what the operator has chosen, held between frames.
//!
//! Deliberately free of any drawing: these types are the keyboard
//! layer's target (`keys.rs` maps a keypress to a mutation here) and the
//! scene layer's input (`app.rs` reads them when it asks `ac-scene` for
//! the next scene). Nothing in this file paints, and nothing in it
//! computes a measurement — the settings it holds *name* `ac-scene`
//! modes, they never define what one means.

use crate::range::{DbRange, FreqRange};

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
/// Drawn against its own recorded delay (`DerotMode::Session`, τ_derot 0)
/// — a stored run has no notion of "this session's" delay to de-rotate
/// against — or raw ([`LoadedRun::raw_phase`]). Operator, 2026-09-28: a
/// slot keeps the raw/de-rotated choice live had when it was stored, and
/// `P`/`R` switch it with focus on the slot; live's third choice (the
/// selected slot's delay) is a statement about live and has no stored
/// equivalent.
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
    /// Drawn inverted (`U`) and offset in dB (`J`) — this run's own, like
    /// its smoothing. Display only.
    pub invert: bool,
    pub offset_db: f64,
    /// Phase drawn raw (measured) rather than de-rotated by the run's own
    /// delay — this run's own, like its smoothing: set from live's at
    /// store, `P`/`R` with focus on it switch it.
    pub raw_phase: bool,
    /// Drawn or hidden (#256, `V`). Hidden runs keep their legend row, so
    /// nothing leaves the comparison without the operator removing it.
    pub visible: bool,
    /// Which comparison colour this run draws in — assigned once when the
    /// run is added ([`TransferViewState::add_run`]), so removing one run
    /// never recolours the others mid-comparison.
    pub color_slot: usize,
    /// The slot this run was stored to with `Ctrl`+digit (#256), 1–9;
    /// `None` for a run opened from a file.
    pub slot: Option<u8>,
    /// Samples added to the recorded delay by `←`/`→` (#256), applied as a
    /// phase rotation when the scene is built.
    pub delay_offset_samples: i64,
    /// The stored run's impulse response — the same IFFT of its H1 the
    /// live sidecar is, computed when the run is derived (off the UI
    /// thread). `None` only for runs built without a snapshot (tests).
    pub ir: Option<ac_scene::IrInput>,
}

/// The live trace's display settings at the moment a slot is stored: the
/// slot is drawn the way live was drawn when the operator stored it, and
/// keeps its own copy from then on. Saved beside the capture
/// ([`crate::capture::settings_path`]) so `F` reopens it the same way.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SlotSettings {
    pub smoothing: ac_scene::Smoothing,
    pub invert: bool,
    pub offset_db: f64,
    /// Live was showing raw phase (`P`/`R`) when the slot was stored.
    pub raw_phase: bool,
}

impl SlotSettings {
    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "smoothing_bpo": self.smoothing.bpo(),
            "invert": self.invert,
            "offset_db": self.offset_db,
            "raw_phase": self.raw_phase,
        })
    }

    /// `None` for anything that is not a settings file this client wrote:
    /// the run then opens at the defaults, as a file stored before #702 does.
    pub fn from_json(v: &serde_json::Value) -> Option<SlotSettings> {
        let smoothing = match v.get("smoothing_bpo")? {
            serde_json::Value::Null => ac_scene::Smoothing::Off,
            bpo => {
                let bpo = bpo.as_u64()?;
                let mut s = ac_scene::Smoothing::Off.next();
                while s.bpo() != Some(bpo as u32) {
                    s = s.next();
                    if s == ac_scene::Smoothing::Off {
                        return None;
                    }
                }
                s
            }
        };
        Some(SlotSettings {
            smoothing,
            invert: v.get("invert")?.as_bool()?,
            offset_db: v.get("offset_db")?.as_f64().filter(|o| o.is_finite())?,
            // Absent from files written before it was stored: de-rotated,
            // which is what those slots were drawn with.
            raw_phase: match v.get("raw_phase") {
                None => false,
                Some(b) => b.as_bool()?,
            },
        })
    }

    pub fn apply(self, run: &mut LoadedRun) {
        run.smoothing = self.smoothing;
        run.invert = self.invert;
        run.offset_db = self.offset_db;
        run.raw_phase = self.raw_phase;
    }
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
            invert: false,
            offset_db: 0.0,
            raw_phase: false,
            visible: true,
            color_slot: 0,
            slot: None,
            delay_offset_samples: 0,
            ir: None,
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
    /// The live traces drawn inverted (`U`) and offset (`J`), shared by
    /// every live pair like `smoothing`. Not persisted, for the same
    /// reason: every session opens showing what it measures.
    pub invert: bool,
    pub offset_db: f64,
    /// IR panel visibility (#286), toggled by `H`. Off by default — the
    /// mag/phase panes are the resting state of the transfer view; the
    /// panel is an on-demand accessory, not a third pane always fighting
    /// the other two for the same screen.
    ir_panel_open: bool,
    /// Which view the IR panel draws: linear, log or ETC (`Shift+H`).
    pub ir_view: ac_scene::IrView,
    /// The live IR panel shows the arrival IR (#706, the default) rather
    /// than the 1 s one; `S` switches.
    pub ir_arrival: bool,
    /// The live trace eases toward each new estimate over 40 ms (#716,
    /// `ac_scene::tween`) rather than stepping to it; `Shift+W` switches.
    pub tween: bool,
    /// The pinned cursor's frequency, Hz (#718): a click on the panes pins
    /// it, a right-click clears it. Held in Hz so zoom and pan keep it on
    /// its frequency.
    pub cursor_pin: Option<f64>,
    /// The IR panel's pinned cursor, ms (#720).
    pub ir_cursor_pin: Option<f64>,
    /// What the phase pane shows (#695), for every trace: wrapped phase,
    /// unwrapped phase or group delay (`Shift+P`).
    pub phase_view: ac_scene::transfer::PhaseView,
    /// Stored runs loaded for comparison (#321), in load order. Unbounded
    /// on purpose — triage and the architect both left the comparison-set
    /// size open as a later UX-driven constraint, not an architectural one.
    pub loaded: Vec<LoadedRun>,
    /// Which trace `N` (smoothing) and the delay readout currently name.
    /// Starts on `Live` — a session that has loaded nothing yet reads
    /// exactly as it did before this issue.
    pub focus: Focus,
    /// How many live pairs the session measures (#685); at least 1.
    live_count: usize,
    /// Which live pair is selected (#685), in launch order. It is what
    /// `Focus::Live` names, and it stays put while a slot is selected, so
    /// the meters and the fault indicator keep following one pair.
    pub live_pair: usize,
    /// Enter while not armed (#256): the live trace is not drawn, so the
    /// slots are compared on their own. Everything else keeps rolling —
    /// meters, fault indicator, readouts, stimulus — and storing to a slot
    /// waits until live is resumed.
    pub paused: bool,
    /// Whether the live trace is drawn (`V` with focus on live, #256) —
    /// hide it to compare stored runs alone.
    pub live_visible: bool,
    /// Next [`LoadedRun::color_slot`].
    next_color_slot: usize,
    /// Columns below this coherence are not drawn (#670, `B`). Display
    /// only; the fault indicator keeps its own fixed threshold.
    pub coherence_mask: f64,
    /// The average of the visible stored runs (#671, `M`): `None` = off,
    /// `Some(weighted)` = on, coherence-weighted or plain (`Shift+M`).
    pub average: Option<bool>,
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
            invert: false,
            offset_db: 0.0,
            ir_panel_open: false,
            ir_view: ac_scene::IrView::Linear,
            ir_arrival: true,
            tween: true,
            cursor_pin: None,
            ir_cursor_pin: None,
            phase_view: ac_scene::transfer::PhaseView::Wrapped,
            loaded: Vec::new(),
            focus: Focus::Live,
            live_count: 1,
            live_pair: 0,
            paused: false,
            live_visible: true,
            // Slot runs take colours 0–8 by slot; runs opened from files
            // start after them.
            next_color_slot: 9,
            coherence_mask: ac_scene::transfer::COHERENCE_THRESHOLD,
            average: None,
        }
    }

    /// `M` (#671): show or hide the slot average — coherence-weighted
    /// when it comes on.
    pub fn toggle_average(&mut self) {
        self.average = match self.average {
            Some(_) => None,
            None => Some(true),
        };
    }

    /// `Shift+M` (#671): coherence weighting on or off for the average.
    /// Turns the average on (weighted) if it was off.
    pub fn toggle_average_weighting(&mut self) {
        self.average = Some(!self.average.unwrap_or(false));
    }

    /// `B` (#670): the next coherence mask step, wrapping.
    pub fn cycle_coherence_mask(&mut self) {
        let steps = ac_scene::transfer::COHERENCE_MASK_STEPS;
        let i = steps
            .iter()
            .position(|m| *m == self.coherence_mask)
            .map_or(0, |i| (i + 1) % steps.len());
        self.coherence_mask = steps[i];
    }

    /// Live's display settings now — what a slot stored now is drawn with.
    pub fn live_settings(&self) -> SlotSettings {
        SlotSettings {
            smoothing: self.smoothing,
            invert: self.invert,
            offset_db: self.offset_db,
            // A stored run is drawn raw or against its own delay; live's
            // snapshot-delay reference is about live, and stores as the
            // run's own.
            raw_phase: self.derot == DerotChoice::Raw,
        }
    }

    /// `Ctrl`+`n` (#256): put `run` in slot `n`, replacing what was there.
    /// `run` arrives with its settings already set (live's at the moment of
    /// storing, or the saved ones for a file opened with `F`); only the
    /// slot's visibility carries over from what it replaces.
    /// Slot runs sit in slot order ahead of runs opened from files, so
    /// `Tab` walks them 1, 2, … A slot keeps its colour across
    /// replacements. Focus stays on the trace it was on.
    pub fn store_slot(&mut self, n: u8, mut run: LoadedRun) {
        run.slot = Some(n);
        run.color_slot = usize::from(n.saturating_sub(1));
        if let Some(existing) = self.loaded.iter_mut().find(|r| r.slot == Some(n)) {
            run.visible = existing.visible;
            *existing = run;
            return;
        }
        let at = self
            .loaded
            .iter()
            .position(|r| r.slot.is_none_or(|s| s > n))
            .unwrap_or(self.loaded.len());
        self.loaded.insert(at, run);
        if let Focus::Stored(i) = self.focus {
            if i >= at {
                self.focus = Focus::Stored(i + 1);
            }
        }
    }

    /// Add a stored run to the comparison with the next colour (#256).
    pub fn add_run(&mut self, mut run: LoadedRun) {
        run.color_slot = self.next_color_slot;
        self.next_color_slot += 1;
        self.loaded.push(run);
    }

    /// Whether the live trace is drawn: shown (`V`) and not paused (#256).
    pub fn live_trace_shown(&self) -> bool {
        self.live_visible && !self.paused
    }

    /// Pause or resume the live display (#256, Enter).
    pub fn toggle_pause(&mut self) {
        self.paused = !self.paused;
    }

    /// `V`: show or hide the focused trace — the live one or a stored run
    /// (#256).
    pub fn toggle_focused_visibility(&mut self) {
        match self.focus {
            Focus::Live => self.live_visible = !self.live_visible,
            Focus::Stored(idx) => {
                if let Some(run) = self.loaded.get_mut(idx) {
                    run.visible = !run.visible;
                }
            }
        }
        self.settle_focus();
    }

    /// A bare digit (#256): show or hide slot `n`. `false` when the slot
    /// is empty — nothing to toggle.
    ///
    /// Recalling a slot selects it; hiding the selected slot hands the
    /// selection back to live (#256).
    pub fn toggle_slot_visibility(&mut self, n: u8) -> bool {
        let Some(i) = self.loaded.iter().position(|r| r.slot == Some(n)) else {
            return false;
        };
        let run = &mut self.loaded[i];
        run.visible = !run.visible;
        if run.visible {
            self.focus = Focus::Stored(i);
        } else if self.focus == Focus::Stored(i) {
            self.focus = Focus::Live;
        }
        true
    }

    /// Show and select slot `n`, if stored (#256: a slot loaded with `F`
    /// is recalled, whatever the slot's old visibility).
    pub fn select_slot(&mut self, n: u8) {
        if let Some(i) = self.loaded.iter().position(|r| r.slot == Some(n)) {
            self.loaded[i].visible = true;
            self.focus = Focus::Stored(i);
        }
    }

    /// `Shift+V`: show every trace (#256).
    pub fn show_all(&mut self) {
        self.live_visible = true;
        for run in &mut self.loaded {
            run.visible = true;
        }
    }

    ///
    /// With a stored run focused, `R` switches that run between raw and its
    /// own delay — the only two references a stored run has.
    pub fn cycle_derot(&mut self) {
        if let Focus::Stored(i) = self.focus {
            if let Some(run) = self.loaded.get_mut(i) {
                run.raw_phase = !run.raw_phase;
            }
            return;
        }
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

    /// `U`: invert the focused trace, or put it back.
    pub fn toggle_invert(&mut self) {
        match self.focus {
            Focus::Live => self.invert = !self.invert,
            Focus::Stored(idx) => {
                if let Some(run) = self.loaded.get_mut(idx) {
                    run.invert = !run.invert;
                }
            }
        }
    }

    /// `J` applied: the focused trace's offset, dB.
    pub fn set_offset(&mut self, offset_db: f64) {
        match self.focus {
            Focus::Live => self.offset_db = offset_db,
            Focus::Stored(idx) => {
                if let Some(run) = self.loaded.get_mut(idx) {
                    run.offset_db = offset_db;
                }
            }
        }
    }

    /// Add a newly opened stored run and move focus onto it — the
    /// operator just opened it, so it is naturally what `N` and the delay
    /// readout should name next, the same "just arrived" precedence the
    /// live view already gives the newest frame.
    pub fn add_loaded_run(&mut self, run: LoadedRun) {
        self.add_run(run);
        self.focus = Focus::Stored(self.loaded.len() - 1);
    }

    /// The number of live pairs (#685). Clamps the selection into range.
    pub fn set_live_count(&mut self, n: usize) {
        self.live_count = n.max(1);
        self.live_pair = self.live_pair.min(self.live_count - 1);
    }

    pub fn live_count(&self) -> usize {
        self.live_count
    }

    /// `Tab`: move focus to the next trace — each live pair in launch
    /// order (#685), then each stored run in load order, wrapping back to
    /// the first live pair. A no-op with one pair and nothing loaded.
    ///
    /// Hidden slots are skipped (#256): the selection never rests on a
    /// curve that is not drawn.
    pub fn cycle_focus(&mut self) {
        if self.focus == Focus::Live && self.live_pair + 1 < self.live_count {
            self.live_pair += 1;
            return;
        }
        let from = match self.focus {
            Focus::Live => 0,
            Focus::Stored(idx) => idx + 1,
        };
        self.focus = match (from..self.loaded.len()).find(|&i| self.loaded[i].visible) {
            Some(i) => Focus::Stored(i),
            None => {
                self.live_pair = 0;
                Focus::Live
            }
        };
    }

    /// Put the selection back on live if it rests on a hidden or removed
    /// run (#256).
    fn settle_focus(&mut self) {
        if let Focus::Stored(idx) = self.focus {
            if self.loaded.get(idx).is_none_or(|r| !r.visible) {
                self.focus = Focus::Live;
            }
        }
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
            self.settle_focus();
        }
    }

    /// `P`: force raw phase, or restore the previous non-raw choice.
    pub fn toggle_raw_phase(&mut self) {
        // A focused stored run's own setting (it is drawn as stored).
        if let Focus::Stored(i) = self.focus {
            if let Some(run) = self.loaded.get_mut(i) {
                run.raw_phase = !run.raw_phase;
            }
            return;
        }
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
                // The run's delay as drawn: recorded plus its `←`/`→`
                // offset (#256), so live and slot share one reference.
                .map(|run| {
                    ac_scene::TransferInput::stored_delay_ms(&run.derivation)
                        + run.delay_offset_samples as f64 * 1000.0 / run.sr as f64
                })
                .unwrap_or(self.snapshot_delay_ms),
            Focus::Live => self.snapshot_delay_ms,
        };
        self.derot.to_mode(snapshot_delay_ms)
    }

    /// `H`: toggle the IR panel.
    /// `Shift+H`: the next IR view, opening the panel if it is closed —
    /// the key is only ever pressed to look at a view.
    pub fn cycle_ir_view(&mut self) {
        if self.ir_panel_open {
            self.ir_view = self.ir_view.next();
        } else {
            self.ir_panel_open = true;
        }
    }

    pub fn toggle_ir_panel(&mut self) {
        self.ir_panel_open = !self.ir_panel_open;
    }

    pub fn ir_panel_open(&self) -> bool {
        self.ir_panel_open
    }
}

#[cfg(test)]
mod tests {
    /// #685: `Tab` visits every live pair, then the visible stored runs,
    /// then wraps to the first live pair.
    #[test]
    fn tab_walks_live_pairs_then_stored_runs() {
        let mut t = TransferViewState::default();
        t.set_live_count(2);
        t.cycle_focus();
        assert_eq!((t.focus, t.live_pair), (Focus::Live, 1));
        t.cycle_focus();
        assert_eq!(
            (t.focus, t.live_pair),
            (Focus::Live, 0),
            "no stored run: wrap"
        );
        t.set_live_count(1);
        t.live_pair = 0;
        t.set_live_count(3);
        t.live_pair = 2;
        t.set_live_count(2);
        assert_eq!(t.live_pair, 1, "a shrinking session clamps the selection");
    }

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
