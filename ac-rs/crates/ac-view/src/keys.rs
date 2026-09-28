//! Keyboard binding table (deliverable 7, D16; single-pass assignment,
//! D10). Every function reachable by keyboard; `[`, `]`, `+`, `-` are
//! forbidden (Finnish layout — those keys require a modifier chord on
//! that layout, so binding them directly is a usability bug for that
//! keyboard, not a style choice). No toolbars, no menus — the help
//! overlay (single key) is the only always-available chrome.
//!
//! # D10 — one assignment pass over the whole table
//!
//! Key letters are assigned here, once, across both views. The stimulus
//! cluster (Space / Enter / Esc / ↑ / ↓) is reserved for the transfer
//! view's arm→fire→stop and level control and is never given another
//! meaning anywhere — except Enter, which pauses the live trace when the
//! stimulus is not armed (#256); `Q` (quit) is reserved globally. New M4b toggles — raw-phase, de-rotation reference, ref-
//! trace visibility, settings overlay — take the remaining letters in
//! this same pass, so key allocation never becomes an incremental
//! scramble across later PRs.
//!
//! # Per-view tables, no dead keys
//!
//! A binding belongs to a [`Scope`]: `Global` (both views), `Spectrum`,
//! or `Transfer`. The dispatcher only offers a key to the current view,
//! and [`bindings_for`] is what the app iterates. Every action a view
//! lists must do something in that view — enforced structurally by the
//! app's dispatch matching on exactly these actions (the
//! `no_dead_keys` intent), not by reviewer memory.

use egui::Key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // -- global --
    ToggleHelp,
    Quit,
    /// `F` (#256): the saved-captures list; a slot digit loads the
    /// selected file into that slot.
    OpenSnapshot,
    /// `C` (#256): write the selected trace to CSV.
    ExportCsv,
    /// `B` (#670): cycle the display's coherence mask.
    CycleCoherenceMask,
    /// `M` (#671): the average of the visible slots, on or off; Shift:
    /// coherence weighting on or off.
    ToggleAverage,
    MoveCursorLeft,
    MoveCursorRight,
    ZoomFreqIn,
    ZoomFreqOut,
    ZoomDbIn,
    ZoomDbOut,
    PanFreqLeft,
    PanFreqRight,

    // -- spectrum view --
    CycleWeighting,
    CycleIntegration,
    /// Ref-trace visibility (spectrum view — the one new spectrum toggle,
    /// mission §1). Transfer view has no ref spectrum (D1).
    ToggleRefTrace,

    // -- transfer view: toggles --
    /// Show measured (raw) phase instead of the de-rotated default (D3).
    ToggleRawPhase,
    /// Cycle the de-rotation reference: session / snapshot / raw (D3).
    CycleDerotReference,
    /// Cycle fractional-octave smoothing of the trace: off / 1/24 / 1/12 /
    /// 1/6 / 1/3 / 1/1 (#229). Display-only — it does not change the column
    /// density, which is fixed (`design-mtw-ladder.md`, decision 3).
    CycleSmoothing,
    /// Open the settings overlay (channels + start level). M4b binds it;
    /// M4c (#182) implements the overlay itself.
    OpenSettings,
    /// Insert the found delay (#669): `set_delay` to the held delay plus the
    /// live IR's residual — Smaart's Find → Insert, one key because the
    /// residual is on screen already. Shift: discard and find again from
    /// the unaligned IR (`set_delay` null), for a setting so far off that the
    /// residual cannot reach the arrival.
    InsertDelay,
    /// Move the selected trace's delay one sample earlier, ten with Shift
    /// (#669, #256: `←`, live or slot).
    NudgeDelayEarlier,
    /// Move the selected trace's delay one sample later, ten with Shift
    /// (#669, #256: `→`, live or slot).
    NudgeDelayLater,
    /// Open the typed-delay entry (#669): digits in samples, `T` again to
    /// apply.
    /// `U`: draw the selected trace inverted, or put it back (Smaart's
    /// Invert, for setting an EQ against a response).
    ToggleInvert,
    /// `J`: type a dB offset for the selected trace; `J` applies.
    TypeOffset,
    /// `Z`: list target curves to draw over the magnitude pane (`Z`
    /// loads the selected one); `Shift+Z` clears it.
    OpenTargets,
    TypeDelay,
    /// Show or hide the focused trace (#256). Shift: show every trace.
    ToggleTraceVisible,
    /// Toggle the IR panel (#286) — h(t) from the `visualize/ir` sidecar,
    /// live-arrival only (the sweep-derived kind is a disjoint data path,
    /// #308). Mnemonic: `H`, this display's own y-axis label.
    ToggleIrPanel,
    /// `S` (#706): the live IR panel's arrival IR (250 ms, fast) or the
    /// 1 s one.
    ToggleIrSpan,
    /// `W` (#714): the ladder's speed preset, Detail → Live → Follow.
    CycleSpeed,
    /// Move focus to the next trace — live, then each loaded stored run,
    /// wrapping back to live (#321). Selects what `N` (smoothing) edits
    /// and what the delay readout names. Not yet reachable from any
    /// stored run in a production build — loading one is #256's file
    /// picker, still a stub — but wired and tested now, same pattern
    /// `Action::OpenSnapshot` already documents.
    /// `Y` (#687): delay tracking on or off for the selected live pair —
    /// the daemon then follows the arrival as the mic moves.
    ToggleDelayTracking,
    CycleFocus,
    /// Close the focused stored run (#321). A no-op while focus is on
    /// the live trace.
    CloseFocusedRun,

    // -- transfer view: stimulus cluster (reserved, D7/D10) --
    /// Space: Idle→Armed, or stop from Armed/Driving.
    StimulusArmOrStop,
    /// Enter: Armed→Driving; otherwise pause / resume the live trace (#256).
    StimulusFireOrPause,
    /// Esc: cancel/stop from any state.
    StimulusCancel,
    /// ↑: raise drive level (M4b: local state; M4c wires the clamp+send).
    StimulusLevelUp,
    /// ↓: lower drive level.
    StimulusLevelDown,
}

