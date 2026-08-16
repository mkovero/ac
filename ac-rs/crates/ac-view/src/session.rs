//! Session lifecycle (deliverable 3): launch `transfer_stream` with
//! per-channel weighting/integration params set once at start (D10 —
//! parameter-static in V1, no live toggling), clean stop on quit, and
//! sane behavior on daemon disconnect: show state, don't crash, don't
//! spin.

use std::time::{Duration, Instant};

use ac_core::visualize::weighting_curves::WeightingCurve;
use anyhow::{bail, Result};
use serde_json::json;

use crate::zmq_client::{Client, Recv};

/// A frame stale enough that the UI should stop assuming the session
/// is still healthy and show a disconnected state instead of quietly
/// waiting forever. Chosen well above `transfer_stream`'s own ~2.5 s
/// iteration period (`ZMQ.md`) so normal jitter never trips it.
const DISCONNECT_AFTER: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    NoSession,
    Live,
    /// A session was launched but no frame has arrived recently — shown
    /// distinctly from `NoSession` so the user knows a session exists
    /// and isn't producing data, rather than looking identical to
    /// "nothing started yet."
    Disconnected,
}

/// Malformed frames logged before going quiet. A corrupt publisher must not
/// turn into an unbounded log stream, but the first few have to be visible or
/// the failure is as silent as the bug this replaced.
const MALFORMED_LOG_LIMIT: u64 = 5;

/// The two DATA frame types [`Session::poll_frame`] returns (#286).
/// Tagged rather than merged into one struct: a `transfer_stream` frame
/// and its `visualize/ir` sidecar are independent JSON objects on the
/// wire (`ZMQ.md`), and merging them here would mean this layer decides
/// how they relate — that fusion belongs to `ac-scene`, one layer up.
#[derive(Debug, Clone)]
pub enum PolledFrame {
    Transfer(serde_json::Value),
    Ir(serde_json::Value),
}

pub struct Session {
    client: Client,
    launched: bool,
    last_frame_at: Option<Instant>,
    malformed_frames: u64,
}

