//! Headless `transfer_stream` client, for rig sessions with no display.
//!
//! `ac transfer` launches `ac-view`, so the only way to run a transfer
//! session has been an interactive GUI or an ad-hoc Python client. Neither
//! works over SSH on a rig, and the Python route needs `pyzmq` installed on
//! the machine under test. This is the same conversation in a binary that
//! builds from the tree already there.
//!
//! It starts a session `drivable` (silent), raises drive through `set_drive`,
//! keeps the 1500 ms dead-man fed at the documented 250 ms, records every
//! frame as JSON Lines, and drops drive before it stops. Emission stops on
//! every exit path, including the error ones.
//!
//! ```text
//! transfer_probe --pairs 2,2;0,2 --seconds 20 --drive-dbfs -30 --out run.jsonl
//! ```
//!
//! Drive is opt-in: without `--drive-dbfs` the session is passive and never
//! opens an output port, per `transfer_stream`'s own default.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// The dead-man is 1500 ms and the documented resend is 250 ms. Do not
/// change one without the other — ZMQ.md calls the 6x margin load-bearing.
const KEEPALIVE_EVERY: Duration = Duration::from_millis(250);

struct Args {
    host: String,
    ctrl_port: u16,
    data_port: u16,
    pairs: Vec<(u32, u32)>,
    seconds: f64,
    drive_dbfs: Option<f64>,
    out: Option<String>,
}

fn parse_args() -> Args {
    let mut a = Args {
        host: "127.0.0.1".to_string(),
        ctrl_port: 5556,
        data_port: 5557,
        pairs: Vec::new(),
        seconds: 20.0,
        drive_dbfs: None,
        out: None,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let key = argv[i].as_str();
        let mut val = || {
            i += 1;
            argv.get(i)
                .unwrap_or_else(|| panic!("{key} needs a value"))
                .clone()
        };
        match key {
            "--host" => a.host = val(),
            "--ctrl-port" => a.ctrl_port = val().parse().expect("--ctrl-port"),
            "--data-port" => a.data_port = val().parse().expect("--data-port"),
            "--seconds" => a.seconds = val().parse().expect("--seconds"),
            "--drive-dbfs" => a.drive_dbfs = Some(val().parse().expect("--drive-dbfs")),
            "--out" => a.out = Some(val()),
            "--pairs" => {
                for p in val().split(';').filter(|s| !s.is_empty()) {
                    let (m, r) = p.split_once(',').unwrap_or_else(|| {
                        panic!("--pairs wants meas,ref pairs separated by ';', got {p:?}")
                    });
                    a.pairs.push((
                        m.trim().parse().expect("meas channel"),
                        r.trim().parse().expect("ref channel"),
                    ));
                }
            }
            other => panic!("unknown argument {other:?}"),
        }
        i += 1;
    }
    assert!(!a.pairs.is_empty(), "--pairs is required");
    a
}

struct Client {
    req: zmq::Socket,
    sub: zmq::Socket,
}

impl Client {
    fn new(a: &Args) -> Self {
        let ctx = zmq::Context::new();
        let req = ctx.socket(zmq::REQ).unwrap();
        req.set_linger(0).unwrap();
        req.set_rcvtimeo(5_000).unwrap();
        req.set_sndtimeo(5_000).unwrap();
        req.connect(&format!("tcp://{}:{}", a.host, a.ctrl_port))
            .expect("connect CTRL");

        let sub = ctx.socket(zmq::SUB).unwrap();
        sub.set_linger(0).unwrap();
        sub.set_subscribe(b"").unwrap();
        sub.connect(&format!("tcp://{}:{}", a.host, a.data_port))
            .expect("connect DATA");
        std::thread::sleep(Duration::from_millis(200));
        Self { req, sub }
    }

    fn call(&self, cmd: Value) -> Value {
        self.req
            .send(serde_json::to_vec(&cmd).unwrap(), 0)
            .expect("CTRL send");
        let bytes = self.req.recv_bytes(0).expect("CTRL recv");
        serde_json::from_slice(&bytes).expect("CTRL decode")
    }

    /// Non-blocking-ish read of one PUB message, split into topic and payload.
    fn recv_frame(&self, timeout_ms: i32) -> Option<(String, Value)> {
        self.sub.set_rcvtimeo(timeout_ms).ok();
        let bytes = self.sub.recv_bytes(0).ok()?;
        let split = bytes.iter().position(|&b| b == b' ')?;
        let topic = String::from_utf8_lossy(&bytes[..split]).to_string();
        let payload: Value = serde_json::from_slice(&bytes[split + 1..]).ok()?;
        Some((topic, payload))
    }

    /// Drop drive and stop the session. Called on every exit path — a probe
    /// that leaves a loudspeaker driven because it panicked is the one
    /// failure mode that matters more than the measurement.
    fn shutdown(&self, drove: bool) {
        if drove {
            let _ = self.call(json!({"cmd": "set_drive", "on": false, "level_dbfs": -120.0}));
        }
        let _ = self.call(json!({"cmd": "stop"}));
    }
}

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    })
}

/// Per-pair tally. Locked and unlocked frames are counted separately because
/// a median over both is a median over two different quantities.
#[derive(Default)]
struct Tally {
    frames: usize,
    locked: usize,
    delay_samples: Vec<f64>,
    delay_ms: Vec<f64>,
    meas_peak: Vec<f64>,
    ref_peak: Vec<f64>,
    prominence: Vec<f64>,
}

