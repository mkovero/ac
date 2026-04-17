use crate::parse::CommandKind;

pub fn run(
    cmd: &CommandKind,
    cfg: &ac_core::config::Config,
) {
    let channels = match cmd {
        CommandKind::Monitor { channels, .. } => channels.as_deref(),
        _ => unreachable!(),
    };

    super::plot::launch_ui("spectrum", cfg, channels);
}
