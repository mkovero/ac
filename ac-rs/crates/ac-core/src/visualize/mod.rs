//! Tier 2 — Live analysis. Display-first, technique-labeled. Values
//! are real measurements but the technique (CWT / CQT / etc.) is not
//! a standards-defined filterbank. See `ARCHITECTURE.md`.

pub mod aggregate;
pub mod cwt;
pub mod fractional_octave;
pub mod spectrum;
pub mod time_integration;
pub mod transfer;
pub mod weighting_curves;
