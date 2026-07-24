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
//! meaning anywhere; `Q` (quit) and `S` (snapshot) are reserved
//! globally. New M4b toggles — raw-phase, de-rotation reference, ref-
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
    TriggerSnapshot,
    OpenSnapshot,
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
    /// Open the settings overlay (channels + start level). M4b binds it;
    /// M4c (#182) implements the overlay itself.
    OpenSettings,

    // -- transfer view: stimulus cluster (reserved, D7/D10) --
    /// Space: Idle→Armed, or stop from Armed/Driving.
    StimulusArmOrStop,
    /// Enter: Armed→Driving, or stop from Driving.
    StimulusFireOrStop,
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
}

/// The full binding table — every view, assigned in one pass (D10). `[`,
/// `]`, `+`, `-` never appear; [`assert_no_forbidden_keys`] enforces it.
///
/// Key ledger (so the single-pass assignment is auditable at a glance):
/// global `/` `Q` `S` `F` `←` `→` `I` `O` `K` `L` `A` `D`; spectrum
/// `W` `T` `V`; transfer `P` `R` `G` + stimulus `Space` `Enter` `Esc`
/// `↑` `↓`.
pub const BINDINGS: &[Binding] = &[
    // -- global --
    Binding {
        key: Key::Slash,
        action: Action::ToggleHelp,
        scope: Scope::Global,
        description: "Toggle help overlay",
    },
    Binding {
        key: Key::Q,
        action: Action::Quit,
        scope: Scope::Global,
        description: "Quit",
    },
    Binding {
        key: Key::S,
        action: Action::TriggerSnapshot,
        scope: Scope::Global,
        description: "Trigger snapshot",
    },
    Binding {
        key: Key::F,
        action: Action::OpenSnapshot,
        scope: Scope::Global,
        description: "Open local .acsnap file",
    },
    Binding {
        key: Key::ArrowLeft,
        action: Action::MoveCursorLeft,
        scope: Scope::Global,
        description: "Move cursor to previous column",
    },
    Binding {
        key: Key::ArrowRight,
        action: Action::MoveCursorRight,
        scope: Scope::Global,
        description: "Move cursor to next column",
    },
    Binding {
        key: Key::I,
        action: Action::ZoomFreqIn,
        scope: Scope::Global,
        description: "Zoom frequency axis in",
    },
    Binding {
        key: Key::O,
        action: Action::ZoomFreqOut,
        scope: Scope::Global,
        description: "Zoom frequency axis out",
    },
    Binding {
        key: Key::K,
        action: Action::ZoomDbIn,
        scope: Scope::Global,
        description: "Zoom level axis in",
    },
    Binding {
        key: Key::L,
        action: Action::ZoomDbOut,
        scope: Scope::Global,
        description: "Zoom level axis out",
    },
    Binding {
        key: Key::A,
        action: Action::PanFreqLeft,
        scope: Scope::Global,
        description: "Pan frequency axis down",
    },
    Binding {
        key: Key::D,
        action: Action::PanFreqRight,
        scope: Scope::Global,
        description: "Pan frequency axis up",
    },
    // -- spectrum view --
    Binding {
        key: Key::W,
        action: Action::CycleWeighting,
        scope: Scope::Spectrum,
        description: "Cycle SPL weighting (snapshot re-derivation)",
    },
    Binding {
        key: Key::T,
        action: Action::CycleIntegration,
        scope: Scope::Spectrum,
        description: "Cycle SPL integration (snapshot re-derivation)",
    },
    Binding {
        key: Key::V,
        action: Action::ToggleRefTrace,
        scope: Scope::Spectrum,
        description: "Toggle reference trace visibility",
    },
    // -- transfer view: toggles --
    Binding {
        key: Key::P,
        action: Action::ToggleRawPhase,
        scope: Scope::Transfer,
        description: "Toggle raw (measured) phase vs de-rotated",
    },
    Binding {
        key: Key::R,
        action: Action::CycleDerotReference,
        scope: Scope::Transfer,
        description: "Cycle de-rotation reference (session / snapshot / raw)",
    },
    Binding {
        key: Key::G,
        action: Action::OpenSettings,
        scope: Scope::Transfer,
        description: "Open settings (channels, start level)",
    },
    // -- transfer view: stimulus cluster (D7/D10 reserved) --
    Binding {
        key: Key::Space,
        action: Action::StimulusArmOrStop,
        scope: Scope::Transfer,
        description: "Arm stimulus (Space); stop if armed/driving",
    },
    Binding {
        key: Key::Enter,
        action: Action::StimulusFireOrStop,
        scope: Scope::Transfer,
        description: "Start driving (Enter); stop if driving",
    },
    Binding {
        key: Key::Escape,
        action: Action::StimulusCancel,
        scope: Scope::Transfer,
        description: "Cancel / stop stimulus (Esc)",
    },
    Binding {
        key: Key::ArrowUp,
        action: Action::StimulusLevelUp,
        scope: Scope::Transfer,
        description: "Raise drive level (Shift: 3 dB step)",
    },
    Binding {
        key: Key::ArrowDown,
        action: Action::StimulusLevelDown,
        scope: Scope::Transfer,
        description: "Lower drive level (Shift: 3 dB step)",
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
        other => format!("{other:?}"),
    }
}

/// Help overlay text for `view` — every binding offered there, one per
/// line. The overlay is per-view so it never lists a key that does
/// nothing in the view the user is looking at.
pub fn help_text(view: ViewId) -> String {
    bindings_for(view)
        .map(|b| format!("{}  {}", key_label(b.key), b.description))
        .collect::<Vec<_>>()
        .join("\n")
}

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
                            | Action::StimulusFireOrStop
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
    }

    #[test]
    fn help_text_never_contains_a_raw_debug_name() {
        for view in [ViewId::Spectrum, ViewId::Transfer] {
            let text = help_text(view);
            for name in ["Slash", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"] {
                assert!(!text.contains(name), "{view:?} help leaked {name}");
            }
        }
    }
}
