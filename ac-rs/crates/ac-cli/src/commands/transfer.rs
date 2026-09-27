//! `ac transfer` — launch the `ac-view` transfer view (M4d-CLI #185).
//! A thin wrapper over `spawn_ac_view`: the whole point of this command
//! is that it starts the transfer view with a drive-**off** session, and
//! it does so by carrying no drive option at all.

use crate::parse::CommandKind;

pub fn run(cmd: &CommandKind, cfg: &ac_core::config::Config) {
    let CommandKind::Transfer { channels } = cmd else {
        unreachable!()
    };
    // An explicit channel spec names the measurement channels (#685):
    // each one is measured against the reference from config, one live
    // trace per channel. Before, only the first was used and the rest were
    // dropped without a word.
    let meas_override = channels.as_deref().filter(|c| !c.is_empty());
    if let Some(m) = meas_override {
        let list = m.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
        let noun = if m.len() == 1 { "channel" } else { "channels" };
        eprintln!("  ac transfer: measurement {noun} {list}  (explicit)");
    }
    super::spawn_ac_view(cfg, true, meas_override);
}
