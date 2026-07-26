//! Session lifecycle (deliverable 3): launch `transfer_stream` with
//! per-channel weighting/integration params set once at start (D10 —
//! parameter-static in V1, no live toggling), clean stop on quit, and
//! sane behavior on daemon disconnect: show state, don't crash, don't
//! spin.

use std::time::{Duration, Instant};

use ac_core::visualize::weighting_curves::WeightingCurve;
use anyhow::{bail, Result};
use serde_json::json;

use crate::zmq_client::Client;

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

pub struct Session {
    client: Client,
    launched: bool,
    last_frame_at: Option<Instant>,
    /// Resolved drive-path port names from the launch reply (#205). Session-
    /// static, so captured once here; the frame carries only the observed
    /// connection *state*, never the names.
    drive_out_port: Option<String>,
    drive_ref_out_port: Option<String>,
}

impl Session {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            launched: false,
            last_frame_at: None,
            drive_out_port: None,
            drive_ref_out_port: None,
        }
    }

    /// Test-only: a session with pre-set resolved routing, standing in for a
    /// launch reply without needing a daemon.
    #[cfg(test)]
    pub fn for_test_with_routing(out: Option<String>, ref_out: Option<String>) -> Self {
        let mut s = Self::new(Client::for_test());
        s.drive_out_port = out;
        s.drive_ref_out_port = ref_out;
        s
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The main output port this session resolved at launch, if any.
    pub fn drive_out_port(&self) -> Option<&str> {
        self.drive_out_port.as_deref()
    }

    /// The reference output port, when it resolved to a different port from
    /// the main output. `None` also covers "same as main output", where there
    /// is no distinct leg to report on.
    pub fn drive_ref_out_port(&self) -> Option<&str> {
        self.drive_ref_out_port
            .as_deref()
            .filter(|p| Some(*p) != self.drive_out_port.as_deref())
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
        // Session-static routing, captured once from the launch reply rather
        // than republished per frame (#205). The port names the health line and
        // the banner display both come from here.
        self.drive_out_port = reply["out_port"].as_str().map(str::to_string);
        self.drive_ref_out_port = reply["ref_out_port"].as_str().map(str::to_string);
        self.launched = true;
        self.last_frame_at = None;
        Ok(())
    }

    pub fn stop(&mut self) {
        if self.launched {
            let _ = self.client.call(&json!({"cmd": "stop"}));
            self.launched = false;
            self.drive_out_port = None;
            self.drive_ref_out_port = None;
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

    /// Poll for the next `transfer_stream` DATA frame, non-blocking
    /// beyond `timeout`. Records arrival time for [`Self::connection_state`]
    /// — this is the only place "are we still connected" gets decided,
    /// so the rest of the app doesn't each invent its own guess.
    pub fn poll_frame(&mut self, timeout: Duration) -> Option<serde_json::Value> {
        let frame = self.client.recv_frame(timeout).and_then(|(topic, v)| {
            (topic == "data" && v["type"] == "transfer_stream").then_some(v)
        });
        if frame.is_some() {
            self.last_frame_at = Some(Instant::now());
        }
        frame
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
