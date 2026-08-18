//! Headless `plot_ir` client that reports the peak, for rig sessions.
//!
//! `ac plot ir` runs the measurement and then discards it — `run_ir` waits
//! for `done` and prints nothing, which is the gap epic #276 exists to close.
//! Until that lands there is no way to read an IR peak from the CLI, and the
//! peak is the whole quantity a latency or arrival measurement wants.
//!
//! This subscribes to the `measurement/impulse_response` frame directly and
//! reports peak index, magnitude, pre-impulse floor, SNR, and the peak's
//! offset from the window centre. Companion to `transfer_probe`: that one
//! gives `transfer_stream`'s cross-correlation delay, this one gives the
//! Farina IR peak, and comparing them on one leg is what #343 asks for.
//!
//! ```text
//! ir_probe --level-dbfs -30 --duration 2.0 --f2 16000 --window 16384
//! ```
//!
//! `--level-dbfs` is required. `plot_ir` does not apply the config's
//! `drive_max_dbfs` ceiling — only `set_drive` does — so the value passed
//! here is the only limit on what reaches the interface.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct Args {
    host: String,
    ctrl_port: u16,
    data_port: u16,
    f1: f64,
    f2: f64,
    duration: f64,
    level_dbfs: f64,
    n_harmonics: u32,
    window_len: u32,
    tail_s: f64,
    timeout_s: u64,
    /// Known interface round trip, subtracted from the peak offset so the
    /// printed arrival is comparable with a `transfer_stream` delay.
    tau_ms: Option<f64>,
}

fn parse_args() -> Args {
    let mut a = Args {
        host: "127.0.0.1".to_string(),
        ctrl_port: 5556,
        data_port: 5557,
        f1: 50.0,
        f2: 16_000.0,
        duration: 2.0,
        level_dbfs: f64::NAN,
        n_harmonics: 3,
        window_len: 16_384,
        tail_s: 0.5,
        timeout_s: 60,
        tau_ms: None,
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
            "--f1" => a.f1 = val().parse().expect("--f1"),
            "--f2" => a.f2 = val().parse().expect("--f2"),
            "--duration" => a.duration = val().parse().expect("--duration"),
            "--level-dbfs" => a.level_dbfs = val().parse().expect("--level-dbfs"),
            "--harmonics" => a.n_harmonics = val().parse().expect("--harmonics"),
            "--window" => a.window_len = val().parse().expect("--window"),
            "--tail" => a.tail_s = val().parse().expect("--tail"),
            "--timeout" => a.timeout_s = val().parse().expect("--timeout"),
            "--tau-ms" => a.tau_ms = Some(val().parse().expect("--tau-ms")),
            other => panic!("unknown argument {other:?}"),
        }
        i += 1;
    }
    assert!(
        a.level_dbfs.is_finite(),
        "--level-dbfs is required: plot_ir does not apply drive_max_dbfs, so \
         this value is the only ceiling on what reaches the interface"
    );
    a
}