/// Which view a binding is offered to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Spectrum,
    Transfer,
}

#[derive(Debug)]
pub struct Binding {
    pub key: Key,
    pub action: Action,
    pub scope: Scope,
    pub description: &'static str,
    /// What `Shift` with this key does, said on its own help row; `None`
    /// when `Shift` adds nothing.
    pub shift: Option<&'static str>,
}

/// The full binding table — every view, assigned in one pass (D10). `[`,
/// `]`, `+`, `-` never appear; [`assert_no_forbidden_keys`] enforces it.
///
/// Key ledger (so the single-pass assignment is auditable at a glance):
/// global `/` `Q` `F` `←` `→` `I` `O` `K` `L` `A` `D`; spectrum
/// `W` `T` `V`; transfer `P` `R` `N` `G` `E` `H` `Tab` `X` (#321: cycle
/// trace focus / close a stored run) `,` `.` `T` (#669: nudge / type the
/// delay — `T` is spectrum's too, the views never share a table) `V`
/// (#256: show/hide the focused trace — `V` likewise) + stimulus `Space`
/// `Enter` `Esc` `↑` `↓` (#256: Enter fires only when armed; otherwise it
/// pauses the live trace) + `Ctrl`+`1`…`9` slots ([`SLOT_KEYS`]).
pub const BINDINGS: &[Binding] = &[
    // -- global --
    Binding {
        key: Key::Slash,
        action: Action::ToggleHelp,
        scope: Scope::Global,
        description: "Show / hide this help",
        shift: None,
    },
    Binding {
        key: Key::Q,
        action: Action::Quit,
        scope: Scope::Global,
        description: "Quit",
        shift: None,
    },
    Binding {
        key: Key::F,
        action: Action::OpenSnapshot,
        scope: Scope::Transfer,
        description: "Saved captures (\u{2191}\u{2193} pick, 1\u{2026}9 load into that slot)",
        shift: None,
    },
    Binding {
        key: Key::ArrowLeft,
        action: Action::MoveCursorLeft,
        scope: Scope::Spectrum,
        description: "Cursor to the previous column",
        shift: None,
    },
    Binding {
        key: Key::ArrowRight,
        action: Action::MoveCursorRight,
        scope: Scope::Spectrum,
        description: "Cursor to the next column",
        shift: None,
    },
    Binding {
        key: Key::I,
        action: Action::ZoomFreqIn,
        scope: Scope::Global,
        description: "Zoom frequency in",
        shift: None,
    },
    Binding {
        key: Key::O,
        action: Action::ZoomFreqOut,
        scope: Scope::Global,
        description: "Zoom frequency out",
        shift: None,
    },
    Binding {
        key: Key::K,
        action: Action::ZoomDbIn,
        scope: Scope::Global,
        description: "Zoom level in",
        shift: None,
    },
    Binding {
        key: Key::L,
        action: Action::ZoomDbOut,
        scope: Scope::Global,
        description: "Zoom level out",
        shift: None,
    },
    Binding {
        key: Key::A,
        action: Action::PanFreqLeft,
        scope: Scope::Global,
        description: "Pan frequency down",
        shift: None,
    },
    Binding {
        key: Key::D,
        action: Action::PanFreqRight,
        scope: Scope::Global,
        description: "Pan frequency up",
        shift: None,
    },
    // -- spectrum view --
    Binding {
        key: Key::W,
        action: Action::CycleWeighting,
        scope: Scope::Spectrum,
        description: "SPL weighting (re-derives the snapshot)",
        shift: None,
    },
    Binding {
        key: Key::T,
        action: Action::CycleIntegration,
        scope: Scope::Spectrum,
        description: "SPL integration (re-derives the snapshot)",
        shift: None,
    },
    Binding {
        key: Key::V,
        action: Action::ToggleRefTrace,
        scope: Scope::Spectrum,
        description: "Reference trace on / off",
        shift: None,
    },
    // -- transfer view: toggles --
    Binding {
        key: Key::P,
        action: Action::ToggleRawPhase,
        scope: Scope::Transfer,
        description: "Raw phase / de-rotated phase",
        shift: Some("Phase view: wrapped / unwrapped / group delay"),
    },
    Binding {
        key: Key::R,
        action: Action::CycleDerotReference,
        scope: Scope::Transfer,
        description: "Phase reference: session / snapshot / raw",
        shift: None,
    },
    Binding {
        key: Key::N,
        action: Action::CycleSmoothing,
        scope: Scope::Transfer,
        description: "Smoothing: off / 1/24 / 1/12 / 1/6 / 1/3 / 1/1 octave",
        shift: None,
    },
    Binding {
        key: Key::G,
        action: Action::OpenSettings,
        scope: Scope::Transfer,
        description: "Settings (channels, start level)",
        shift: None,
    },
    Binding {
        key: Key::E,
        action: Action::InsertDelay,
        scope: Scope::Transfer,
        description: "Use the found delay",
        shift: Some("Find the delay again"),
    },
    Binding {
        key: Key::Y,
        action: Action::ToggleDelayTracking,
        scope: Scope::Transfer,
        description: "Delay tracking on / off (selected live pair)",
        shift: None,
    },
    Binding {
        key: Key::ArrowLeft,
        action: Action::NudgeDelayEarlier,
        scope: Scope::Transfer,
        description: "Delay 1 sample earlier (selected trace)",
        shift: Some("Delay 10 samples earlier"),
    },
    Binding {
        key: Key::ArrowRight,
        action: Action::NudgeDelayLater,
        scope: Scope::Transfer,
        description: "Delay 1 sample later (selected trace)",
        shift: Some("Delay 10 samples later"),
    },
    Binding {
        key: Key::V,
        action: Action::ToggleTraceVisible,
        scope: Scope::Transfer,
        description: "Show / hide the selected trace",
        shift: Some("Show every trace"),
    },
    Binding {
        key: Key::B,
        action: Action::CycleCoherenceMask,
        scope: Scope::Transfer,
        description: "Coherence mask: 0.3 / 0.5 / 0.7 / 0.9",
        shift: None,
    },
    Binding {
        key: Key::C,
        action: Action::ExportCsv,
        scope: Scope::Transfer,
        description: "Selected trace to CSV",
        shift: Some("The average to CSV"),
    },
    Binding {
        key: Key::M,
        action: Action::ToggleAverage,
        scope: Scope::Transfer,
        description: "Average of the visible slots",
        shift: Some("Coherence weighting on / off"),
    },
    Binding {
        key: Key::T,
        action: Action::TypeDelay,
        scope: Scope::Transfer,
        description: "Type a delay in samples (Enter applies)",
        shift: None,
    },
    Binding {
        key: Key::U,
        action: Action::ToggleInvert,
        scope: Scope::Transfer,
        description: "Invert the selected trace (display only)",
        shift: None,
    },
    Binding {
        key: Key::J,
        action: Action::TypeOffset,
        scope: Scope::Transfer,
        description: "Type a dB offset for the selected trace (Enter applies, empty resets)",
        shift: None,
    },
    Binding {
        key: Key::Z,
        action: Action::OpenTargets,
        scope: Scope::Transfer,
        description: "Target curves (Z again loads the picked one)",
        shift: Some("Clear the target curve"),
    },
    Binding {
        key: Key::H,
        action: Action::ToggleIrPanel,
        scope: Scope::Transfer,
        description: "Impulse response panel on / off",
        shift: Some("IR view: linear / log / ETC"),
    },
    Binding {
        key: Key::S,
        action: Action::ToggleIrSpan,
        scope: Scope::Transfer,
        description: "Live IR: arrival 250 ms (fast) / 1 s",
        shift: None,
    },
    Binding {
        key: Key::W,
        action: Action::CycleSpeed,
        scope: Scope::Transfer,
        description: "Speed: Detail / Live / Follow (LF resolution vs update rate)",
        shift: Some("Ease the live trace between estimates (40 ms) on / off"),
    },
    Binding {
        key: Key::Tab,
        action: Action::CycleFocus,
        scope: Scope::Transfer,
        description: "Select the next trace (live pairs, then slots and files)",
        shift: None,
    },
    Binding {
        key: Key::X,
        action: Action::CloseFocusedRun,
        scope: Scope::Transfer,
        description: "Close the selected stored run",
        shift: None,
    },
    // -- transfer view: stimulus cluster (D7/D10 reserved) --
    Binding {
        key: Key::Space,
        action: Action::StimulusArmOrStop,
        scope: Scope::Transfer,
        description: "Arm the stimulus; stop it if armed or driving",
        shift: None,
    },
    Binding {
        key: Key::Enter,
        action: Action::StimulusFireOrPause,
        scope: Scope::Transfer,
        description: "Start driving if armed; otherwise pause / resume live",
        shift: None,
    },
    Binding {
        key: Key::Escape,
        action: Action::StimulusCancel,
        scope: Scope::Transfer,
        description: "Cancel / stop the stimulus",
        shift: None,
    },
    Binding {
        key: Key::ArrowUp,
        action: Action::StimulusLevelUp,
        scope: Scope::Transfer,
        description: "Drive level up 1 dB",
        shift: Some("Drive level up 3 dB"),
    },
    Binding {
        key: Key::ArrowDown,
        action: Action::StimulusLevelDown,
        scope: Scope::Transfer,
        description: "Drive level down 1 dB",
        shift: Some("Drive level down 3 dB"),
    },
];

