use crate::parse::CommandKind;

pub fn run(
    cmd: &CommandKind,
    cfg: &ac_core::config::Config,
) {
    let channels = match cmd {
        CommandKind::Monitor { channels, .. } => channels.clone(),
        _ => unreachable!(),
    };
    let channels = channels.unwrap_or_else(|| vec![cfg.input_channel]);

    super::plot::launch_ui("spectrum", cfg, Some(&channels));
}
