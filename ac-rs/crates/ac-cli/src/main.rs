mod client;
mod commands;
mod io;
mod parse;
mod spawn;

use std::process;

use parse::CommandKind;

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
    let ctrl_port = 5556u16;
    let data_port = 5557u16;
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

    commands::dispatch(parsed, &cfg, &mut client);
}