/// Which fixed view is active — the app never switches at runtime (§1),
/// but the key table is shared, so dispatch and help need to know which
/// view's bindings apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewId {
    Spectrum,
    Transfer,
}

impl Scope {
    /// Whether a binding of this scope is offered in `view`.
    pub fn applies_to(self, view: ViewId) -> bool {
        matches!(
            (self, view),
            (Scope::Global, _)
                | (Scope::Spectrum, ViewId::Spectrum)
                | (Scope::Transfer, ViewId::Transfer)
        )
    }
}

/// The bindings offered in `view`: global plus that view's own.
pub fn bindings_for(view: ViewId) -> impl Iterator<Item = &'static Binding> {
    BINDINGS.iter().filter(move |b| b.scope.applies_to(view))
}

/// Panics (a review-rejectable state, not a runtime one — this belongs
/// in a `#[test]`, never on a live keypress path) if any binding uses a
/// forbidden key. D16's Finnish-layout constraint, enforced structurally
/// rather than left to reviewer memory.
pub fn assert_no_forbidden_keys() {
    let forbidden = [Key::OpenBracket, Key::CloseBracket, Key::Plus, Key::Minus];
    for b in BINDINGS {
        assert!(
            !forbidden.contains(&b.key),
            "forbidden key bound: {:?} ({})",
            b.key,
            b.description
        );
    }
}

