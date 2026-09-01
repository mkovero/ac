//! Reference-plane commands: H1 transfer estimation and channel probe.
//! Both depend on engine port routing and the configured reference channel.
//!
//! Split by what each piece needs to run, which is also what each piece can
//! be tested against:
//!
//! | module | holds | needs |
//! |---|---|---|
//! | [`request`] | the launch contract — pair parsing, [`request::TransferParams`] | a `Value` |
//! | [`plan`] | launch resolution: ports, calibration, the reply | `ServerState` |
//! | [`ctrl`] | `set_drive`, `relock` — CTRL commands that target a live worker | `ServerState` |
//! | [`window`] | the analysis window's geometry and the block-lattice drain (#208) | a `Vec<f32>` |
//! | [`pair`] | per-pair session constants and maintained state | — |
//! | [`analysis`] | the H1 estimate and everything derived from it | rings |
//! | [`frame`] | wire assembly: the frame, the settling frame, the IR sidecar | an estimate |
//! | [`session`] | the per-tick state machine, [`session::SessionState::tick`] | samples and a clock |
//! | [`worker`] | launch, port routing, the capture loop | a daemon |
//! | [`probe`] | the channel probe, which shares nothing but port routing | a daemon |
//!
//! Only [`worker`] and [`probe`] need an audio backend and a socket. That is
//! the line the split is drawn on: everything above them is reachable from a
//! unit test, and most of it did not used to be.
//!
//! Where a module's tests are more than a handful they live in a file of
//! their own beside it — `session/session_tests.rs`, `frame/tests.rs`,
//! `request/tests.rs`, `window/pinned_window_tests.rs` — so the file you
//! open to read an implementation is not four fifths test fixtures.

mod analysis;
mod ctrl;
mod frame;
mod pair;
mod plan;
mod probe;
mod request;
mod session;
mod window;
mod worker;

pub use ctrl::{relock, set_drive};
pub use probe::probe;
pub use worker::transfer_stream;
