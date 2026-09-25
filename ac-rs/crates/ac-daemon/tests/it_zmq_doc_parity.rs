//! `ZMQ.md` ↔ daemon parity.
//!
//! `ZMQ.md` states the wire contract in prose. For the first-cut DATA frames
//! (#112) the code states it once, in `ac_core::wire`, which the daemon
//! serialises and `ac-scene` / `ac-cli` deserialise; everything else is
//! still built as untyped JSON. Nothing but these tests links the document
//! to either. It has drifted before — six commands the daemon dispatched had
//! no section in it when this file was written.
//!
//! What is checked, and on what evidence:
//!
//! * **Command roster, both directions** — `ZMQ.md`'s `### `cmd`` headings
//!   against the `COMMANDS` table in `src/server.rs`. Text on both sides; no
//!   daemon runs. Catches a command added to the daemon without a spec, and a
//!   spec left behind by a removed command.
//! * **Every dispatched command refuses an unrecognised field** (#628) —
//!   against a live `--fake-audio` daemon. Safe for every command, because a
//!   refused request runs nothing.
//! * **Each section names its own command** — every `"cmd"` literal inside a
//!   section's JSON blocks must equal that section's heading. Catches a
//!   section copy-pasted from its neighbour.
//! * **Reply keys of the read-only commands** — against a live `--fake-audio`
//!   daemon.
//! * **Key sets of the four typed DATA frames** (#112) — `transfer_stream`
//!   (with its ladder settled), `visualize/ir`, `visualize/spectrum` (THD
//!   branch) and `measurement/loudness`, captured live, read through their
//!   `ac_core::wire` type and written back out. The key set, nested keys
//!   included, must equal the one in the frame's `ZMQ.md` block. A field
//!   renamed on the type changes what it writes, so this goes red even for a
//!   field no consumer reads and no compiler would flag.
//!
//! What is **not** checked, so that a green run is not read as more than it
//! is: the payloads of the untyped DATA frames, field *types*, field
//! *values*, the keys inside `cal_tags` and `delay_evidence` (raw subtrees,
//! typing them is a follow-up), and the reply of every command whose handler
//! spawns a worker or drives an output. A green run here says the roster, the
//! read-only replies and the typed frames' keys agree — nothing about whether
//! the prose is true.
//!
//! The reply check has no notion of an optional field: it asserts set
//! equality. If one of [`READ_ONLY_COMMANDS`] ever gains a reply field that is
//! only sometimes present, this test has to learn that distinction before that
//! field can be documented.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::json;

#[path = "common/mod.rs"]
mod common;

use common::{Client, Daemon};

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