/// The character/symbol a user actually presses — not `Key`'s `Debug`
/// name (UX review: showing `"Slash"`/`"ArrowLeft"` instead of `/`/`←`
/// makes the one piece of always-available chrome require translation
/// instead of being legible at a glance).
fn key_label(key: Key) -> String {
    match key {
        Key::Slash => "/".to_string(),
        Key::ArrowLeft => "←".to_string(),
        Key::ArrowRight => "→".to_string(),
        Key::ArrowUp => "↑".to_string(),
        Key::ArrowDown => "↓".to_string(),
        Key::Space => "Space".to_string(),
        Key::Enter => "Enter".to_string(),
        Key::Escape => "Esc".to_string(),
        Key::Comma => ",".to_string(),
        Key::Period => ".".to_string(),
        other => format!("{other:?}"),
    }
}

/// The help overlay's sections, in the order they are drawn. Grouped by
/// what the operator is doing rather than by key, so a key is found by its
/// job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpGroup {
    Stimulus,
    Traces,
    Display,
    Delay,
    Cursor,
    Axes,
    General,
}

impl HelpGroup {
    pub const ALL: [HelpGroup; 7] = [
        HelpGroup::Stimulus,
        HelpGroup::Traces,
        HelpGroup::Display,
        HelpGroup::Delay,
        HelpGroup::Cursor,
        HelpGroup::Axes,
        HelpGroup::General,
    ];

