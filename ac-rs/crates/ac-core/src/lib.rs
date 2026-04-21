pub mod shared;
pub mod measurement;
pub mod visualize;

pub mod config;
pub mod transfer;

// Legacy flat paths — emit deprecation warnings; slated for removal in
// v0.2.0 (`ARCHITECTURE.md` transition window).
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

// The old `analysis` module exposed both Tier 1 items (`analyze`,
// `analyze_default`, `AnalysisResult`) and the Tier 2 `spectrum_only`.
// A single `pub use` can't cover both destinations, so keep a thin
// transitional module that re-exports from each tier.
#[deprecated(note = "THD moved to ac_core::measurement::thd; spectrum_only moved to ac_core::visualize::spectrum")]
pub mod analysis {
    pub use crate::measurement::thd::{analyze, analyze_default, find_peak};
    pub use crate::shared::types::AnalysisResult;
    pub use crate::visualize::spectrum::spectrum_only;
}
