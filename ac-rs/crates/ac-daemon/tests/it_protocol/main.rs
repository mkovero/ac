//! ZMQ integration tests against a real `ac-daemon` binary in `--fake-audio` mode.
//!
//! Each test spawns its own daemon on an OS-assigned port pair, drives the
//! CTRL/DATA sockets, and kills the process on drop. No shared state, no
//! hardware needed.
//!
//! One test *binary*, many modules. Separate `tests/it_*.rs` files would link
//! `ac-daemon` once per file and, before [`common::alloc_ports`] moved to
//! OS-assigned ports, would each have needed a hand-picked port base — the
//! trap #195 records.

#[path = "../common/mod.rs"]
mod common;

mod basics;
mod calibrate;
mod level_clamp;
mod modes;
mod monitor;
mod mtw;
mod out_of_range;
mod plot_ir;
mod server;
mod setup;
mod transfer;
mod warmup;
