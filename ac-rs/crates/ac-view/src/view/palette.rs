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
pub const COMPARE_COLORS: [Color32; 6] = [
    Color32::from_rgb(0x5f, 0x87, 0xaf), // blue
    Color32::from_rgb(0x87, 0xaf, 0x5f), // green
    Color32::from_rgb(0xaf, 0x5f, 0xaf), // violet
    Color32::from_rgb(0x5f, 0xaf, 0xaf), // teal
    Color32::from_rgb(0xaf, 0xaf, 0x5f), // olive
    Color32::from_rgb(0xaf, 0x87, 0x87), // rose
];

/// The comparison colour for `slot`.
pub fn compare_color(slot: usize) -> Color32 {
    COMPARE_COLORS[slot % COMPARE_COLORS.len()]
}
