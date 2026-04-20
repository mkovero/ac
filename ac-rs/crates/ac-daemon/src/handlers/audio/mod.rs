//! Audio commands: tone/noise generation, level/frequency sweeps, plot
//! helpers, live spectrum monitor. Each command lives in its own sibling
//! file; this module just re-exports them so `handlers::audio::<name>` keeps
//! working.

mod generate;
mod monitor;
mod plot;
mod sweep;

pub use generate::{generate, generate_pink};
pub use monitor::monitor_spectrum;
pub use plot::{plot, plot_level};
pub use sweep::{sweep_frequency, sweep_level};