/// Every name in the CTRL `COMMANDS` table in `server.rs`, read from the
/// source text: each `name: "<cmd>"` line between `const COMMANDS` and the
/// table's closing `];`. There is no runtime roster to ask for — a client can
/// only discover a command by guessing its name — so the source is the only
/// statement of what the daemon actually serves.
fn dispatched_commands(src: &str) -> BTreeSet<String> {
    let start = src
        .find("const COMMANDS")
        .expect("server.rs has a `const COMMANDS` table");
    let table = &src[start..];
    let table = &table[..table.find("\n];").expect("COMMANDS table ends with `];`")];
    let mut out = BTreeSet::new();
    for line in table.lines() {
        let Some(rest) = line.trim().strip_prefix("name: \"") else {
            continue;
        };
        if let Some((name, _)) = rest.split_once('"') {
            out.insert(name.to_string());
        }
    }
    assert!(
        out.len() > 20,
        "dispatch parse found only {} commands — the `COMMANDS` table in \
         server.rs probably changed shape, so this whole file is checking nothing",
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

/// #628: every dispatched command refuses a top-level field it does not
/// read, naming that field. Goes red for any command the check misses.
#[test]
fn every_dispatched_command_refuses_an_unrecognised_field() {
    const FIELD: &str = "zz_unrecognised_628";
    let daemon = Daemon::spawn();
    let client = Client::new(&daemon);

    let mut failures: Vec<String> = Vec::new();
    for name in dispatched_commands(&server_rs()) {
        let reply = client.call(json!({ "cmd": name, FIELD: 1 }));
        let err = reply["error"].as_str().unwrap_or_default();
        if reply["ok"] != json!(false)
            || reply["unrecognised_fields"] != json!([FIELD])
            || !err.contains(&format!("'{FIELD}'"))
        {
            failures.push(format!("{name}: {reply}"));
        }
    }
    assert!(
        failures.is_empty(),
        "commands that did not refuse `{FIELD}`:\n  {}",
        failures.join("\n  ")
    );
    let status = client.call(json!({"cmd": "status"}));
    assert_eq!(status["busy"], json!(false), "{status}");
}

// ---- typed DATA frame key parity (#112 D4.5) ----

/// Nested keys under these are not compared: the subtrees are held as raw
/// `serde_json::Value` on the shared type, so their keys are not the type's
/// statement. The top-level key itself still is.
const RAW_SUBTREES: &[&str] = &["cal_tags", "delay_evidence"];

/// The first fenced block after the first line starting with `marker`.
fn block_after(md: &str, marker: &str) -> String {
    let mut lines = md.lines().skip_while(|l| !l.starts_with(marker));
    assert!(
        lines.next().is_some(),
        "ZMQ.md has no line starting with {marker:?}"
    );
    let mut block = String::new();
    let mut inside = false;
    for line in lines {
        if line.starts_with("```") {
            if inside {
                return block;
            }
            inside = true;
            continue;
        }
        if inside {
            block.push_str(line);
            block.push('\n');
        }
    }
    panic!("no fenced block after {marker:?} in ZMQ.md");
}

/// Every key path in a JSON-ish `ZMQ.md` block: `a`, `a.b`, and `a[].b` for
/// objects inside an array. Walks text for the same reason
/// [`top_level_keys`] does — the blocks carry placeholders and comments.
fn doc_key_paths(block: &str) -> BTreeSet<String> {
    let b: Vec<char> = block.chars().collect();
    // One entry per open bracket: the path segment it contributes, if any.
    let mut stack: Vec<Option<String>> = Vec::new();
    let mut paths = BTreeSet::new();
    let skip_ws = |mut j: usize| {
        while j < b.len() && b[j].is_whitespace() {
            j += 1;
        }
        j
    };
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            '/' if b.get(i + 1) == Some(&'/') => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '{' | '[' => {
                stack.push(None);
                i += 1;
            }
            '}' | ']' => {
                stack.pop();
                i += 1;
            }
            '"' => {
                let start = i + 1;
                i = start;
                while i < b.len() && b[i] != '"' {
                    i += 1;
                }
                let key: String = b[start..i.min(b.len())].iter().collect();
                i += 1;
                let j = skip_ws(i);
                if b.get(j) != Some(&':') {
                    continue;
                }
                let prefix: Vec<&str> = stack.iter().flatten().map(String::as_str).collect();
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{key}", prefix.join("."))
                };
                paths.insert(path);
                let k = skip_ws(j + 1);
                match b.get(k) {
                    Some('{') => {
                        stack.push(Some(key));
                        i = k + 1;
                    }
                    Some('[') => {
                        stack.push(Some(format!("{key}[]")));
                        i = k + 1;
                    }
                    _ => i = j + 1,
                }
            }
            _ => i += 1,
        }
    }
    paths
}

