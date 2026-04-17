use crate::parse::CommandKind;

pub fn run(
    cmd: &CommandKind,
    cfg: &ac_core::config::Config,
) {
    let _params = match cmd {
        CommandKind::Monitor { .. } => {}
        _ => unreachable!(),
    };

    super::plot::launch_ui("spectrum", cfg);
}