    pub fn title(self) -> &'static str {
        match self {
            HelpGroup::Stimulus => "Stimulus",
            HelpGroup::Traces => "Traces and slots",
            HelpGroup::Display => "Display",
            HelpGroup::Delay => "Delay",
            HelpGroup::Cursor => "Cursor",
            HelpGroup::Axes => "Axes",
            HelpGroup::General => "General",
        }
    }

    /// The section `action` is listed under. Exhaustive, so a new action
    /// cannot reach the help without a place in it.
    pub fn of(action: Action) -> HelpGroup {
        use Action::*;
        match action {
            StimulusArmOrStop | StimulusFireOrPause | StimulusCancel | StimulusLevelUp
            | StimulusLevelDown => HelpGroup::Stimulus,
            CycleFocus | ToggleTraceVisible | CloseFocusedRun | OpenSnapshot | ToggleAverage
            | ExportCsv => HelpGroup::Traces,
            CycleSmoothing | CycleCoherenceMask | ToggleRawPhase | CycleDerotReference
            | ToggleInvert | TypeOffset | OpenTargets | ToggleIrPanel | ToggleIrSpan
            | CycleSpeed | CycleWeighting | CycleIntegration | ToggleRefTrace => HelpGroup::Display,
            InsertDelay | ToggleDelayTracking | NudgeDelayEarlier | NudgeDelayLater | TypeDelay => {
                HelpGroup::Delay
            }
            MoveCursorLeft | MoveCursorRight => HelpGroup::Cursor,
            ZoomFreqIn | ZoomFreqOut | ZoomDbIn | ZoomDbOut | PanFreqLeft | PanFreqRight => {
                HelpGroup::Axes
            }
            ToggleHelp | Quit | OpenSettings => HelpGroup::General,
        }
    }
}

/// One help row: the keys as pressed (`Shift+V`, `Ctrl+1…9`) and what
/// they do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpRow {
    pub keys: String,
    pub text: &'static str,
}

