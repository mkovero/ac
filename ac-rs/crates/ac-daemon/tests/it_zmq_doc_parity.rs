//! `ZMQ.md` ↔ daemon parity.
//!
//! `ZMQ.md` is the only place the wire contract is stated. The daemon emits
//! frames inline with `json!(`, `ac-scene` parses them with typed serde,
//! `ac-cli` parses them untyped, and nothing links those three to each other
//! or to the document at compile time. The document can therefore drift from
//! the code freely, and has — six commands the daemon dispatched had no
//! section in it when this file was written. These tests are the first thing
//! in the repo that can make the document go red.
//!
//! What is checked, and on what evidence:
//!
//! * **Command roster, both directions** — `ZMQ.md`'s `### `cmd`` headings
//!   against the dispatch `match` in `src/server.rs`. Text on both sides; no
//!   daemon runs. Catches a command added to the daemon without a spec, and a
//!   spec left behind by a removed command.
//! * **Each section names its own command** — every `"cmd"` literal inside a
//!   section's JSON blocks must equal that section's heading. Catches a
//!   section copy-pasted from its neighbour.
//! * **Reply keys of the read-only commands** — against a live `--fake-audio`
//!   daemon. This is the only check here that compares the document to
//!   behaviour rather than to source text.
//!
//! What is **not** checked, so that a green run is not read as more than it
//! is: DATA frame payloads, field *types*, field *values*, and the reply of
//! every command whose handler spawns a worker or drives an output. Those need
//! a driven measurement. A green run here says the roster agrees and the
//! read-only replies agree — nothing about whether the prose is true.
//!
//! The reply check has no notion of an optional field: it asserts set
//! equality. If one of [`READ_ONLY_COMMANDS`] ever gains a reply field that is
//! only sometimes present, this test has to learn that distinction before that
//! field can be documented.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use serde_json::{json, Value};

/// Commands whose handler only reads state: no worker, no output, no device
/// mutation. Safe to call in a loop against a fake-audio daemon, and their
/// replies are the same on every call, which is what makes a set comparison
/// against the documented `**Reply**` block meaningful.
const READ_ONLY_COMMANDS: &[&str] = &[
    "status",
    "get_analysis_mode",
    "get_band_weighting",
    "get_time_integration",
    "devices",
    "list_calibrations",
    "server_connections",
    "snapshot_list",
];

// ---- document and source parsing ----

fn zmq_md() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ZMQ.md");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn server_rs() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/server.rs");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Command name → section body, for every `### `name`` under `## Commands`.
///
/// A heading counts as a command only when it is exactly one backticked
/// identifier. That is deliberate: `### `warnings` (optional, any reply)`
/// documents a reply *field*, not a command, and says so in the heading. The
/// same rule means a heading decorated with a suffix stops being recognised —
/// which turns into a roster failure rather than a silent pass.
fn documented_commands(md: &str) -> BTreeMap<String, String> {
    let lines: Vec<&str> = md.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with("## Commands"))
        .expect("ZMQ.md has a `## Commands` section");
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("## "))
        .map(|i| start + 1 + i)
        .unwrap_or(lines.len());

    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in &lines[start..end] {
        if let Some(rest) = line.strip_prefix("### ") {
            current = heading_command(rest.trim_end());
            if let Some(name) = &current {
                assert!(
                    out.insert(name.clone(), String::new()).is_none(),
                    "ZMQ.md documents `{name}` twice"
                );
            }
        } else if let Some(name) = &current {
            let body = out.get_mut(name).expect("section was inserted above");
            body.push_str(line);
            body.push('\n');
        }
    }
    out
}

/// `` `name` `` → `Some("name")`; anything else → `None`.
fn heading_command(heading: &str) -> Option<String> {
    let inner = heading.strip_prefix('`')?.strip_suffix('`')?;
    is_command_ident(inner).then(|| inner.to_string())
}

/// Whether a string is shaped like a command name. Also what separates a real
/// name from a `<placeholder>` in an example.
fn is_command_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
}

/// Every name in the CTRL dispatch `match` in `server.rs`, read from the
/// source text. There is no runtime roster to ask for — a client can only
/// discover a command by guessing its name — so the source is the only
/// statement of what the daemon actually serves.
fn dispatched_commands(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in src.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix('"') else {
            continue;
        };
        let Some((name, tail)) = rest.split_once('"') else {
            continue;
        };
        if tail.trim_start().starts_with("=> handlers::") {
            out.insert(name.to_string());
        }
    }
    assert!(
        out.len() > 20,
        "dispatch parse found only {} commands — the `match` in server.rs \
         probably changed shape, so this whole file is checking nothing",
        out.len()
    );
    out
}