/// Every key path in a real value, with the same `a[].b` convention.
fn value_key_paths(v: &serde_json::Value) -> BTreeSet<String> {
    fn walk(v: &serde_json::Value, prefix: &str, out: &mut BTreeSet<String>) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, child) in m {
                    let path = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    out.insert(path.clone());
                    walk(child, &path, out);
                }
            }
            serde_json::Value::Array(a) => {
                for child in a {
                    walk(child, &format!("{prefix}[]"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(v, "", &mut out);
    out
}

/// The key set the shared type `T` serialises for a live frame: the frame
/// read through `T` and written back out, so a key `T` does not name (or
/// names differently) shows up here the way it would on the wire.
fn typed_key_paths<T>(live: &serde_json::Value) -> BTreeSet<String>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let typed: T = serde_json::from_value(live.clone())
        .unwrap_or_else(|e| panic!("live frame does not parse as its shared type: {e}"));
    value_key_paths(&serde_json::to_value(&typed).expect("serialise"))
}

fn without_raw_subtrees(paths: BTreeSet<String>) -> BTreeSet<String> {
    paths
        .into_iter()
        .filter(|p| !RAW_SUBTREES.iter().any(|r| p.starts_with(&format!("{r}."))))
        .collect()
}

/// Receive DATA frames until `want` has matched one of each predicate, or the
/// deadline passes.
fn capture(
    c: &Client,
    secs: u64,
    want: &[&dyn Fn(&serde_json::Value) -> bool],
) -> Vec<Option<serde_json::Value>> {
    let mut got: Vec<Option<serde_json::Value>> = vec![None; want.len()];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline && got.iter().any(Option::is_none) {
        let Some((topic, v)) = c.recv_pub(1_000) else {
            continue;
        };
        if topic != "data" {
            continue;
        }
        for (slot, pred) in got.iter_mut().zip(want) {
            if slot.is_none() && pred(&v) {
                *slot = Some(v.clone());
            }
        }
    }
    got
}

#[test]
fn typed_data_frame_blocks_match_the_shared_types() {
    use ac_core::wire::{IrFrame, LoudnessFrame, SpectrumFrame, TransferFrame};

    let md = zmq_md();
    let daemon = Daemon::spawn();
    let c = Client::new(&daemon);

    // Monitor: the fake engine's default tone, so the THD branch — the
    // spectrum block documents that branch, with its keys marked as such.
    let r = c.call(json!({"cmd": "monitor_spectrum", "channels": [0], "interval": 0.1}));
    assert_eq!(r["ok"], json!(true), "{r}");
    let thd_spectrum = |v: &serde_json::Value| {
        v["type"] == json!("visualize/spectrum") && v.get("freq_hz").is_some()
    };
    let loudness = |v: &serde_json::Value| v["type"] == json!("measurement/loudness");
    let monitor = capture(&c, 10, &[&thd_spectrum, &loudness]);
    let _ = c.call(json!({"cmd": "stop"}));
    let _ = c.wait_for_topic("done", std::time::Duration::from_secs(5));

    // Transfer: wait for the ladder to settle so `mtw` and its nested keys
    // are present.
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "fake_correlated_pair": {"gain": 0.5, "delay_samples": 200},
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    let with_ladder =
        |v: &serde_json::Value| v["type"] == json!("transfer_stream") && v["mtw"].is_object();
    let ir = |v: &serde_json::Value| v["type"] == json!("visualize/ir");
    let transfer = capture(&c, 15, &[&with_ladder, &ir]);
    let _ = c.call(json!({"cmd": "stop"}));

    let frame = |slot: &Option<serde_json::Value>, what: &str| -> serde_json::Value {
        slot.clone()
            .unwrap_or_else(|| panic!("no live {what} frame captured"))
    };
    let cases: [(&str, &str, BTreeSet<String>); 4] = [
        (
            "visualize/spectrum",
            "### `spectrum` frame",
            typed_key_paths::<SpectrumFrame>(&frame(&monitor[0], "visualize/spectrum")),
        ),
        (
            "measurement/loudness",
            "### `measurement/loudness` frame",
            typed_key_paths::<LoudnessFrame>(&frame(&monitor[1], "measurement/loudness")),
        ),
        (
            "transfer_stream",
            "**DATA** — one frame per pair per iteration",
            typed_key_paths::<TransferFrame>(&frame(&transfer[0], "transfer_stream")),
        ),
        (
            "visualize/ir",
            "#### `visualize/ir` sidecar",
            typed_key_paths::<IrFrame>(&frame(&transfer[1], "visualize/ir")),
        ),
    ];

    let mut failures: Vec<String> = Vec::new();
    for (name, marker, typed) in cases {
        let typed = without_raw_subtrees(typed);
        let documented = without_raw_subtrees(doc_key_paths(&block_after(&md, marker)));
        let undocumented: Vec<&String> = typed.difference(&documented).collect();
        let unwritten: Vec<&String> = documented.difference(&typed).collect();
        if !undocumented.is_empty() {
            failures.push(format!(
                "{name}: the shared type writes {undocumented:?}, ZMQ.md does not document them"
            ));
        }
        if !unwritten.is_empty() {
            failures.push(format!(
                "{name}: ZMQ.md documents {unwritten:?}, the shared type does not write them"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "ZMQ.md DATA blocks disagree with ac_core::wire:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn doc_key_paths_reads_nested_objects_and_arrays_of_objects() {
    let block = r#"// topic: data
{
  "a": <int>,            // "not": a key
  "b": { "c": "x" | "y" },
  "d": [ { "e": <float> } ],
  "f": [[<float>, <float>], ...]
}"#;
    let want: BTreeSet<String> = ["a", "b", "b.c", "d", "d[].e", "f"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(doc_key_paths(block), want);
}
