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
        data.set_subscribe(b"").context("subscribing to all topics")?;
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
            Ok(Ok(s)) => serde_json::from_str(&s).ok(),
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