/// The help overlay for `view`: every binding offered there, grouped
/// ([`HelpGroup`]), a `Shift` variant on its own row under its key, empty
/// groups left out. Per-view, so it never lists a key that does nothing in
/// the view the user is looking at.
pub fn help_sections(view: ViewId) -> Vec<(HelpGroup, Vec<HelpRow>)> {
    HelpGroup::ALL
        .iter()
        .filter_map(|&group| {
            let mut rows: Vec<HelpRow> = Vec::new();
            for b in bindings_for(view).filter(|b| HelpGroup::of(b.action) == group) {
                let key = key_label(b.key);
                rows.push(HelpRow {
                    keys: key.clone(),
                    text: b.description,
                });
                if let Some(shift) = b.shift {
                    rows.push(HelpRow {
                        keys: format!("Shift+{key}"),
                        text: shift,
                    });
                }
            }
            if view == ViewId::Transfer && group == HelpGroup::Traces {
                let n = SLOT_KEYS.len();
                rows.push(HelpRow {
                    keys: format!("1\u{2026}{n}"),
                    text: SLOT_TOGGLE_HELP,
                });
                rows.push(HelpRow {
                    keys: format!("Ctrl+1\u{2026}{n}"),
                    text: SLOT_HELP,
                });
            }
            if view == ViewId::Transfer && group == HelpGroup::Display {
                rows.push(HelpRow {
                    keys: "Mouse".to_string(),
                    text: MOUSE_HELP,
                });
            }
            (!rows.is_empty()).then_some((group, rows))
        })
        .collect()
}

/// The help overlay as plain text, one section heading then one aligned
/// row per key — what [`help_sections`] draws, for tests and logs.
pub fn help_text(view: ViewId) -> String {
    let mut out = String::new();
    for (group, rows) in help_sections(view) {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(group.title());
        out.push('\n');
        for r in rows {
            out.push_str(&format!("  {:<10} {}\n", r.keys, r.text));
        }
    }
    out
}

/// The slot keys (#256), `Ctrl` + a digit, in slot order. Not in
/// [`BINDINGS`]: the table has no modifiers, and a bare digit must not
/// store anything. The app checks these with `Ctrl` held to store, and
/// bare to show or hide the slot.
pub const SLOT_KEYS: [Key; 9] = [
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
    Key::Num8,
    Key::Num9,
];

/// The help line for the pointer cursor (#718).
pub const MOUSE_HELP: &str =
    "Hover reads the selected trace; click pins a cursor, right-click clears it";

/// The help line for a bare slot digit.
pub const SLOT_TOGGLE_HELP: &str = "Show / hide that slot";

