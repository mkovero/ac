//! GPIO handler for USB2GPIO (Arduino Mega2560).
//!
//! USB2GPIO binary serial protocol (115200 8N1):
//!   MCU→Host: [0xAA][pin][value][ts_lo][ts_hi]  (5 bytes)
//!     value=0 = button pressed (active-low INPUT_PULLUP)
//!   Host→MCU: [0x55][cmd][pin][value]           (4 bytes)
//!     cmd=1 → SET_OUTPUT (value: 0=LOW, 1=HIGH)
//!     cmd=2 → SET_MODE   (value: 0=INPUT w/ pullup, 1=OUTPUT)
//!
//! Spawned by main() when `--gpio <port>` is passed.
//! Connects back to the daemon's own ZMQ ports for command dispatch.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Pin assignments
// ---------------------------------------------------------------------------

const INPUT_PINS:  &[u8] = &[2, 3, 20, 21];
const OUTPUT_PINS: &[u8] = &[13, 14, 15];

const PIN_STOP:     u8 = 2;
const PIN_GEN_SINE: u8 = 3;
const PIN_GEN_PINK: u8 = 20;
const PIN_LED_SINE: u8 = 11;
const PIN_LED_PINK: u8 = 10;
const PIN_LED_BUSY: u8 = 13;

const CMD_SET_OUTPUT: u8 = 1;
const CMD_SET_MODE:   u8 = 2;
const MODE_INPUT:     u8 = 0;   // INPUT with pullup
const MODE_OUTPUT:    u8 = 1;

// ---------------------------------------------------------------------------
// Event type
// ---------------------------------------------------------------------------

