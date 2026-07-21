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

    /// Non-blocking-with-timeout DATA frame receive. Returns `None` on
    /// timeout — the caller decides what "no frame arrived" means
    /// (still-connecting vs. disconnected), this layer doesn't guess.
    pub fn recv_frame(&self, timeout: Duration) -> Option<(String, Value)> {
        self.sub.set_rcvtimeo(timeout.as_millis() as i32).ok();
        let bytes = self.sub.recv_bytes(0).ok()?;
        let split = bytes.iter().position(|&b| b == b' ')?;
        let topic = String::from_utf8(bytes[..split].to_vec()).ok()?;
        let payload: Value = serde_json::from_slice(&bytes[split + 1..]).ok()?;
        Some((topic, payload))
    }

    /// Drain and discard whatever's currently buffered on DATA —
    /// used before starting a new session so a stale frame from a
    /// previous one can't be mistaken for the first live frame.
    pub fn drain_pending(&self) {
        while self.recv_frame(Duration::from_millis(20)).is_some() {}
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
}
