//! Keyboard binding table (deliverable 7, D16). Every function reachable
//! by keyboard; `[`, `]`, `+`, `-` are forbidden (Finnish layout — those
//! keys require a modifier chord on that layout, so binding them
//! directly is a usability bug for that keyboard, not a style choice).
//! No toolbars, no menus — the help overlay (single key) is the only
//! always-available chrome.

use egui::Key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ToggleHelp,
    MoveCursorLeft,
    MoveCursorRight,
    ZoomFreqIn,
    ZoomFreqOut,
    ZoomDbIn,
    ZoomDbOut,
    PanFreqLeft,
    PanFreqRight,
    TriggerSnapshot,
    OpenSnapshot,
    CycleWeighting,
    CycleIntegration,
    Quit,
}

pub struct Binding {
    pub key: Key,
    pub action: Action,
    pub description: &'static str,
}

/// The full binding table. `[`, `]`, `+`, `-` never appear here — see
/// [`assert_no_forbidden_keys`], which any test or CI step can call to
/// enforce it structurally rather than by convention.
pub const BINDINGS: &[Binding] = &[
    Binding {
        key: Key::Slash,
        action: Action::ToggleHelp,
        description: "Toggle help overlay",
    },
    Binding {
        key: Key::ArrowLeft,
        action: Action::MoveCursorLeft,
        description: "Move cursor to previous column",
    },
    Binding {
        key: Key::ArrowRight,
        action: Action::MoveCursorRight,
        description: "Move cursor to next column",
    },
    Binding {
        key: Key::I,
        action: Action::ZoomFreqIn,
        description: "Zoom frequency axis in",
    },
    Binding {
        key: Key::O,
        action: Action::ZoomFreqOut,
        description: "Zoom frequency axis out",
    },
    Binding {
        key: Key::K,
        action: Action::ZoomDbIn,
        description: "Zoom dB axis in",
    },
    Binding {
        key: Key::L,
        action: Action::ZoomDbOut,
        description: "Zoom dB axis out",
    },
    Binding {
        key: Key::A,
        action: Action::PanFreqLeft,
        description: "Pan frequency axis down",
    },
    Binding {
        key: Key::D,
        action: Action::PanFreqRight,
        description: "Pan frequency axis up",
    },
    Binding {
        key: Key::S,
        action: Action::TriggerSnapshot,
        description: "Trigger snapshot",
    },
    Binding {
        key: Key::F,
        action: Action::OpenSnapshot,
        description: "Open local .acsnap file",
    },
    Binding {
        key: Key::W,
        action: Action::CycleWeighting,
        description: "Cycle SPL weighting (snapshot re-derivation)",
    },
    Binding {
        key: Key::T,
        action: Action::CycleIntegration,
        description: "Cycle SPL integration (snapshot re-derivation)",
    },
    Binding {
        key: Key::Q,
        action: Action::Quit,
        description: "Quit",
    },
];

/// Panics (a review-rejectable state, not a runtime one — this belongs
/// in a `#[test]`, never on a live keypress path) if any binding uses a
/// forbidden key. D16's Finnish-layout constraint, enforced
/// structurally rather than left to reviewer memory.
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
/// instead of being legible at a glance). Every key `BINDINGS` uses is
/// covered explicitly; anything else falls back to the debug name
/// rather than panicking, since this is display text, not a contract.
fn key_label(key: Key) -> String {
    match key {
        Key::Slash => "/".to_string(),
        Key::ArrowLeft => "←".to_string(),
        Key::ArrowRight => "→".to_string(),
        other => format!("{other:?}"),
    }
}

/// Text for the single-key help overlay — every binding, one per line.
pub fn help_text() -> String {
    BINDINGS
        .iter()
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

    // UX review: the help overlay must show the character a user
    // presses, not egui::Key's Debug name.
    #[test]
    fn key_label_shows_the_actual_character_not_the_debug_name() {
        assert_eq!(key_label(Key::Slash), "/");
        assert_eq!(key_label(Key::ArrowLeft), "←");
        assert_eq!(key_label(Key::ArrowRight), "→");
        // A plain letter key's label IS its debug name coincidentally
        // ("A" == "A") — covered separately so the special-case match
        // above isn't the only thing exercised.
        assert_eq!(key_label(Key::A), "A");
    }

    #[test]
    fn help_text_never_contains_a_raw_arrow_or_slash_debug_name() {
        let text = help_text();
        assert!(!text.contains("Slash"));
        assert!(!text.contains("ArrowLeft"));
        assert!(!text.contains("ArrowRight"));
    }

    #[test]
    fn every_binding_has_a_unique_key() {
        let mut keys: Vec<Key> = BINDINGS.iter().map(|b| b.key).collect();
        let before = keys.len();
        keys.sort_by_key(|k| format!("{k:?}"));
        keys.dedup();
        assert_eq!(keys.len(), before, "duplicate key binding found");
    }

    #[test]
    fn help_text_lists_every_binding() {
        let text = help_text();
        for b in BINDINGS {
            assert!(text.contains(b.description), "missing: {}", b.description);
        }
    }

    #[test]
    fn all_deliverable_6_and_7_functions_are_reachable() {
        // Deliverable 6 (snapshot flow) + deliverable 5 (range
        // adjustment) + help overlay — every named function has a
        // binding, checked by presence of its Action variant.
        let actions: Vec<Action> = BINDINGS.iter().map(|b| b.action).collect();
        for want in [
            Action::ToggleHelp,
            Action::MoveCursorLeft,
            Action::MoveCursorRight,
            Action::ZoomFreqIn,
            Action::ZoomFreqOut,
            Action::ZoomDbIn,
            Action::ZoomDbOut,
            Action::PanFreqLeft,
            Action::PanFreqRight,
            Action::TriggerSnapshot,
            Action::OpenSnapshot,
            Action::CycleWeighting,
            Action::CycleIntegration,
            Action::Quit,
        ] {
            assert!(actions.contains(&want), "unreachable action: {want:?}");
        }
    }
}
