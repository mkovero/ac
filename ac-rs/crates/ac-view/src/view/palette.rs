//! The view palette. Four colours, one hue: the project's palette rule
//! is that exactly one thing on screen glows, and everything else is a
//! weight, not a competing colour. Kept in its own module so every
//! drawing module reads the same constants rather than each acquiring
//! its own "close enough" grey.

use egui::Color32;

/// The signal colour (UX review: "the ember" — the one thing on screen
/// that should glow). Never green/blue (this project's own palette
/// rule: they recede in dark environments and carry status/success
/// baggage that conflicts with a neutral signal indicator).
pub const COLOR_SIGNAL: Color32 = Color32::from_rgb(0xd7, 0x87, 0x5f);
/// Reference channel: recedes via weight, not a second competing hue.
pub const COLOR_STRUCTURAL: Color32 = Color32::from_rgb(0x62, 0x62, 0x62);
/// Axis tick labels: mid grey, one step brighter than
/// [`COLOR_STRUCTURAL`]'s "inactive/context" register.
pub const COLOR_LABEL: Color32 = Color32::from_rgb(0x9e, 0x9e, 0x9e);
/// Readout text: near-white, not pure white — pure white reads harsher
/// than the palette calls for and competes with the ember trace.
pub const COLOR_VALUE: Color32 = Color32::from_rgb(0xe4, 0xe4, 0xe4);

/// Stored-run colours for comparison (#256). The palette rule above has
/// one exception: several stored runs drawn in one grey cannot be told
/// apart, which defeats comparing them. Muted, mutually distinct hues,
/// none as bright as [`COLOR_SIGNAL`], so the focused trace still glows.
/// Cycled by `LoadedRun::color_slot`.
/// Nine, one per slot (`Ctrl`+1…9), so no two slots share a colour.
pub const COMPARE_COLORS: [Color32; 9] = [
    Color32::from_rgb(0x5f, 0x87, 0xaf), // 1 blue
    Color32::from_rgb(0x87, 0xaf, 0x5f), // 2 green
    Color32::from_rgb(0xaf, 0x5f, 0xaf), // 3 violet
    Color32::from_rgb(0x5f, 0xaf, 0xaf), // 4 teal
    Color32::from_rgb(0xaf, 0xaf, 0x5f), // 5 olive
    Color32::from_rgb(0xaf, 0x87, 0x87), // 6 rose
    Color32::from_rgb(0x87, 0x87, 0xd7), // 7 lavender
    Color32::from_rgb(0x5f, 0xaf, 0x87), // 8 sea green
    Color32::from_rgb(0xd7, 0xaf, 0x87), // 9 sand
];

/// Live-pair colours (#685). The first pair is the signal colour, as a
/// single live trace always was; further pairs of one session each need
/// their own. Brighter than [`COMPARE_COLORS`], so a live trace still
/// reads as live beside the slots, and none repeats a slot colour.
pub const LIVE_COLORS: [Color32; 4] = [
    COLOR_SIGNAL,
    Color32::from_rgb(0xd7, 0xd7, 0x5f), // yellow
    Color32::from_rgb(0x5f, 0xd7, 0xd7), // cyan
    Color32::from_rgb(0xd7, 0x5f, 0xd7), // magenta
];

/// The colour of live pair `pair` (launch order), cycling past four.
pub fn live_color(pair: usize) -> Color32 {
    LIVE_COLORS[pair % LIVE_COLORS.len()]
}

/// The colour slot of the slot average (#671): drawn in the value colour,
/// apart from every slot's.
pub const AVERAGE_SLOT: usize = usize::MAX;

/// The comparison colour for `slot`.
pub fn compare_color(slot: usize) -> Color32 {
    if slot == AVERAGE_SLOT {
        return COLOR_VALUE;
    }
    COMPARE_COLORS[slot % COMPARE_COLORS.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #685: every live pair's colour differs from the others' and from
    /// every slot's, so a live trace is never mistaken for a stored one.
    #[test]
    fn live_colours_are_distinct_from_each_other_and_the_slots() {
        for (i, a) in LIVE_COLORS.iter().enumerate() {
            assert!(
                !COMPARE_COLORS.contains(a),
                "live {i} repeats a slot colour"
            );
            assert_ne!(*a, COLOR_VALUE, "live {i} is the average's colour");
            for b in &LIVE_COLORS[i + 1..] {
                assert_ne!(a, b, "live {i} repeats a live colour");
            }
        }
        assert_eq!(live_color(0), COLOR_SIGNAL);
    }

    /// Every slot draws in its own colour, and none is the focus colour.
    #[test]
    fn the_nine_slot_colours_are_distinct() {
        for (i, a) in COMPARE_COLORS.iter().enumerate() {
            assert_ne!(*a, COLOR_SIGNAL, "slot {} is the focus colour", i + 1);
            for b in &COMPARE_COLORS[i + 1..] {
                assert_ne!(a, b, "slot {} repeats a colour", i + 1);
            }
        }
    }
}
