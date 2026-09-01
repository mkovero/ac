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