impl Session {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            launched: false,
            last_frame_at: None,
            malformed_frames: 0,
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Launch a single-pair `transfer_stream` session (M3's V1 scope —
    /// multi-pair is a later milestone). Weighting/integration are set
    /// here only; nothing in this crate re-sends them mid-session.
    pub fn launch(
        &mut self,
        meas_channel: u32,
        ref_channel: u32,
        weighting: WeightingCurve,
        integration: &str,
    ) -> Result<()> {
        self.client.drain_pending();
        let reply = self.client.call(&json!({
            "cmd": "transfer_stream",
            // Drivable but silent: the daemon connects its output at launch
            // so a later `set_drive` actually reaches the interface, while
            // `drive` stays unset so the session comes up emitting nothing.
            "drivable": true,
            "meas_channel": meas_channel,
            "ref_channel": ref_channel,
            "weighting": weighting.tag(),
            "integration": integration,
        }))?;
        if reply["ok"] != serde_json::Value::Bool(true) {
            bail!(
                "transfer_stream failed: {}",
                reply["error"].as_str().unwrap_or("unknown error")
            );
        }
        self.launched = true;
        self.last_frame_at = None;
        Ok(())
    }

    pub fn stop(&mut self) {
        if self.launched {
            let _ = self.client.call(&json!({"cmd": "stop"}));
            self.launched = false;
        }
    }

    /// Send a `set_drive` (§4.3). Best-effort: a failed send surfaces as
    /// the daemon's dead-man dropping the drive, never a crash — this is
    /// the stimulus path and must degrade safely, not panic. `level_dbfs`
    /// is already clamped client-side by the stimulus machine; the server
    /// clamps again as the authoritative backstop.
    pub fn set_drive(&self, on: bool, level_dbfs: f64) {
        let _ = self.client.call(&json!({
            "cmd": "set_drive",
            "on": on,
            "level_dbfs": level_dbfs,
        }));
    }

    /// Poll for the next DATA frame this crate consumes — either a
    /// `transfer_stream` frame or its `visualize/ir` sidecar (#286) —
    /// non-blocking beyond `timeout`. Records arrival time for
    /// [`Self::connection_state`] — this is the only place "are we still
    /// connected" gets decided, so the rest of the app doesn't each
    /// invent its own guess.
    ///
    /// **`None` means the socket is empty, not "the next frame wasn't mine"**
    /// (issue #219). The DATA socket is interleaved: a live session publishes
    /// `keepalive`, `data`/`transfer_stream` and `data`/`visualize/ir`, the
    /// last of these once per transfer frame and immediately behind it. This
    /// used to return `None` for any non-`transfer_stream` frame, which ended
    /// the caller's drain loop after exactly one transfer frame however deep
    /// the backlog was — measured at 1 surfaced out of 75 available after a
    /// 2 s stall against a real daemon. So a frame of either matched type is
    /// returned, tagged by [`PolledFrame`] rather than merged into one
    /// struct — the two arrive as distinct wire frames on distinct
    /// schedules (the sidecar is once per transfer tick, not every tick
    /// necessarily carries one), and `ac-scene` fuses them into one
    /// panel's traces, not this layer.
    ///
    /// Any other frame type is still skipped rather than reported as
    /// end-of-stream: `ac-view` has no consumer for it, so it would
    /// otherwise be thrown away one call later anyway.
    pub fn poll_frame(&mut self, timeout: Duration) -> Option<PolledFrame> {
        loop {
            match self.client.recv_frame(timeout) {
                Recv::Empty => return None,
                // Not end-of-stream. Report it and keep draining — a frame we
                // could not read says nothing about whether more are queued.
                Recv::Malformed(why) => {
                    self.malformed_frames += 1;
                    if self.malformed_frames <= MALFORMED_LOG_LIMIT {
                        eprintln!(
                            "ac-view: discarding malformed DATA frame ({why}); \
                             {} so far",
                            self.malformed_frames
                        );
                    }
                }
                Recv::Frame(topic, v) => {
                    if topic != "data" {
                        continue;
                    }
                    if v["type"] == "transfer_stream" {
                        self.last_frame_at = Some(Instant::now());
                        return Some(PolledFrame::Transfer(v));
                    }
                    if v["type"] == "visualize/ir" {
                        self.last_frame_at = Some(Instant::now());
                        return Some(PolledFrame::Ir(v));
                    }
                }
            }
        }
    }

    /// Malformed DATA frames seen this session. Exposed so a test can assert
    /// they were counted rather than mistaken for an empty socket.
    pub fn malformed_frames(&self) -> u64 {
        self.malformed_frames
    }

    pub fn connection_state(&self) -> ConnectionState {
        if !self.launched {
            return ConnectionState::NoSession;
        }
        match self.last_frame_at {
            Some(t) if t.elapsed() < DISCONNECT_AFTER => ConnectionState::Live,
            Some(_) => ConnectionState::Disconnected,
            // Launched but no frame yet — the first ~2.5s iteration is
            // still in flight, not a disconnect.
            None => ConnectionState::Live,
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // connection_state's decision logic is pure enough to test without
    // a real socket by constructing the timing directly — the ZMQ
    // integration itself is covered by tests/it_live_end_to_end.rs
    // against a real fake-audio daemon.
    #[test]
    fn connection_state_transitions() {
        // Can't construct a real Session without a socket; this test
        // documents and locks the DISCONNECT_AFTER threshold's
        // ordering relative to transfer_stream's own iteration period
        // instead (2.5s at 48kHz per ZMQ.md), which is the invariant
        // that actually matters: the threshold must never be tighter
        // than one iteration, or a healthy session would flap to
        // "disconnected" between frames.
        let iteration_period = Duration::from_millis(2_500);
        assert!(
            DISCONNECT_AFTER > iteration_period * 2,
            "disconnect threshold must clear at least 2 healthy iteration periods"
        );
    }
}
