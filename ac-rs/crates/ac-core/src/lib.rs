pub mod shared;
pub mod visualize;

pub mod analysis;
pub mod config;
pub mod transfer;

#[deprecated(note = "use ac_core::shared::calibration")]
pub use shared::calibration;
#[deprecated(note = "use ac_core::shared::conversions")]
pub use shared::conversions;
#[deprecated(note = "use ac_core::shared::constants")]
pub use shared::constants;
#[deprecated(note = "use ac_core::shared::generator")]
pub use shared::generator;
#[deprecated(note = "use ac_core::shared::types")]
pub use shared::types;

#[deprecated(note = "use ac_core::visualize::cwt")]
pub use visualize::cwt;
#[deprecated(note = "use ac_core::visualize::aggregate")]
pub use visualize::aggregate;
#[deprecated(note = "use ac_core::visualize::fractional_octave")]
pub use visualize::fractional_octave;
