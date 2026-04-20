//\! `parse_server` — subcommand parser extracted from `parse/mod.rs`.

use super::*;

pub(super) fn parse_server(args: &[String]) -> Result<ParsedCommand, String> {
    if args.is_empty() {
        return Ok(ParsedCommand {
            cmd: CommandKind::ServerSetHost {
                host: "localhost".into(),
            },
            show_plot: false,
        });
    }

    let sub = match args[0].to_lowercase().as_str() {
        "e" | "en" | "enable" | "start" | "daemon" => "enable",
        "d" | "dis" | "disable" => "disable",
        "c" | "con" | "connections" => "connections",
        _ => "host",
    };

    let cmd = match sub {
        "enable" => CommandKind::ServerEnable,
        "disable" => CommandKind::ServerDisable,
        "connections" => CommandKind::ServerConnections,
        _ => {
            if args.len() > 1 {
                return Err(format!(
                    "unexpected token(s) after host: {:?}",
                    &args[1..]
                ));
            }
            CommandKind::ServerSetHost {
                host: args[0].clone(),
            }
        }
    };

    Ok(ParsedCommand {
        cmd,
        show_plot: false,
    })
}

#[cfg(test)]
mod tests {
    use super::super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn test_server_enable() {
        let p = parse(&args("server enable")).unwrap();
        assert!(matches!(p.cmd, CommandKind::ServerEnable));
        let p = parse(&args("server e")).unwrap();
        assert!(matches!(p.cmd, CommandKind::ServerEnable));
    }

    #[test]
    fn test_server_disable() {
        let p = parse(&args("server disable")).unwrap();
        assert!(matches!(p.cmd, CommandKind::ServerDisable));
    }

    #[test]
    fn test_server_connections() {
        let p = parse(&args("server connections")).unwrap();
        assert!(matches!(p.cmd, CommandKind::ServerConnections));
    }

    #[test]
    fn test_server_host() {
        let p = parse(&args("server 192.168.1.100")).unwrap();
        match p.cmd {
            CommandKind::ServerSetHost { host } => assert_eq!(host, "192.168.1.100"),
            other => panic!("expected ServerSetHost, got {other:?}"),
        }
    }

    #[test]
    fn test_server_default_localhost() {
        let p = parse(&args("server")).unwrap();
        match p.cmd {
            CommandKind::ServerSetHost { host } => assert_eq!(host, "localhost"),
            other => panic!("expected ServerSetHost, got {other:?}"),
        }
    }
}
