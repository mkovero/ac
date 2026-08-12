mod client;
mod commands;
mod io;
mod parse;
mod spawn;

use std::process;

use parse::CommandKind;

/// Daemon port for `var`, defaulting to `default`. Mirrors `ac-daemon`'s
/// own `--ctrl-port` / `--data-port` flags so a client and a daemon can be
/// pointed at the same non-default pair — which is what lets an
/// integration test drive the real `ac` binary against its own
/// `--fake-audio` daemon instead of whatever happens to be listening on
/// 5556 (#283).
///
/// An unparseable value is refused rather than silently falling back:
/// a client that quietly used 5556 after being told otherwise would talk
/// to the wrong daemon and report someone else's measurement.
fn port_override(var: &str, default: u16) -> u16 {
    match std::env::var(var) {
        Err(_) => default,
        Ok(v) => match v.parse::<u16>() {
            Ok(p) if p > 0 => p,
            _ => {
                eprintln!("  error: {var}={v:?} is not a valid TCP port");
                process::exit(1);
            }
        },
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() || args[0] == "-h" || args[0] == "--help" || args[0] == "help" {
        println!("{}", parse::USAGE);
        return;
    }

    let parsed = match parse::parse(&args) {
        Ok(cmd) => cmd,
        Err(e) => {
            eprintln!("\n  error: {e}\n");
            eprintln!("{}", parse::USAGE);
            process::exit(1);
        }
    };

    let cfg = ac_core::config::load(None).unwrap_or_default();

    match &parsed.cmd {
        CommandKind::ServerSetHost { host } => {
            commands::server::set_host(host);
            return;
        }
        CommandKind::SessionNew { .. }
        | CommandKind::SessionList
        | CommandKind::SessionUse { .. }
        | CommandKind::SessionRm { .. }
        | CommandKind::SessionDiff { .. } => {
            commands::session::dispatch(&parsed.cmd, &cfg);
            return;
        }
        CommandKind::Report { path, format } => {
            commands::report::run(path, *format);
            return;
        }
        _ => {}
    }

    let host = cfg.server_host.as_deref().unwrap_or("localhost");
    let ctrl_port = port_override("AC_CTRL_PORT", 5556);
    let data_port = port_override("AC_DATA_PORT", 5557);
    let mut client = match client::AcClient::new(host, ctrl_port, data_port) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  error: cannot connect to server: {e}");
            process::exit(1);
        }
    };

    spawn::ensure_server(&mut client, host);

    if matches!(parsed.cmd, CommandKind::Monitor { .. }) {
        drop(client);
        commands::monitor::run(&parsed.cmd, &cfg);
        return;
    }
    if matches!(parsed.cmd, CommandKind::Transfer { .. }) {
        drop(client);
        commands::transfer::run(&parsed.cmd, &cfg);
        return;
    }

    commands::dispatch(parsed, &cfg, &mut client);
}