fn main() {
    let args = parse_args();
    let c = Client::new(&args);

    let pairs: Vec<Value> = args.pairs.iter().map(|(m, r)| json!([m, r])).collect();
    let start_req = json!({
        "cmd":      "transfer_stream",
        "pairs":    pairs,
        "drivable": args.drive_dbfs.is_some(),
    });
    let ack = c.call(start_req);
    if ack["ok"] != json!(true) {
        panic!("transfer_stream rejected: {ack}");
    }
    eprintln!("session up: {ack}");

    let mut writer = args
        .out
        .as_ref()
        .map(|p| BufWriter::new(File::create(p).expect("open --out")));

    let mut drove = false;
    let mut last_ka = Instant::now() - KEEPALIVE_EVERY;
    let deadline = Instant::now() + Duration::from_secs_f64(args.seconds);
    let mut tallies: std::collections::BTreeMap<(i64, i64), Tally> = Default::default();
    let mut errors: Vec<String> = Vec::new();

    while Instant::now() < deadline {
        if let Some(level) = args.drive_dbfs {
            if last_ka.elapsed() >= KEEPALIVE_EVERY {
                let r = c.call(json!({"cmd": "set_drive", "on": true, "level_dbfs": level}));
                if r["ok"] != json!(true) {
                    c.shutdown(drove);
                    panic!("set_drive rejected: {r}");
                }
                if !drove {
                    // The reply echoes the *applied* level after the server
                    // clamp to drive_max_dbfs. Print it: a clamp is success,
                    // and the number that reached the interface is the one
                    // the session record needs.
                    eprintln!("drive on, applied level_dbfs = {}", r["level_dbfs"]);
                    drove = true;
                }
                last_ka = Instant::now();
            }
        }

        let Some((topic, v)) = c.recv_frame(50) else {
            continue;
        };
        if topic == "error" {
            let msg = v["message"].as_str().unwrap_or("(no message)").to_string();
            eprintln!("daemon error: {msg}");
            errors.push(msg);
            continue;
        }
        if v["type"] != json!("transfer_stream") {
            continue;
        }

        let m = v["meas_channel"].as_i64().unwrap_or(-1);
        let r = v["ref_channel"].as_i64().unwrap_or(-1);
        let t = tallies.entry((m, r)).or_default();
        t.frames += 1;
        if let Some(p) = v["meas_peak_dbfs"].as_f64() {
            t.meas_peak.push(p);
        }
        if let Some(p) = v["ref_peak_dbfs"].as_f64() {
            t.ref_peak.push(p);
        }
        if v["delay_locked"] == json!(true) {
            t.locked += 1;
            if let Some(d) = v["delay_samples"].as_f64() {
                t.delay_samples.push(d);
            }
            if let Some(d) = v["delay_ms"].as_f64() {
                t.delay_ms.push(d);
            }
            if let Some(p) = v["delay_evidence"]["prominence"].as_f64() {
                t.prominence.push(p);
            }
        }

        if let Some(w) = writer.as_mut() {
            // Keep the whole delay_evidence object, candidates included:
            // #251 wants the full candidate list, and a probe that trims it
            // is a probe that has to be re-run.
            let row = json!({
                "t":                  now_unix(),
                "meas_channel":       v["meas_channel"],
                "ref_channel":        v["ref_channel"],
                "delay_samples":      v["delay_samples"],
                "delay_ms":           v["delay_ms"],
                "delay_locked":       v["delay_locked"],
                "delay_attempts":     v["delay_attempts"],
                "speed_of_sound_m_s": v["speed_of_sound_m_s"],
                "meas_peak_dbfs":     v["meas_peak_dbfs"],
                "ref_peak_dbfs":      v["ref_peak_dbfs"],
                "delay_evidence":     v["delay_evidence"],
                "drive":              v["drive"],
            });
            writeln!(w, "{row}").expect("write --out");
        }
    }

    c.shutdown(drove);
    if let Some(mut w) = writer {
        w.flush().expect("flush --out");
    }

    println!("\n--- transfer_probe summary ---");
    println!("seconds requested: {}", args.seconds);
    match args.drive_dbfs {
        Some(l) => println!("drive requested:   {l} dBFS"),
        None => println!("drive requested:   none (passive session)"),
    }
    for ((m, r), t) in &tallies {
        let mut ds = t.delay_samples.clone();
        let mut dm = t.delay_ms.clone();
        let mut mp = t.meas_peak.clone();
        let mut rp = t.ref_peak.clone();
        let mut pr = t.prominence.clone();
        println!(
            "pair ({m},{r}): {} frames, {} locked ({:.1}%)",
            t.frames,
            t.locked,
            100.0 * t.locked as f64 / t.frames.max(1) as f64
        );
        println!(
            "    delay median: {} samples, {} ms   prominence median: {}",
            median(&mut ds).map_or("—".into(), |v| format!("{v:.0}")),
            median(&mut dm).map_or("—".into(), |v| format!("{v:.4}")),
            median(&mut pr).map_or("—".into(), |v| format!("{v:.1}")),
        );
        println!(
            "    peaks median: meas {} dBFS, ref {} dBFS",
            median(&mut mp).map_or("—".into(), |v| format!("{v:.2}")),
            median(&mut rp).map_or("—".into(), |v| format!("{v:.2}")),
        );
        // Spread matters more than centre here: a delay that wanders is a
        // different finding from one that is wrong and steady.
        if let (Some(lo), Some(hi)) = (
            t.delay_samples.iter().cloned().reduce(f64::min),
            t.delay_samples.iter().cloned().reduce(f64::max),
        ) {
            println!("    delay range:  {lo:.0} … {hi:.0} samples");
        }
    }
    if !errors.is_empty() {
        println!("errors seen: {}", errors.len());
        for e in errors.iter().take(5) {
            println!("    {e}");
        }
    }
}
