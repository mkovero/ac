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
