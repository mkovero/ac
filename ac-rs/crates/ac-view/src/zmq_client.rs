//! ZMQ client: CTRL (REQ) + DATA (SUB) against a configurable
//! `host:port` pair — no localhost hardcode (D6, remote is
//! first-class). Existing daemon commands only (architect review,
//! decision 1): `transfer_stream`, `stop`, `snapshot`,
//! `snapshot_fetch`, `snapshot_list`, `snapshot_delete`.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::time::Duration;

pub struct Endpoint {
    pub host: String,
    pub ctrl_port: u16,
    pub data_port: u16,
}

impl Endpoint {
    pub fn ctrl_url(&self) -> String {
        format!("tcp://{}:{}", self.host, self.ctrl_port)
    }
    pub fn data_url(&self) -> String {
        format!("tcp://{}:{}", self.host, self.data_port)
    }
}

/// One DATA read: a frame, a frame that could not be decoded, or nothing.
///
/// The three-way split exists because a drain loop has to tell "the socket is
/// empty, stop" from "that one was unreadable, keep going". Collapsing them
/// into `Option` — as this did — means a single malformed frame ends the drain
/// silently and intermittently, which is the same symptom as issue #219 with a
/// cause that is much harder to find.
#[derive(Debug)]
pub enum Recv {
    Frame(String, Value),
    /// A frame arrived but could not be decoded. Carries why, so the caller
    /// can report it rather than silently treating it as an empty socket.
    Malformed(&'static str),
    /// Nothing available within the timeout.
    Empty,
}

/// Decode one DATA payload. Split out from the socket read so the
/// malformed-vs-empty distinction is testable without a live socket — `Empty`
/// is a property of the socket and can never be produced here.
///
/// Wire format: a single frame `<topic> <json>` (ZMQ.md, DATA).
fn parse_frame(bytes: &[u8]) -> Recv {
    let Some(split) = bytes.iter().position(|&b| b == b' ') else {
        return Recv::Malformed("no topic separator");
    };
    let Ok(topic) = String::from_utf8(bytes[..split].to_vec()) else {
        return Recv::Malformed("topic is not utf-8");
    };
    match serde_json::from_slice(&bytes[split + 1..]) {
        Ok(payload) => Recv::Frame(topic, payload),
        Err(_) => Recv::Malformed("payload is not json"),
    }
}

/// A connected CTRL+DATA pair. Reconnecting (e.g. after a daemon
/// restart) means constructing a new `Client` — no hidden retry state
/// here, so callers control exactly what "disconnected" means for
/// their own UI state (deliverable 3: sane behavior on disconnect,
/// not a silent background reconnect loop).
pub struct Client {
    req: zmq::Socket,
    sub: zmq::Socket,
    _ctx: zmq::Context,
}

impl Client {
    /// Test-only: detached sockets, never connected. Enough to construct a
    /// [`crate::session::Session`] for wiring tests that must not talk to a
    /// daemon. Any send/recv on it simply goes nowhere.
    #[cfg(test)]
    pub fn for_test() -> Self {
        let ctx = zmq::Context::new();
        let req = ctx.socket(zmq::REQ).expect("test REQ socket");
        let sub = ctx.socket(zmq::SUB).expect("test SUB socket");
        req.set_linger(0).ok();
        sub.set_linger(0).ok();
        Self {
            req,
            sub,
            _ctx: ctx,
        }
    }

    pub fn connect(endpoint: &Endpoint) -> Result<Self> {
        let ctx = zmq::Context::new();

        let req = ctx.socket(zmq::REQ).context("create REQ socket")?;
        req.set_linger(0).ok();
        req.set_rcvtimeo(5_000).ok();
        req.set_sndtimeo(5_000).ok();
        req.connect(&endpoint.ctrl_url())
            .with_context(|| format!("connect CTRL {}", endpoint.ctrl_url()))?;

        let sub = ctx.socket(zmq::SUB).context("create SUB socket")?;
        sub.set_linger(0).ok();
        sub.set_subscribe(b"").ok();
        sub.connect(&endpoint.data_url())
            .with_context(|| format!("connect DATA {}", endpoint.data_url()))?;

        Ok(Self {
            req,
            sub,
            _ctx: ctx,
        })
    }

