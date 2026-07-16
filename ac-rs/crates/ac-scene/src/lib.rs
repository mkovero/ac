//! `ac-scene` — the pure scene/data layer for the spectrum view
//! (handoff: ac-scene M2). Turns a `transfer_stream` v2 wire frame or a
//! snapshot `PairDerivation` into trace geometry, axis ticks, and
//! readout strings — plain data, zero rendering. No egui, no wgpu, no
//! ZMQ socket code (enforced by this crate's own dependency list, not
//! by convention).
//!
//! # Appendix: the 1.5x / 1.76 dB Hann coherent-vs-noise-gain ratio
//!
//! This one constant has now been independently re-derived at least
//! four times across this project's hand-derived test expectations
//! (M0's `transfer_stream_meas_spectrum_amplitude_truth`, M1.5's
//! fixture self-containment re-derivation, and this crate's AC1 SPL and
//! cursor-readout checks) — it's a single root cause showing up in two
//! different-looking guises, worth citing instead of re-deriving:
//!
//! For a Hann window, coherent gain (mean of the window) is `0.5`;
//! power/noise gain (mean of the window squared) is `0.375`. Their
//! ratio, `0.375 / 0.25 = 1.5` (`10*log10(1.5) ≈ 1.76 dB`), is the same
//! number whether it shows up as:
//! - a bin-exact tone's band-power-aggregated column reading
//!   `sqrt(0.5² + 0.25² + 0.25²) / 0.5 ≈ 1.2247×` its ideal amplitude
//!   (the 3-tap leakage kernel — `1.2247² = 1.5`), or
//! - broadband/noise content's total band-power sum reading `1.5×` too
//!   high when normalized with this codebase's coherent-gain-calibrated
//!   convention (`h1_estimate_core`'s `norm = (nperseg/2)·wc`, tuned so
//!   a pure tone reads its own peak amplitude exactly — correct for
//!   tones, a 1.5× noise-gain overshoot for anything broadband).
//!
//! Any future hand-derivation involving a Welch/Hann-windowed amplitude
//! spectrum in this codebase should expect this factor and cite it here
//! rather than re-deriving it from the window coefficients again.

pub mod dbfs;
pub mod readout;
pub mod scene;
pub mod ticks;
pub mod wire;

pub use scene::{Provenance, Readouts, Scene, SceneInput, Source, Trace};
pub use ticks::{Axis, Tick};
pub use wire::WireFrame;