/// The help line for [`SLOT_KEYS`].
pub const SLOT_HELP: &str = "Store live into that slot (replaces it; live must be running)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_forbidden_keys_bound() {
        assert_no_forbidden_keys();
    }

    #[test]
    fn key_label_shows_the_actual_character_not_the_debug_name() {
        assert_eq!(key_label(Key::Slash), "/");
        assert_eq!(key_label(Key::ArrowLeft), "←");
        assert_eq!(key_label(Key::ArrowRight), "→");
        assert_eq!(key_label(Key::ArrowUp), "↑");
        assert_eq!(key_label(Key::ArrowDown), "↓");
        assert_eq!(key_label(Key::Escape), "Esc");
        // A plain letter key's label IS its debug name coincidentally.
        assert_eq!(key_label(Key::A), "A");
    }

    // Uniqueness is now per-view: a key may not resolve to two actions in
    // the SAME view (global + that view's own). Across different views the
    // same key legitimately means different things (e.g. ↑ is stimulus in
    // transfer, unused in spectrum) — that is the point of scoping.
    #[test]
    fn every_key_is_unique_within_each_view() {
        for view in [ViewId::Spectrum, ViewId::Transfer] {
            let mut keys: Vec<Key> = bindings_for(view).map(|b| b.key).collect();
            let before = keys.len();
            keys.sort_by_key(|k| format!("{k:?}"));
            keys.dedup();
            assert_eq!(keys.len(), before, "duplicate key within {view:?}");
        }
    }

    // The stimulus cluster is reserved for the transfer view and must not
    // be reused for anything, in any view (D10).
    #[test]
    fn stimulus_cluster_is_reserved_to_transfer_and_only_stimulus() {
        let cluster = [
            Key::Space,
            Key::Enter,
            Key::Escape,
            Key::ArrowUp,
            Key::ArrowDown,
        ];
        for b in BINDINGS {
            if cluster.contains(&b.key) {
                assert_eq!(
                    b.scope,
                    Scope::Transfer,
                    "stimulus key outside transfer: {b:?}"
                );
                assert!(
                    matches!(
                        b.action,
                        Action::StimulusArmOrStop
                            | Action::StimulusFireOrPause
                            | Action::StimulusCancel
                            | Action::StimulusLevelUp
                            | Action::StimulusLevelDown
                    ),
                    "stimulus key bound to a non-stimulus action: {b:?}"
                );
            }
        }
    }

    #[test]
    fn help_text_is_per_view_and_lists_every_binding_of_that_view() {
        for view in [ViewId::Spectrum, ViewId::Transfer] {
            let text = help_text(view);
            for b in bindings_for(view) {
                assert!(
                    text.contains(b.description),
                    "{view:?} help missing: {}",
                    b.description
                );
            }
        }
    }

    // Spectrum help must NOT advertise transfer-only keys, and vice
    // versa — a listed key that does nothing in the current view is the
    // dead-key defect, seen from the help side.
    #[test]
    fn help_text_does_not_list_other_views_keys() {
        let spectrum = help_text(ViewId::Spectrum);
        assert!(
            !spectrum.contains("de-rotation"),
            "spectrum help lists a transfer toggle"
        );
        assert!(
            !spectrum.contains("drive level"),
            "spectrum help lists stimulus"
        );

        let transfer = help_text(ViewId::Transfer);
        assert!(
            !transfer.contains("SPL weighting"),
            "transfer help lists a spectrum-only toggle"
        );
        assert!(
            !spectrum.contains("Impulse response panel"),
            "spectrum help lists the transfer-only IR toggle"
        );
        assert!(
            transfer.contains("Impulse response panel"),
            "transfer help missing the IR toggle"
        );
    }

    /// Every `Shift` variant has its own row, named `Shift+<key>`, right
    /// under its key — the reading the old one-line-per-binding list buried
    /// in parentheses.
    #[test]
    fn every_shift_variant_has_its_own_row_under_its_key() {
        for view in [ViewId::Spectrum, ViewId::Transfer] {
            let rows: Vec<HelpRow> = help_sections(view)
                .into_iter()
                .flat_map(|(_, rows)| rows)
                .collect();
            for b in bindings_for(view) {
                let at = rows
                    .iter()
                    .position(|r| r.text == b.description)
                    .expect("binding listed");
                assert_eq!(rows[at].keys, key_label(b.key));
                if let Some(shift) = b.shift {
                    assert_eq!(rows[at + 1].text, shift);
                    assert_eq!(rows[at + 1].keys, format!("Shift+{}", key_label(b.key)));
                }
                assert!(
                    !b.description.contains("Shift"),
                    "Shift still buried in: {}",
                    b.description
                );
            }
        }
    }

    /// Sections come in [`HelpGroup::ALL`] order, none empty, and a view
    /// without a stimulus lists no stimulus section.
    #[test]
    fn help_sections_are_ordered_and_never_empty() {
        for view in [ViewId::Spectrum, ViewId::Transfer] {
            let groups: Vec<HelpGroup> = help_sections(view).iter().map(|(g, _)| *g).collect();
            let order: Vec<HelpGroup> = HelpGroup::ALL
                .iter()
                .copied()
                .filter(|g| groups.contains(g))
                .collect();
            assert_eq!(groups, order, "{view:?}");
            assert!(help_sections(view).iter().all(|(_, rows)| !rows.is_empty()));
        }
        let spectrum: Vec<HelpGroup> = help_sections(ViewId::Spectrum)
            .iter()
            .map(|(g, _)| *g)
            .collect();
        assert!(!spectrum.contains(&HelpGroup::Stimulus));
        assert!(!spectrum.contains(&HelpGroup::Delay));
    }

    #[test]
    fn help_text_never_contains_a_raw_debug_name() {
        for view in [ViewId::Spectrum, ViewId::Transfer] {
            let text = help_text(view);
            for name in [
                "Slash",
                "ArrowLeft",
                "ArrowRight",
                "ArrowUp",
                "ArrowDown",
                "Comma",
                "Period",
            ] {
                assert!(!text.contains(name), "{view:?} help leaked {name}");
            }
        }
    }
}