    /// One CTRL request/reply round trip. A REQ socket must alternate
    /// send/recv exactly — callers never issue a second `call` before
    /// this one returns.
    pub fn call(&self, cmd: &Value) -> Result<Value> {
        self.req
            .send(serde_json::to_vec(cmd)?, 0)
            .context("CTRL send")?;
        let bytes = self
            .req
            .recv_bytes(0)
            .context("CTRL recv (daemon unreachable?)")?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Non-blocking-with-timeout DATA frame receive.
    ///
    /// Returns [`Recv::Empty`] on timeout — the caller decides what "no frame
    /// arrived" means (still-connecting vs. disconnected), this layer doesn't
    /// guess. A frame that arrives but does not decode is [`Recv::Malformed`],
    /// which is **not** the same thing and must not stop a drain.
    pub fn recv_frame(&self, timeout: Duration) -> Recv {
        self.sub.set_rcvtimeo(timeout.as_millis() as i32).ok();
        match self.sub.recv_bytes(0) {
            Ok(bytes) => parse_frame(&bytes),
            // The only route to `Empty`: the socket had nothing to give.
            Err(_) => Recv::Empty,
        }
    }

    /// Drain and discard whatever's currently buffered on DATA —
    /// used before starting a new session so a stale frame from a
    /// previous one can't be mistaken for the first live frame.
    ///
    /// Keeps going past a malformed frame: this runs before a session starts,
    /// so leaving anything behind is exactly the staleness it exists to
    /// prevent.
    pub fn drain_pending(&self) {
        while !matches!(self.recv_frame(Duration::from_millis(20)), Recv::Empty) {}
    }

    /// `snapshot_fetch` reassembly loop: chunked read by offset, sha256
    /// -verified against `expected_sha256` (from the `snapshot` reply)
    /// before returning. Errors (not panics) on a mismatch — a
    /// corrupted/truncated transfer is a recoverable UI condition, not
    /// a crash.
    pub fn fetch_snapshot(&self, id: &str, expected_sha256: &str) -> Result<Vec<u8>> {
        use sha2::{Digest, Sha256};

        const CHUNK: u64 = 262_144;
        let mut out = Vec::new();
        let mut offset: u64 = 0;
        loop {
            let reply = self.call(&serde_json::json!({
                "cmd": "snapshot_fetch", "id": id, "offset": offset, "len": CHUNK,
            }))?;
            if reply["ok"] != Value::Bool(true) {
                bail!(
                    "snapshot_fetch failed: {}",
                    reply["error"].as_str().unwrap_or("unknown error")
                );
            }
            let chunk_b64 = reply["chunk_b64"]
                .as_str()
                .context("snapshot_fetch reply missing chunk_b64")?;
            let chunk = base64_decode(chunk_b64)?;
            let total_bytes = reply["total_bytes"]
                .as_u64()
                .context("snapshot_fetch reply missing total_bytes")?;
            offset += chunk.len() as u64;
            out.extend_from_slice(&chunk);
            if offset >= total_bytes {
                break;
            }
        }

        let mut hasher = Sha256::new();
        hasher.update(&out);
        let actual: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if actual != expected_sha256 {
            bail!("snapshot sha256 mismatch: expected {expected_sha256}, got {actual}");
        }
        Ok(out)
    }
}

/// Standard-alphabet base64 decoder — the daemon's `snapshot_fetch`
/// only encodes (`ac-daemon::handlers::snapshot`'s hand-rolled
/// encoder), so this is the client-side counterpart. Small enough not
/// to warrant a crate dependency, same call this codebase already made
/// for the encoder side (M1) and for test-side decoding (M1's
/// `it_snapshot.rs`).
fn base64_decode(s: &str) -> Result<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let clean: Vec<u8> = s.bytes().filter(|&b| b != b'=').collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        let vals: Vec<u8> = chunk
            .iter()
            .map(|&b| val(b).context("invalid base64 character"))
            .collect::<Result<_>>()?;
        match vals.len() {
            4 => {
                out.push((vals[0] << 2) | (vals[1] >> 4));
                out.push((vals[1] << 4) | (vals[2] >> 2));
                out.push((vals[2] << 6) | vals[3]);
            }
            3 => {
                out.push((vals[0] << 2) | (vals[1] >> 4));
                out.push((vals[1] << 4) | (vals[2] >> 2));
            }
            2 => {
                out.push((vals[0] << 2) | (vals[1] >> 4));
            }
            _ => bail!("invalid base64 length"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decode_round_trips_known_bytes() {
        // "hello world" -> base64 (verified against a standard encoder).
        let decoded = base64_decode("aGVsbG8gd29ybGQ=").unwrap();
        assert_eq!(decoded, b"hello world");
    }

    #[test]
    fn base64_decode_handles_non_padded_length() {
        // 3-byte input, no padding needed.
        let decoded = base64_decode("YWJj").unwrap();
        assert_eq!(decoded, b"abc");
    }

    #[test]
    fn endpoint_urls_have_no_localhost_hardcode() {
        let e = Endpoint {
            host: "192.168.9.40".to_string(),
            ctrl_port: 5556,
            data_port: 5557,
        };
        assert_eq!(e.ctrl_url(), "tcp://192.168.9.40:5556");
        assert_eq!(e.data_url(), "tcp://192.168.9.40:5557");
    }

    /// The distinction issue #219's fix rests on: a frame that cannot be
    /// decoded is **not** an empty socket. Before the split these all
    /// collapsed to `None`, so one corrupt payload ended a drain loop and
    /// looked exactly like "nothing more queued" — the same symptom as the
    /// type-filter bug, but intermittent and much harder to find.
    ///
    /// `Recv::Empty` is deliberately unreachable here: it is a property of the
    /// socket, not of the bytes, and that is the whole point of the split.
    #[test]
    fn malformed_payloads_are_distinguishable_from_an_empty_socket() {
        for (name, bytes) in [
            ("no separator", &b"datanospacehere"[..]),
            ("non-utf8 topic", &[0xff, 0xfe, b' ', b'{', b'}'][..]),
            ("payload not json", &b"data {this is not json"[..]),
            ("empty payload", &b"data "[..]),
        ] {
            match parse_frame(bytes) {
                Recv::Malformed(why) => assert!(!why.is_empty(), "{name}"),
                other => panic!("{name}: expected Malformed, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_well_formed_frame_decodes_to_its_topic_and_payload() {
        match parse_frame(br#"data {"type":"transfer_stream","sr":48000}"#) {
            Recv::Frame(topic, v) => {
                assert_eq!(topic, "data");
                assert_eq!(v["type"], "transfer_stream");
                assert_eq!(v["sr"], 48_000);
            }
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    /// A JSON payload containing a space must not be truncated at it — the
    /// split is on the *first* space only.
    #[test]
    fn only_the_first_space_separates_topic_from_payload() {
        match parse_frame(br#"data {"a": 1, "b": 2}"#) {
            Recv::Frame(topic, v) => {
                assert_eq!(topic, "data");
                assert_eq!(v["b"], 2);
            }
            other => panic!("expected Frame, got {other:?}"),
        }
    }
}