enum GpioEvent {
    /// Button pressed: (pin, value) — value=0 means pressed (active-low)
    Button(u8, u8),
    /// Worker finished or errored: cmd name
    WorkerDone(String),
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Spawn the GPIO handler in a background thread. Returns immediately.
pub fn spawn(serial_path: String, ctrl_port: u16, data_port: u16) {
    std::thread::spawn(move || {
        if let Err(e) = run(&serial_path, ctrl_port, data_port) {
            eprintln!("gpio: {e:#}");
        }
    });
}

// ---------------------------------------------------------------------------
// Main handler logic
// ---------------------------------------------------------------------------

fn run(serial_path: &str, ctrl_port: u16, data_port: u16) -> anyhow::Result<()> {
    let read_port = serialport::new(serial_path, 115_200)
        .timeout(Duration::from_millis(100))
        .open()?;
    eprintln!("gpio: opened {serial_path}");

    let write_port: Arc<Mutex<Box<dyn serialport::SerialPort>>> =
        Arc::new(Mutex::new(read_port.try_clone()?));

    // Configure pins
    for &p in INPUT_PINS {
        serial_write(&write_port, [0x55, CMD_SET_MODE, p, MODE_INPUT]);
    }
    for &p in OUTPUT_PINS {
        serial_write(&write_port, [0x55, CMD_SET_MODE, p, MODE_OUTPUT]);
        serial_write(&write_port, [0x55, CMD_SET_OUTPUT, p, 0]);
    }

    // ZMQ — REQ for commands, SUB for DATA frames
    let zmq_ctx = zmq::Context::new();

    let mut req = ReqClient::new(zmq_ctx.clone(), format!("tcp://127.0.0.1:{ctrl_port}"))?;

    let sub = zmq_ctx.socket(zmq::SUB)?;
    sub.set_subscribe(b"")?;
    sub.set_linger(0)?;
    sub.connect(&format!("tcp://127.0.0.1:{data_port}"))?;

    // Fetch initial config for startup diagnostic; event_processor refreshes
    // the level on every button press anyway.
    let out_channel = fetch_output_channel(&mut req);
    eprintln!("gpio: level={:.2} dBFS  out_ch={}", resolve_level(&mut req), out_channel);

    let (ev_tx, ev_rx): (Sender<GpioEvent>, Receiver<GpioEvent>) = bounded(128);

    // Serial reader thread (takes ownership of read_port)
    {
        let tx = ev_tx.clone();
        std::thread::spawn(move || serial_reader(read_port, tx));
    }

    // ZMQ SUB watcher thread
    {
        let tx = ev_tx.clone();
        std::thread::spawn(move || sub_watcher(sub, tx));
    }

    // Event processor (runs on this thread)
    event_processor(ev_rx, req, write_port, out_channel);

    Ok(())
}

// ---------------------------------------------------------------------------
// Serial helpers
// ---------------------------------------------------------------------------

fn serial_write(port: &Arc<Mutex<Box<dyn serialport::SerialPort>>>, bytes: [u8; 4]) {
    use std::io::Write;
    if let Ok(mut p) = port.lock() {
        let _ = p.write_all(&bytes);
    }
}

// ---------------------------------------------------------------------------
// Frame parser (pure, no I/O)
// ---------------------------------------------------------------------------

/// Parse complete 5-byte MCU→Host frames from `buf`, draining consumed bytes.
/// Returns `(pin, value)` pairs. Partial frames stay in `buf` for the next call.
fn parse_frames(buf: &mut Vec<u8>) -> Vec<(u8, u8)> {
    let mut events = Vec::new();
    loop {
        let Some(start) = buf.iter().position(|&b| b == 0xAA) else {
            buf.clear();
            break;
        };
        if start > 0 { buf.drain(..start); }
        if buf.len() < 5 { break; }

        let pin   = buf[1];
        let value = buf[2];
        buf.drain(..5);
        events.push((pin, value));
    }
    events
}

// ---------------------------------------------------------------------------
// Thread: serial reader
// ---------------------------------------------------------------------------

fn serial_reader(mut port: Box<dyn serialport::SerialPort>, tx: Sender<GpioEvent>) {
    use std::io::Read;
    let mut buf = Vec::<u8>::new();
    let mut tmp = [0u8; 64];

    loop {
        match port.read(&mut tmp) {
            Ok(0) | Err(_) => {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }

        for (pin, value) in parse_frames(&mut buf) {
            let _ = tx.try_send(GpioEvent::Button(pin, value));
        }
    }
}

// ---------------------------------------------------------------------------
// Thread: ZMQ SUB watcher
// ---------------------------------------------------------------------------

fn sub_watcher(sub: zmq::Socket, tx: Sender<GpioEvent>) {
    loop {
        let bytes = match sub.recv_bytes(0) {
            Ok(b)  => b,
            Err(_) => break,
        };
        let space = match bytes.iter().position(|&b| b == b' ') {
            Some(i) => i,
            None    => continue,
        };
        let topic = std::str::from_utf8(&bytes[..space]).unwrap_or("").to_string();
        if topic != "done" && topic != "error" { continue; }

        let frame: Value = match serde_json::from_slice(&bytes[space + 1..]) {
            Ok(v)  => v,
            Err(_) => continue,
        };
        let cmd = frame.get("cmd").and_then(Value::as_str).unwrap_or("").to_string();
        let _ = tx.try_send(GpioEvent::WorkerDone(cmd));
    }
}

// ---------------------------------------------------------------------------
// Event processor (main GPIO thread after setup)
// ---------------------------------------------------------------------------

fn event_processor(
    rx:          Receiver<GpioEvent>,
    mut req:     ReqClient,
    write_port:  Arc<Mutex<Box<dyn serialport::SerialPort>>>,
    out_channel: u32,
) {
    let mut sine_active = false;
    let mut pink_active = false;
    let mut level: f64;

    loop {
        let ev = match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(e)  => e,
            Err(_) => continue,
        };

        match ev {
            GpioEvent::WorkerDone(cmd) => {
                match cmd.as_str() {
                    "generate"      => { sine_active = false; }
                    "generate_pink" => { pink_active = false; }
                    _ => {}
                }
                update_leds(&write_port, sine_active, pink_active);
            }

            GpioEvent::Button(pin, value) => {
                if value != 0 { continue; } // active-low; ignore release

                match pin {
                    p if p == PIN_STOP => {
                        eprintln!("gpio: STOP");
                        req.call(json!({"cmd": "stop"}));
                        sine_active = false;
                        pink_active = false;
                        update_leds(&write_port, false, false);
                    }

                    p if p == PIN_GEN_SINE => {
                        if !sine_active {
                            if pink_active {
                                eprintln!("gpio: stopping pink before sine");
                                req.call(json!({"cmd": "stop", "name": "generate_pink"}));
                                pink_active = false;
                            }
                            level = resolve_level(&mut req);
                            eprintln!("gpio: SINE @ {level:.2} dBFS ch {out_channel}");
                            sine_active = true;
                            update_leds(&write_port, true, false);
                            let ack = req.call(json!({
                                "cmd":        "generate",
                                "freq_hz":    1000.0,
                                "level_dbfs": level,
                                "channels":   [out_channel],
                            }));
                            if !ack.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                                eprintln!("gpio: generate failed: {ack}");
                                sine_active = false;
                                update_leds(&write_port, false, pink_active);
                            }
                        }
                    }

                    p if p == PIN_GEN_PINK => {
                        if !pink_active {
                            if sine_active {
                                eprintln!("gpio: stopping sine before pink");
                                req.call(json!({"cmd": "stop", "name": "generate"}));
                                sine_active = false;
                            }
                            level = resolve_level(&mut req);
                            eprintln!("gpio: PINK @ {level:.2} dBFS ch {out_channel}");
                            pink_active = true;
                            update_leds(&write_port, false, true);
                            let ack = req.call(json!({
                                "cmd":        "generate_pink",
                                "level_dbfs": level,
                                "channels":   [out_channel],
                            }));
                            if !ack.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                                eprintln!("gpio: generate_pink failed: {ack}");
                                pink_active = false;
                                update_leds(&write_port, sine_active, false);
                            }
                        }
                    }

                    _ => {} // pin 21 reserved, ignore
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ReqClient — REQ socket wrapper that rebuilds on send/recv failure so one
// slow daemon response does not wedge the button pipeline until restart.
// ---------------------------------------------------------------------------

struct ReqClient {
    ctx:      zmq::Context,
    endpoint: String,
    sock:     zmq::Socket,
}

impl ReqClient {
    fn new(ctx: zmq::Context, endpoint: String) -> anyhow::Result<Self> {
        let sock = Self::connect(&ctx, &endpoint)?;
        Ok(Self { ctx, endpoint, sock })
    }

    fn connect(ctx: &zmq::Context, endpoint: &str) -> anyhow::Result<zmq::Socket> {
        let s = ctx.socket(zmq::REQ)?;
        s.set_linger(0)?;
        s.set_rcvtimeo(2000)?;
        s.set_sndtimeo(2000)?;
        s.connect(endpoint)?;
        Ok(s)
    }

    fn reset(&mut self) {
        match Self::connect(&self.ctx, &self.endpoint) {
            Ok(s)  => self.sock = s,
            Err(e) => eprintln!("gpio: REQ reconnect failed: {e}"),
        }
    }

    fn call(&mut self, cmd: Value) -> Value {
        let bytes = match serde_json::to_vec(&cmd) {
            Ok(b)  => b,
            Err(e) => { eprintln!("gpio: json encode: {e}"); return json!({"ok": false}); }
        };
        if self.sock.send(bytes, 0).is_err() {
            eprintln!("gpio: REQ send failed — resetting socket");
            self.reset();
            return json!({"ok": false});
        }
        match self.sock.recv_bytes(0) {
            Ok(reply) => serde_json::from_slice(&reply).unwrap_or(json!({"ok": false})),
            Err(_)    => {
                eprintln!("gpio: REQ recv timeout — resetting socket");
                self.reset();
                json!({"ok": false})
            }
        }
    }
}

/// Resolve 0 dBu in dBFS from calibration, or fall back to -20 dBFS.
fn resolve_level(req: &mut ReqClient) -> f64 {
    let ack = req.call(json!({"cmd": "get_calibration"}));
    if let Some(vrms) = ack.get("vrms_at_0dbfs_out").and_then(Value::as_f64) {
        let vrms_ref = 0.7745966692_f64; // 0 dBu
        let dbfs = 20.0 * (vrms_ref / vrms).log10();
        dbfs.clamp(-60.0, -0.5)
    } else {
        eprintln!("gpio: calibration unavailable, using -20 dBFS");
        -20.0
    }
}

/// Read output_channel from daemon config.
fn fetch_output_channel(req: &mut ReqClient) -> u32 {
    let ack = req.call(json!({"cmd": "setup", "update": {}}));
    ack.get("config")
        .and_then(|c| c.get("output_channel"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32
}

// ---------------------------------------------------------------------------
// LED control
// ---------------------------------------------------------------------------

fn update_leds(port: &Arc<Mutex<Box<dyn serialport::SerialPort>>>, sine: bool, pink: bool) {
    let busy = sine || pink;
    serial_write(port, [0x55, CMD_SET_OUTPUT, PIN_LED_SINE, sine as u8]);
    serial_write(port, [0x55, CMD_SET_OUTPUT, PIN_LED_PINK, pink as u8]);
    serial_write(port, [0x55, CMD_SET_OUTPUT, PIN_LED_BUSY, busy as u8]);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(pin: u8, value: u8) -> Vec<u8> {
        vec![0xAA, pin, value, 0x00, 0x00]
    }

    // ---- Well-formed frames ----

    #[test]
    fn single_press() {
        let mut buf = frame(PIN_STOP, 0);
        let evs = parse_frames(&mut buf);
        assert_eq!(evs, vec![(PIN_STOP, 0)]);
        assert!(buf.is_empty());
    }

    #[test]
    fn single_release() {
        let mut buf = frame(PIN_GEN_SINE, 1);
        let evs = parse_frames(&mut buf);
        assert_eq!(evs, vec![(PIN_GEN_SINE, 1)]);
        assert!(buf.is_empty());
    }

    #[test]
    fn timestamp_bytes_ignored() {
        let mut buf = vec![0xAA, PIN_STOP, 0, 0xDE, 0xAD];
        let evs = parse_frames(&mut buf);
        assert_eq!(evs, vec![(PIN_STOP, 0)]);
    }

    // ---- Partial frame buffering ----

    #[test]
    fn partial_frame_retained() {
        let mut buf = vec![0xAA, PIN_STOP, 0];
        let evs = parse_frames(&mut buf);
        assert!(evs.is_empty());
        assert_eq!(buf, vec![0xAA, PIN_STOP, 0]);
    }

    #[test]
    fn partial_then_complete() {
        let mut buf = vec![0xAA, PIN_STOP, 0];
        assert!(parse_frames(&mut buf).is_empty());

        buf.extend_from_slice(&[0x00, 0x00]);
        let evs = parse_frames(&mut buf);
        assert_eq!(evs, vec![(PIN_STOP, 0)]);
        assert!(buf.is_empty());
    }

    // ---- Corrupted / garbage ----

    #[test]
    fn garbage_before_sync_discarded() {
        let mut buf = vec![0x01, 0x02, 0xFF];
        buf.extend_from_slice(&frame(PIN_GEN_PINK, 0));
        let evs = parse_frames(&mut buf);
        assert_eq!(evs, vec![(PIN_GEN_PINK, 0)]);
        assert!(buf.is_empty());
    }

    #[test]
    fn no_sync_clears_buffer() {
        let mut buf = vec![0x01, 0x02, 0x03, 0x04, 0x05];
        let evs = parse_frames(&mut buf);
        assert!(evs.is_empty());
        assert!(buf.is_empty());
    }

    #[test]
    fn lone_sync_retained() {
        let mut buf = vec![0xAA];
        let evs = parse_frames(&mut buf);
        assert!(evs.is_empty());
        assert_eq!(buf, vec![0xAA]);
    }

    // ---- Multi-frame bursts ----

    #[test]
    fn two_frames_in_one_read() {
        let mut buf = frame(PIN_STOP, 0);
        buf.extend_from_slice(&frame(PIN_GEN_SINE, 0));
        let evs = parse_frames(&mut buf);
        assert_eq!(evs, vec![(PIN_STOP, 0), (PIN_GEN_SINE, 0)]);
        assert!(buf.is_empty());
    }

    #[test]
    fn three_frames_with_garbage_between() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&frame(2, 0));
        buf.extend_from_slice(&[0xFF, 0x00]);
        buf.extend_from_slice(&frame(3, 0));
        buf.extend_from_slice(&[0x42]);
        buf.extend_from_slice(&frame(20, 1));
        let evs = parse_frames(&mut buf);
        assert_eq!(evs, vec![(2, 0), (3, 0), (20, 1)]);
        assert!(buf.is_empty());
    }

    // ---- All known pins ----

    #[test]
    fn all_input_pins_parsed() {
        let mut buf = Vec::new();
        for &pin in INPUT_PINS {
            buf.extend_from_slice(&frame(pin, 0));
        }
        let evs = parse_frames(&mut buf);
        let pins: Vec<u8> = evs.iter().map(|&(p, _)| p).collect();
        assert_eq!(pins, INPUT_PINS);
    }
}
