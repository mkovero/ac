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

    if args[0] == "ui" {
        exec_ui(&args[1..]);
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
    commands::dispatch(parsed, &cfg, &mut client);
}

fn exec_ui(args: &[String]) {
    let bin = spawn::find_binary("ac-ui");
    match bin {
        Some(path) => {
            let status = std::process::Command::new(&path)
                .args(args)
                .status();
            match status {
                Ok(s) => process::exit(s.code().unwrap_or(1)),
                Err(e) => {
                    eprintln!("  error: failed to launch ac-ui: {e}");
                    process::exit(1);
                }
            }
        }
        None => {
            eprintln!("  error: ac-ui not found — build it with: cd ac-rs && cargo build -p ac-ui");
            process::exit(1);
        }
    }
}
