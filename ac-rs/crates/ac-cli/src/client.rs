use anyhow::{Context, Result};

const DEFAULT_TIMEOUT_MS: i32 = 5000;

pub struct AcClient {
    ctx: zmq::Context,
    ctrl: zmq::Socket,
    data: zmq::Socket,
    host: String,
    ctrl_port: u16,
}

impl AcClient {
    pub fn new(host: &str, ctrl_port: u16, data_port: u16) -> Result<Self> {
        let ctx = zmq::Context::new();

        let ctrl = ctx.socket(zmq::REQ).context("creating CTRL socket")?;
        ctrl.set_rcvtimeo(DEFAULT_TIMEOUT_MS)
            .context("setting CTRL timeout")?;
        ctrl.set_linger(0).context("setting CTRL linger")?;
        let ctrl_addr = format!("tcp://{host}:{ctrl_port}");
        ctrl.connect(&ctrl_addr)
            .with_context(|| format!("connecting CTRL to {ctrl_addr}"))?;

        let data = ctx.socket(zmq::SUB).context("creating DATA socket")?;
        data.set_subscribe(b"")
            .context("subscribing to all topics")?;
        data.set_linger(0).context("setting DATA linger")?;
        let data_addr = format!("tcp://{host}:{data_port}");
        data.connect(&data_addr)
            .with_context(|| format!("connecting DATA to {data_addr}"))?;

        Ok(Self {
            ctx,
            ctrl,
            data,
            host: host.to_string(),
            ctrl_port,
        })
    }

    pub fn send_cmd(
        &mut self,
        cmd: &serde_json::Value,
        timeout_ms: Option<i32>,
    ) -> Option<serde_json::Value> {
        let payload = serde_json::to_string(cmd).ok()?;
        if self.ctrl.send(&payload, 0).is_err() {
            return None;
        }
        let timeout = timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        self.ctrl.set_rcvtimeo(timeout).ok();
        match self.ctrl.recv_string(0) {
            Ok(Ok(s)) => serde_json::from_str(&s)
                .ok()
                .map(|reply| annotate_unrecognised(reply, &self.host, self.ctrl_port)),
            _ => {
                self.reconnect_ctrl();
                None
            }
        }
    }

    pub fn recv_data(&self, timeout_ms: i64) -> Option<(String, serde_json::Value)> {
        self.data.set_rcvtimeo(timeout_ms as i32).ok();
        match self.data.recv_string(0) {
            Ok(Ok(raw)) => {
                let (topic, json_str) = raw.split_once(' ')?;
                let value: serde_json::Value = serde_json::from_str(json_str).ok()?;
                Some((topic.to_string(), value))
            }
            _ => None,
        }
    }

    fn reconnect_ctrl(&mut self) {
        let addr = format!("tcp://{}:{}", self.host, self.ctrl_port);
        drop(std::mem::replace(
            &mut self.ctrl,
            self.ctx.socket(zmq::REQ).unwrap(),
        ));
        self.ctrl.set_rcvtimeo(DEFAULT_TIMEOUT_MS).ok();
        self.ctrl.set_linger(0).ok();
        self.ctrl.connect(&addr).ok();
    }
}

/// #628: an unrecognised-field refusal gets a `daemon  <endpoint>` line under
/// the daemon's text, so every path that prints `error` also says which
/// daemon refused. Any other reply is returned unchanged.
fn annotate_unrecognised(
    mut reply: serde_json::Value,
    host: &str,
    ctrl_port: u16,
) -> serde_json::Value {
    if reply.get("unrecognised_fields").is_none() {
        return reply;
    }
    if let Some(err) = reply.get_mut("error") {
        if let Some(text) = err.as_str() {
            *err = serde_json::Value::String(format!(
                "{text}\n         daemon  tcp://{host}:{ctrl_port}"
            ));
        }
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::annotate_unrecognised;
    use serde_json::json;

    #[test]
    fn unrecognised_refusal_gains_the_daemon_line() {
        let reply = json!({
            "ok": false,
            "error": "monitor_spectrum: field 'interval_ms' not recognised \u{2014} command not run\n         reads   interval",
            "unrecognised_fields": ["interval_ms"],
            "accepted_fields": ["interval"],
        });
        let out = annotate_unrecognised(reply, "127.0.0.1", 5556);
        assert_eq!(
            out["error"],
            json!(
                "monitor_spectrum: field 'interval_ms' not recognised \u{2014} command not run\n\
                 \x20        reads   interval\n\
                 \x20        daemon  tcp://127.0.0.1:5556"
            )
        );
        assert_eq!(out["unrecognised_fields"], json!(["interval_ms"]));
    }

    #[test]
    fn other_replies_are_unchanged() {
        for reply in [
            json!({"ok": false, "error": "busy: plot running — send stop first"}),
            json!({"ok": true, "busy": false}),
            json!({"ok": false, "error": "unknown command: 'x'"}),
        ] {
            assert_eq!(annotate_unrecognised(reply.clone(), "h", 1), reply);
        }
    }
}