/// Contents of every fenced block in a section, concatenated.
fn fenced_blocks(section: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in section.lines() {
        if line.starts_with("```") {
            inside = !inside;
            continue;
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The first fenced block following a line that opens with `**Reply`.
///
/// The label is not uniform across the document (`**Reply**`,
/// `**Reply** — same shape as …`), so only the prefix is matched.
fn reply_block(section: &str) -> Option<String> {
    let mut lines = section.lines();
    while let Some(line) = lines.next() {
        if !line.trim_start().starts_with("**Reply") {
            continue;
        }
        let mut block = String::new();
        let mut inside = false;
        for line in lines.by_ref() {
            if line.starts_with("```") {
                if inside {
                    return Some(block);
                }
                inside = true;
                continue;
            }
            if inside {
                block.push_str(line);
                block.push('\n');
            }
        }
        return None;
    }
    None
}

/// Object keys at nesting depth 1 of a JSON-ish block.
///
/// The blocks in `ZMQ.md` are illustrations, not JSON: they carry `<int>`
/// placeholders, `|` alternatives and `//` comments, so `serde_json` cannot
/// read them. This walks the text instead — enough to recover the key names,
/// which is all the reply check compares.
fn top_level_keys(block: &str) -> BTreeSet<String> {
    let b: Vec<char> = block.chars().collect();
    let mut keys = BTreeSet::new();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            '/' if b.get(i + 1) == Some(&'/') => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '{' | '[' => {
                depth += 1;
                i += 1;
            }
            '}' | ']' => {
                depth -= 1;
                i += 1;
            }
            '"' => {
                i += 1;
                let start = i;
                while i < b.len() && b[i] != '"' {
                    if b[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                let s: String = b[start..i.min(b.len())].iter().collect();
                i += 1;
                let mut j = i;
                while j < b.len() && b[j].is_whitespace() {
                    j += 1;
                }
                if depth == 1 && b.get(j) == Some(&':') {
                    keys.insert(s);
                }
            }
            _ => i += 1,
        }
    }
    keys
}

/// Every `"cmd": "…"` string literal in a chunk of JSON-ish text, keeping
/// only the identifier-shaped ones.
///
/// Placeholders are skipped: `stop` illustrates the terminal frame of whatever
/// worker it stopped as `{ "cmd": "<worker-name>" }`, which names no command
/// and must not be read as naming the wrong one.
fn cmd_literals(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(pos) = rest.find("\"cmd\"") {
        rest = &rest[pos + 5..];
        let after = rest.trim_start();
        let Some(after) = after.strip_prefix(':') else {
            continue;
        };
        let after = after.trim_start();
        let Some(after) = after.strip_prefix('"') else {
            continue;
        };
        if let Some((name, _)) = after.split_once('"') {
            if is_command_ident(name) {
                out.insert(name.to_string());
            }
        }
    }
    out
}

// ---- static checks ----

#[test]
fn every_dispatched_command_has_a_zmq_md_section() {
    let doc: BTreeSet<String> = documented_commands(&zmq_md()).into_keys().collect();
    let code = dispatched_commands(&server_rs());
    let missing: Vec<&String> = code.difference(&doc).collect();
    assert!(
        missing.is_empty(),
        "the daemon dispatches these commands but ZMQ.md has no `### `name`` \
         section for them: {missing:?}\n\
         ZMQ.md is the only statement of the wire contract, so an undocumented \
         command is a command no client can be written against."
    );
}

#[test]
fn every_documented_command_is_dispatched() {
    let doc: BTreeSet<String> = documented_commands(&zmq_md()).into_keys().collect();
    let code = dispatched_commands(&server_rs());
    let stale: Vec<&String> = doc.difference(&code).collect();
    assert!(
        stale.is_empty(),
        "ZMQ.md documents these commands but server.rs does not dispatch them: \
         {stale:?}\n\
         A section for a command that no longer exists reads as authoritative \
         right up until a client sends it and gets `unknown command`."
    );
}

#[test]
fn every_section_names_its_own_command() {
    for (name, section) in documented_commands(&zmq_md()) {
        let found = cmd_literals(&fenced_blocks(&section));
        assert!(
            !found.is_empty(),
            "ZMQ.md section `{name}` has no JSON block containing a \"cmd\" \
             literal, so nothing ties its examples to its heading"
        );
        let wrong: Vec<&String> = found.iter().filter(|c| **c != name).collect();
        assert!(
            wrong.is_empty(),
            "ZMQ.md section `{name}` contains examples for other commands: \
             {wrong:?}"
        );
    }
}

#[test]
fn read_only_commands_have_a_documented_reply() {
    let doc = documented_commands(&zmq_md());
    for name in READ_ONLY_COMMANDS {
        let section = doc
            .get(*name)
            .unwrap_or_else(|| panic!("ZMQ.md has no section for `{name}`"));
        let block = reply_block(section)
            .unwrap_or_else(|| panic!("ZMQ.md section `{name}` has no **Reply** block"));
        assert!(
            top_level_keys(&block).contains("ok"),
            "ZMQ.md `{name}` reply block has no `ok` key; every CTRL reply has \
             one (see the CTRL reply envelope section)"
        );
    }
}

// ---- live check ----

#[test]
fn documented_reply_keys_match_the_daemon() {
    let doc = documented_commands(&zmq_md());
    let daemon = Daemon::spawn();
    let client = Client::new(&daemon);

    let mut failures: Vec<String> = Vec::new();
    for name in READ_ONLY_COMMANDS {
        let section = doc
            .get(*name)
            .unwrap_or_else(|| panic!("ZMQ.md has no section for `{name}`"));
        let documented = top_level_keys(
            &reply_block(section)
                .unwrap_or_else(|| panic!("ZMQ.md section `{name}` has no **Reply** block")),
        );

        let reply = client.call(json!({ "cmd": name }));
        let live: BTreeSet<String> = reply
            .as_object()
            .unwrap_or_else(|| panic!("`{name}` reply was not a JSON object: {reply}"))
            .keys()
            .cloned()
            .collect();

        let undocumented: Vec<&String> = live.difference(&documented).collect();
        let unfulfilled: Vec<&String> = documented.difference(&live).collect();
        if !undocumented.is_empty() {
            failures.push(format!(
                "{name}: daemon sends {undocumented:?}, ZMQ.md does not document them"
            ));
        }
        if !unfulfilled.is_empty() {
            failures.push(format!(
                "{name}: ZMQ.md documents {unfulfilled:?}, daemon did not send them"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "ZMQ.md reply blocks disagree with the daemon:\n  {}",
        failures.join("\n  ")
    );
}

// ---- harness ----
//
// Deliberately a private copy of the spawn/call helpers rather than a shared
// module: each integration test is its own binary, and the alternative is a
// test-support crate that nothing else currently wants.

static PORT_CURSOR: AtomicU16 = AtomicU16::new(29_600);
static HOME_CURSOR: AtomicU32 = AtomicU32::new(0);

struct Daemon {
    child: Child,
    ctrl_port: u16,
    home: PathBuf,
}

impl Daemon {
    fn spawn() -> Self {
        let base = PORT_CURSOR.fetch_add(2, Ordering::Relaxed);
        let (ctrl, data) = (base, base + 1);

        let n = HOME_CURSOR.fetch_add(1, Ordering::Relaxed);
        let mut home = env::temp_dir();
        home.push(format!("ac-daemon-doc-parity-{}-{n}", std::process::id()));
        let _ = fs::create_dir_all(home.join(".config").join("ac"));

        let child = Command::new(env!("CARGO_BIN_EXE_ac-daemon"))
            .env("HOME", &home)
            .args([
                "--fake-audio",
                "--local",
                "--ctrl-port",
                &ctrl.to_string(),
                "--data-port",
                &data.to_string(),
            ])
            .spawn()
            .expect("spawn ac-daemon");

        let deadline = Instant::now() + Duration::from_secs(3);
        let ctx = zmq::Context::new();
        loop {
            assert!(Instant::now() <= deadline, "daemon never came up");
            thread::sleep(Duration::from_millis(50));
            let s = ctx.socket(zmq::REQ).unwrap();
            s.set_linger(0).ok();
            s.set_rcvtimeo(300).ok();
            s.set_sndtimeo(300).ok();
            if s.connect(&format!("tcp://127.0.0.1:{ctrl}")).is_err() {
                continue;
            }
            if s.send(br#"{"cmd":"status"}"#.as_ref(), 0).is_err() {
                continue;
            }
            if s.recv_bytes(0).is_ok() {
                break;
            }
        }
        Self {
            child,
            ctrl_port: ctrl,
            home,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.home);
    }
}

struct Client {
    _ctx: zmq::Context,
    req: zmq::Socket,
}

impl Client {
    fn new(d: &Daemon) -> Self {
        let ctx = zmq::Context::new();
        let req = ctx.socket(zmq::REQ).unwrap();
        req.set_linger(0).unwrap();
        req.set_rcvtimeo(3_000).unwrap();
        req.set_sndtimeo(3_000).unwrap();
        req.connect(&format!("tcp://127.0.0.1:{}", d.ctrl_port))
            .unwrap();
        Self { _ctx: ctx, req }
    }

    fn call(&self, cmd: Value) -> Value {
        self.req.send(serde_json::to_vec(&cmd).unwrap(), 0).unwrap();
        let bytes = self.req.recv_bytes(0).expect("CTRL recv");
        serde_json::from_slice(&bytes).expect("CTRL decode")
    }
}
