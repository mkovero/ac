//! `ac transfer` — launch the `ac-view` transfer view (M4d-CLI #185).
//! A thin wrapper over `spawn_ac_view`: the whole point of this command
//! is that it starts the transfer view with a drive-**off** session, and
//! it does so by carrying no drive option at all.

use crate::parse::CommandKind;

pub fn run(cmd: &CommandKind, cfg: &ac_core::config::Config) {
    let CommandKind::Transfer { channels } = cmd else {
        unreachable!()
    };
    // An explicit channel spec overrides the measurement leg, same as
    // `ac monitor`. Only the first channel is the meas leg; the reference
    // comes from config (transfer is a fixed meas/ref pair).
    let meas_override = channels.as_ref().and_then(|c| c.first().copied());
    if let Some(m) = meas_override {
        eprintln!("  ac transfer: measurement channel {m}  (explicit)");
    }
    super::spawn_ac_view(cfg, true, meas_override);
}
