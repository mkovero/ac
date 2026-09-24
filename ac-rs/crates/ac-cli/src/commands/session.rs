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
    std::path::PathBuf::from(home).join(".local/share/ac/sessions")
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
    // #513: read the config before touching the directory, and undo the
    // directory if the save is refused, so a refusal leaves nothing behind
    // and a retry of the same name is not blocked by "already exists".
    let mut cfg = load_or_refuse();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "  error: cannot create session directory {}: {e}",
            dir.display()
        );
        std::process::exit(1);
    }
    cfg.session = Some(name.to_string());
    if let Err(e) = ac_core::config::save(&cfg, None) {
        // `remove_dir` only removes an empty directory: the name was free a
        // moment ago, so this can only undo what this command created.
        std::fs::remove_dir(&dir).ok();
        refuse(&e);
    }
    println!("  Created and switched to session: {name}");
}

/// Config as it is on disk, or exit 1 with the `session not saved` refusal
/// (#513). A file that cannot be read is refused rather than replaced by
/// defaults: defaults would make the active session look unset.
fn load_or_refuse() -> ac_core::config::Config {
    match ac_core::config::load(None) {
        Ok(cfg) => cfg,
        Err(source) => refuse(&ac_core::config::SaveError::Unreadable {
            path: ac_core::config::default_config_path(),
            source,
        }),
    }
}

/// Print the `session not saved` refusal to stderr and exit 1 (#513).
fn refuse(e: &ac_core::config::SaveError) -> ! {
    eprintln!("  error: {}", e.not_saved_message("session"));
    std::process::exit(1);
}

fn list_sessions() {
    let base = session_base();
    let active = ac_core::config::load(None).ok().and_then(|c| c.session);
    if !base.exists() {
        println!("  No sessions.");
        return;
    }
    let mut entries: Vec<String> = std::fs::read_dir(&base)
        .ok()
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_type().ok().is_some_and(|ft| ft.is_dir()))
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
    let mut cfg = load_or_refuse();
    cfg.session = Some(name.to_string());
    if let Err(e) = ac_core::config::save(&cfg, None) {
        refuse(&e);
    }
    println!("  Switched to session: {name}");
}

fn rm_session(name: &str) {
    let dir = session_dir(name);
    if !dir.exists() {
        eprintln!("  error: session {name:?} not found");
        std::process::exit(1);
    }
    // #513: clear the active session first; the directory is deleted only
    // once the config no longer names it (or never did), so a refusal
    // deletes nothing.
    let mut cfg = load_or_refuse();
    if cfg.session.as_deref() == Some(name) {
        cfg.session = None;
        if let Err(e) = ac_core::config::save(&cfg, None) {
            refuse(&e);
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    println!("  Removed session: {name}");
}

fn diff_sessions(_a: &str, _b: &str) {
    eprintln!("  session diff: not yet implemented in Rust client");
    std::process::exit(1);
}