fn main() {
    let a = parse_args();
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

    let cmd = json!({
        "cmd":         "plot_ir",
        "f1_hz":       a.f1,
        "f2_hz":       a.f2,
        "duration":    a.duration,
        "level_dbfs":  a.level_dbfs,
        "tail_s":      a.tail_s,
        "n_harmonics": a.n_harmonics,
        "window_len":  a.window_len,
    });
    req.send(serde_json::to_vec(&cmd).unwrap(), 0)
        .expect("CTRL send");
    let ack: Value =
        serde_json::from_slice(&req.recv_bytes(0).expect("CTRL recv")).expect("CTRL decode");
    if ack["ok"] != json!(true) {
        panic!("plot_ir rejected: {ack}");
    }
    eprintln!("plot_ir accepted at {} dBFS, waiting…", a.level_dbfs);

    let deadline = Instant::now() + Duration::from_secs(a.timeout_s);
    let frame = loop {
        if Instant::now() > deadline {
            panic!("timeout waiting for measurement/impulse_response");
        }
        sub.set_rcvtimeo(1_000).ok();
        let Ok(bytes) = sub.recv_bytes(0) else {
            continue;
        };
        let Some(split) = bytes.iter().position(|&b| b == b' ') else {
            continue;
        };
        let topic = String::from_utf8_lossy(&bytes[..split]).to_string();
        let Ok(v) = serde_json::from_slice::<Value>(&bytes[split + 1..]) else {
            continue;
        };
        if topic == "error" {
            panic!(
                "daemon error: {}",
                v["message"].as_str().unwrap_or("(no message)")
            );
        }
        if topic == "measurement/impulse_response" {
            break v;
        }
    };

    let data = &frame["data"];
    let sr = data["sample_rate_hz"].as_f64().unwrap_or(f64::NAN);
    let ir: Vec<f64> = data["linear_ir"]
        .as_array()
        .expect("linear_ir array")
        .iter()
        .filter_map(Value::as_f64)
        .collect();
    assert!(ir.len() > 64, "linear_ir suspiciously short: {}", ir.len());

    let (peak_idx, peak_abs) = ir.iter().enumerate().fold((0usize, 0.0f64), |acc, (i, v)| {
        let m = v.abs();
        if m > acc.1 {
            (i, m)
        } else {
            acc
        }
    });
    let far_end = ir.len() / 8;
    let floor = ir[..far_end]
        .iter()
        .map(|v| v.abs())
        .fold(0.0_f64, f64::max);
    let snr_db = 20.0 * (peak_abs / floor.max(1e-15)).log10();

    let centre = ir.len() / 2;
    let offset = peak_idx as i64 - centre as i64;
    let offset_ms = offset as f64 * 1000.0 / sr;

    println!("--- ir_probe record ---");
    println!("sample_rate:   {sr} Hz");
    println!(
        "sweep:         {} Hz – {} Hz, {} s, {} dBFS, {} harm, {} tail",
        a.f1, a.f2, a.duration, a.level_dbfs, a.n_harmonics, a.tail_s
    );
    println!(
        "window:        requested {}, ir len {}{}",
        a.window_len,
        ir.len(),
        if (ir.len() as u32) < a.window_len {
            "  ← CLAMPED to the order-2 IR gap"
        } else {
            ""
        }
    );
    println!("peak_index:    {peak_idx}");
    println!("peak_abs:      {peak_abs:.6e}");
    println!("floor_abs:     {floor:.6e}  (max |x| over leading {far_end} samples)");
    println!("snr_db:        {snr_db:.2}");
    println!("peak_offset:   {offset:+} samples from centre ({centre}) = {offset_ms:+.4} ms");

    // The peak's distance from either window edge decides whether the number
    // above is a measurement or an artefact: an arrival outside the window
    // pins the peak against the end and still returns something plausible.
    let edge = peak_idx.min(ir.len().saturating_sub(1) - peak_idx);
    println!(
        "edge_margin:   {edge} samples to the nearest window edge{}",
        if edge < 64 {
            "  ← TOO CLOSE, treat the peak as pinned, not measured"
        } else {
            ""
        }
    );

    // Onset, not peak. On a multi-way loudspeaker the largest sample in a
    // band-limited deconvolution is not the arrival: LF and crossover group
    // delay pull the maximum later than the wavefront that actually left the
    // baffle first. Distance wants the onset. Report where the IR first
    // crosses a few fractions of the peak, searching backward from the peak
    // so a later reflection cannot be mistaken for the start.
    println!("--- onset ---");
    for frac in [0.5_f64, 0.25, 0.1, 0.05] {
        let thresh = frac * peak_abs;
        let mut onset = peak_idx;
        while onset > 0 && ir[onset - 1].abs() >= thresh {
            onset -= 1;
        }
        let o = onset as i64 - centre as i64;
        println!(
            "  {:>4.0}% of peak: index {onset}, offset {o:+} samples = {:+.4} ms  ({} before peak)",
            frac * 100.0,
            o as f64 * 1000.0 / sr,
            peak_idx - onset
        );
    }

    if let Some(tau) = a.tau_ms {
        println!("--- against τ ---");
        println!(
            "peak arrival:  {:+.4} ms after subtracting τ = {tau} ms",
            offset_ms - tau
        );
    }
}
