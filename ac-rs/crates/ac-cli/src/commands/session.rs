use crate::parse::CommandKind;

pub fn dispatch(cmd: &CommandKind, _cfg: &ac_core::config::Config) {
    match cmd {
        CommandKind::SessionNew { name } => new_session(name),
        CommandKind::SessionList => list_sessions(),
        CommandKind::SessionUse { name } => use_session(name),
        CommandKind::SessionRm { name } => rm_session(name),
        CommandKind::SessionDiff { name_a, name_b } => diff_sessions(name_a, name_b),
        _ => unreachable!(),
    }
}

fn session_base() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home)
        .join(".local/share/ac/sessions")
}

fn session_dir(name: &str) -> std::path::PathBuf {
    session_base().join(name)
}

fn new_session(name: &str) {
    let dir = session_dir(name);
    if dir.exists() {
        eprintln!("  error: session {name:?} already exists");
        std::process::exit(1);
    }
    std::fs::create_dir_all(&dir).ok();
    let mut cfg = ac_core::config::load(None).unwrap_or_default();
    cfg.session = Some(name.to_string());
    ac_core::config::save(&cfg, None).ok();
    println!("  Created and switched to session: {name}");
}

fn list_sessions() {
    let base = session_base();
    let active = ac_core::config::load(None)
        .ok()
        .and_then(|c| c.session);
    if !base.exists() {
        println!("  No sessions.");
        return;
    }
    let mut entries: Vec<String> = std::fs::read_dir(&base)
        .ok()
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_type().ok().map_or(false, |ft| ft.is_dir()))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    entries.sort();
    if entries.is_empty() {
        println!("  No sessions.");
        return;
    }
    println!();
    for name in &entries {
        let marker = if active.as_deref() == Some(name.as_str()) {
            " *"
        } else {
            ""
        };
        let dir = session_dir(name);
        let n_files = std::fs::read_dir(&dir)
            .ok()
            .map(|rd| rd.count())
            .unwrap_or(0);
        println!("  {name}{marker}  ({n_files} files)");
    }
    println!();
}

fn use_session(name: &str) {
    let dir = session_dir(name);
    if !dir.exists() {
        eprintln!("  error: session {name:?} not found");
        std::process::exit(1);
    }
    let mut cfg = ac_core::config::load(None).unwrap_or_default();
    cfg.session = Some(name.to_string());
    ac_core::config::save(&cfg, None).ok();
    println!("  Switched to session: {name}");
}

fn rm_session(name: &str) {
    let dir = session_dir(name);
    if !dir.exists() {
        eprintln!("  error: session {name:?} not found");
        std::process::exit(1);
    }
    std::fs::remove_dir_all(&dir).ok();
    let cfg = ac_core::config::load(None).unwrap_or_default();
    if cfg.session.as_deref() == Some(name) {
        let mut cfg = cfg;
        cfg.session = None;
        ac_core::config::save(&cfg, None).ok();
    }
    println!("  Removed session: {name}");
}

fn diff_sessions(_a: &str, _b: &str) {
    eprintln!("  session diff: not yet implemented in Rust client");
    std::process::exit(1);
}
