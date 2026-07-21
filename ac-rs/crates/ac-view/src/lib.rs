//! `ac-view` — the keyboard-driven egui shell for the spectrum view
//! (handoff: ac-view M3). The one structural rule: **this crate
//! computes nothing**. Every number, string, coordinate, tick, and
//! label comes from `ac-scene` (live path) or `ac-core::snapshot` →
//! `ac-scene` (snapshot path). See `computes_nothing::tests` for the
//! enforcement mechanism.

pub mod app;
pub mod computes_nothing;
pub mod geometry;
pub mod keys;
pub mod range;
pub mod session;
pub mod snapshot_flow;
pub mod view;
pub mod zmq_client;
