//! Blocking REQ client for sending commands to `ac-daemon`.
//!
//! Used by the Transfer layout to start/stop the daemon's `transfer_stream`
//! worker. Kept intentionally dumb: one socket, one short RCV timeout, no
//! reconnect loop — if the daemon is down, commands just fail and the UI
//! stays on the last frame it received.

use serde_json::Value;

pub struct CtrlClient {
    socket: zmq::Socket,
}

impl CtrlClient {
    pub fn connect(endpoint: &str) -> anyhow::Result<Self> {
        let ctx = zmq::Context::new();
        let socket = ctx.socket(zmq::REQ)?;
        socket.set_sndtimeo(1000)?;
        socket.set_rcvtimeo(2000)?;
        socket.set_linger(0)?;
        socket.connect(endpoint)?;
        log::info!("ctrl client connected to {endpoint}");
        Ok(Self { socket })
    }

    pub fn send(&self, cmd: &Value) -> anyhow::Result<Value> {
        let body = serde_json::to_vec(cmd)?;
        self.socket.send(body, 0)?;
        let reply = self.socket.recv_bytes(0)?;
        let val: Value = serde_json::from_slice(&reply)?;
        Ok(val)
    }
}
