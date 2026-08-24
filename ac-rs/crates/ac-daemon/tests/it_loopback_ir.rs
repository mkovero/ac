//! `#[ignore]`'d JACK-loopback integration test for `plot_ir`.
//!
//! Runs a Farina exponential sweep through the daemon's real `JackEngine`
//! with the JACK output port connected to the JACK input port, then asserts
//! the recovered linear IR has a sharp dominant peak well above the
//! pre-impulse floor.
//!
//! This test is `#[ignore]`'d so it does not run as part of `cargo test`.
//! It needs a live JACK server. See `ARCHITECTURE.md` → "Testing strategy"
//! → "Loopback IR runbook" for invocation.
//!
//! The internal loopback works because both the daemon's output and input
//! ports are registered under the same JACK client (`ac-daemon`). Setting
//! `output_port = "ac-daemon:in"` makes `JackEngine::start()` connect
//! `ac-daemon:out → ac-daemon:in` directly — no external `jack_connect`
//! and no system audio devices required (works with `jackd -d dummy`).
//!
//! # Running through real converters
//!
//! The self-loop above never leaves the daemon's own JACK client: it
//! exercises the ring, not a converter. To route the sweep through actual
//! hardware, set both of these to real JACK port names:
//!
//! ```text
//! AC_LOOPBACK_OUT="Babyface Pro Pro:playback_2"   # daemon's out connects here
//! AC_LOOPBACK_IN="Babyface Pro Pro:capture_4"     # daemon's in connects here
//! ```
//!
//! Unset, both default to the self-loop, so the `jackd -d dummy` runbook in
//! `ARCHITECTURE.md` is unchanged.
//!
//! Setting them puts a stimulus on physical outputs, which is behind the
//! rig's standing drive-level policy. So when `AC_LOOPBACK_OUT` is set,
//! `AC_LOOPBACK_LEVEL_DBFS` becomes **mandatory** and the test panics
//! without it rather than inheriting the self-loop's `-6.0`. As of #360,
//! `plot_ir` clamps its requested level to the config's `drive_max_dbfs`
//! ceiling the same way `set_drive` always has (`handlers/mod.rs`'s
//! `apply_drive_ceiling`) — so this value is a request, not the only thing
//! bounding what reaches the converter, but it should still be set
//! explicitly here rather than relying on whatever ceiling happens to be
//! configured on the box this runs against.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ac_core::measurement::sweep::SweepParams;
use serde_json::{json, Value};

static PORT_CURSOR: AtomicU16 = AtomicU16::new(25_900);
static HOME_CURSOR: AtomicU32 = AtomicU32::new(0);

fn alloc_ports() -> (u16, u16) {
    let base = PORT_CURSOR.fetch_add(2, Ordering::Relaxed);
    (base, base + 1)
}

fn alloc_home() -> PathBuf {
    let n = HOME_CURSOR.fetch_add(1, Ordering::Relaxed);
    let mut p = env::temp_dir();
    p.push(format!("ac-daemon-loopback-{}-{n}", std::process::id()));
    let _ = fs::create_dir_all(p.join(".config").join("ac"));
    p
}

/// The self-loop defaults: the daemon's own ports, under one JACK client.
const SELF_LOOP_OUT: &str = "ac-daemon:in";
const SELF_LOOP_IN: &str = "ac-daemon:out";

/// Default drive for the self-loop, where nothing physical is driven.
const SELF_LOOP_LEVEL_DBFS: f64 = -6.0;

/// Sweep duration. Not a free parameter: the linear IR's window is clamped
/// to the gap between the linear IR and the order-2 IR
/// (`per_order_window_lens`, `sweep.rs:318`/`:335`), and that gap is
/// `duration · ln 2 / ln(f2/f1)` seconds. So the window that has to contain
/// the round trip grows with the sweep, and a chain whose latency exceeds
/// half the gap puts its own peak outside the window it is measured in.
///
/// #361: at the old default (0.5 s), that half-gap is ~30 ms at 96 kHz —
/// *less* than `MAX_ROUND_TRIP_S` (60 ms) below, and less than the
/// reference rig's own measured 43.75 ms round trip (#277). `hi_bound`
/// then saturates at the window's own last sample instead of genuinely
/// being placed by `MAX_ROUND_TRIP_S`, and a peak pinned at that edge —
/// exactly what a too-short window produces — passes the position check
/// as if it were a plausible round trip. 2.0 s (the reference rig's own
/// runbook duration, ARCHITECTURE.md's "Loopback IR runbook") puts the
/// half-gap at ~120 ms, twice `MAX_ROUND_TRIP_S`, so the bound genuinely
/// binds with headroom — checked at runtime by `round_trip_bound` below
/// and pinned for the runnable sample rates by
/// `default_duration_binds_max_round_trip_at_runnable_sample_rates`.
const DEFAULT_DURATION_S: f64 = 2.0;

/// Maximum acceptable round-trip latency, in seconds. #277 measured
/// 43.75 ms on the reference rig (Babyface Pro leg, 96 kHz, 2.0 s sweep —
/// ARCHITECTURE.md's "Loopback IR runbook"). This is that figure with
/// ~37% headroom for rig-to-rig jitter, not a bound fitted to one run.
/// If a chain ever needs more than this, the number moves and this
/// comment's citation moves with it — it must never grow silently.
const MAX_ROUND_TRIP_S: f64 = 0.060;

/// Where the round-trip-latency bound sits inside a deconvolved IR window,
/// once the window has been shown large enough to hold `MAX_ROUND_TRIP_S`
/// at all.
#[derive(Debug)]
struct RoundTripBound {
    lo_bound: usize,
    hi_bound: usize,
}

impl RoundTripBound {
    /// The IR peak is the deconvolution delta. `extract_irs` centres the
    /// gate at the sweep endpoint, so the peak nominally sits at
    /// `ir_len / 2` — but JACK adds at least one period of port-to-port
    /// latency, and an external converter adds its own latency on top,
    /// shifting the peak later. Latency only ever adds delay, so the peak
    /// cannot legitimately land before centre; `lo_bound` gives a small
    /// rounding tolerance rather than requiring the peak at-or-after
    /// centre exactly.
    fn contains(&self, peak_idx: usize) -> bool {
        peak_idx > self.lo_bound && peak_idx < self.hi_bound
    }
}

/// Derive the round-trip-latency bound for a window of `ir_len` samples at
/// `sample_rate_hz`, admitting up to `max_round_trip_s` of round trip.
///
/// Refuses (rather than silently capping `hi_bound` at `ir_len - 1`) when
/// the window is too short to express `max_round_trip_s` at all. That
/// capping is exactly #361's defect: a window too short to hold the stated
/// maximum round trip makes `hi_bound` saturate at the window's own last
/// sample regardless of `max_round_trip_s`, so a peak pinned at the far
/// edge — what a too-short window actually produces — passes the position
/// check as a plausible-looking round trip instead of being refused.
fn round_trip_bound(
    ir_len: usize,
    sample_rate_hz: f64,
    max_round_trip_s: f64,
) -> Result<RoundTripBound, String> {
    let centre = ir_len / 2;
    let low_margin_samples = ((0.001 * sample_rate_hz).round() as usize).max(1);
    let hi_margin_samples = (max_round_trip_s * sample_rate_hz).round() as usize;
    let lo_bound = centre.saturating_sub(low_margin_samples);
    let hi_bound = centre + hi_margin_samples;
    let saturated_at = ir_len.saturating_sub(1);
    if hi_bound >= saturated_at {
        return Err(format!(
            "window too short to hold max_round_trip_s ({:.1} ms) at \
             {sample_rate_hz} Hz: hi_bound (centre {centre} + margin \
             {hi_margin_samples} = {hi_bound}) would saturate at ir_len - 1 \
             ({saturated_at}) instead of genuinely binding, so a peak pinned \
             at the window edge would pass the position check unrejected \
             (ir_len={ir_len})",
            max_round_trip_s * 1000.0,
        ));
    }
    Ok(RoundTripBound { lo_bound, hi_bound })
}

/// The linear IR's gate length (`window_len_used[0]`) that `extract_irs`
/// actually produces for a sweep with these parameters — order 1's gate
/// clamped down to the sample distance to order 2 (`per_order_window_lens`,
/// `sweep.rs:328`), the same computation the daemon runs, called here
/// directly rather than re-derived by hand so the two can't drift apart.
fn linear_ir_len(duration_s: f64, sample_rate_hz: f64, window_len_requested: usize) -> usize {
    let p = SweepParams {
        f1_hz: 50.0,
        f2_hz: 16_000.0,
        duration_s,
        sample_rate: sample_rate_hz as u32,
    };
    let gap_samples = (p.harmonic_time_offset_s(2) * sample_rate_hz).round() as usize;
    window_len_requested.min(gap_samples)
}

/// Where the sweep goes and where it comes back, and how hard it is driven.
struct Routing {
    output_port: String,
    input_port: String,
    level_dbfs: f64,
    duration_s: f64,
    /// True when the ports came from the environment, i.e. real hardware is
    /// in the path. Recorded so the assertions can say which chain they ran
    /// against instead of implying the self-loop.
    external: bool,
}

impl Routing {
    /// Read the routing from the environment, defaulting to the self-loop.
    ///
    /// Both port variables must be set together: half a route is a route
    /// through the wrong thing, and silently completing it with a self-loop
    /// end would produce a plausible-looking IR of the ring while the
    /// operator believed they were measuring a converter.
    fn from_env() -> Self {
        let out = env::var("AC_LOOPBACK_OUT").ok().filter(|s| !s.is_empty());
        let inp = env::var("AC_LOOPBACK_IN").ok().filter(|s| !s.is_empty());

        match (out, inp) {
            (None, None) => Self {
                output_port: SELF_LOOP_OUT.to_string(),
                input_port: SELF_LOOP_IN.to_string(),
                level_dbfs: level_from_env().unwrap_or(SELF_LOOP_LEVEL_DBFS),
                duration_s: duration_from_env(),
                external: false,
            },
            (Some(output_port), Some(input_port)) => {
                // Real ports means real emission. Refuse to pick the level.
                let level_dbfs = level_from_env().expect(
                    "AC_LOOPBACK_OUT/IN name real JACK ports, so this run drives \
                     hardware: set AC_LOOPBACK_LEVEL_DBFS explicitly. plot_ir does \
                     not apply drive_max_dbfs, so this value is the only ceiling.",
                );
                Self {
                    output_port,
                    input_port,
                    level_dbfs,
                    duration_s: duration_from_env(),
                    external: true,
                }
            }
            (out, inp) => panic!(
                "AC_LOOPBACK_OUT and AC_LOOPBACK_IN must be set together \
                 (out={out:?}, in={inp:?}); one alone would route half the \
                 signal through the daemon's own ports"
            ),
        }
    }

    /// One line naming the chain under test, for the failure messages and
    /// for the operator's record of which patch produced which number.
    fn describe(&self) -> String {
        format!(
            "{} → {} at {:.1} dBFS, {:.3} s sweep ({})",
            self.output_port,
            self.input_port,
            self.level_dbfs,
            self.duration_s,
            if self.external {
                "external ports"
            } else {
                "daemon self-loop"
            }
        )
    }
}

fn duration_from_env() -> f64 {
    match env::var("AC_LOOPBACK_DURATION_S")
        .ok()
        .filter(|s| !s.is_empty())
    {
        None => DEFAULT_DURATION_S,
        Some(raw) => raw
            .parse::<f64>()
            .unwrap_or_else(|e| panic!("AC_LOOPBACK_DURATION_S={raw:?} is not a number: {e}")),
    }
}

fn level_from_env() -> Option<f64> {
    let raw = env::var("AC_LOOPBACK_LEVEL_DBFS")
        .ok()
        .filter(|s| !s.is_empty())?;
    Some(
        raw.parse::<f64>()
            .unwrap_or_else(|e| panic!("AC_LOOPBACK_LEVEL_DBFS={raw:?} is not a number: {e}")),
    )
}

/// Pre-write `$HOME/.config/ac/config.json` so the daemon picks up the sticky
/// port names the run is routed over — by default the self-loop of the JACK
/// client (`ac-daemon:out → ac-daemon:in`).
fn write_loopback_config(home: &Path, routing: &Routing) {
    let cfg = json!({
        "device":           0,
        "output_channel":   0,
        "input_channel":    0,
        "output_port":      routing.output_port,
        "input_port":       routing.input_port,
        "dbu_ref_vrms":     0.7745966692414834,
        "range_start_hz":   20.0,
        "range_stop_hz":    20_000.0,
        "server_enabled":   false,
    });
    let path = home.join(".config").join("ac").join("config.json");
    fs::write(&path, serde_json::to_vec_pretty(&cfg).unwrap()).expect("write config");
}

struct Daemon {
    child: Child,
    ctrl_port: u16,
    data_port: u16,
    home: PathBuf,
}

impl Daemon {
    fn spawn_jack(routing: &Routing) -> Self {
        let (ctrl, data) = alloc_ports();
        let home = alloc_home();
        write_loopback_config(&home, routing);

        let bin = env!("CARGO_BIN_EXE_ac-daemon");
        let child = Command::new(bin)
            .env("HOME", &home)
            .args([
                "--local",
                "--ctrl-port",
                &ctrl.to_string(),
                "--data-port",
                &data.to_string(),
            ])
            .spawn()
            .expect("spawn ac-daemon");

        let deadline = Instant::now() + Duration::from_secs(3);
        let ctx = zmq::Context::new();
        loop {
            if Instant::now() > deadline {
                panic!("daemon never came up");
            }
            thread::sleep(Duration::from_millis(50));
            let s = ctx.socket(zmq::REQ).unwrap();
            s.set_linger(0).ok();
            s.set_rcvtimeo(300).ok();
            s.set_sndtimeo(300).ok();
            if s.connect(&format!("tcp://127.0.0.1:{ctrl}")).is_err() {
                continue;
            }
            if s.send(br#"{"cmd":"status"}"#.as_ref(), 0).is_err() {
                continue;
            }
            if s.recv_bytes(0).is_ok() {
                break;
            }
        }
        Self {
            child,
            ctrl_port: ctrl,
            data_port: data,
            home,
        }
    }

    fn ctrl_endpoint(&self) -> String {
        format!("tcp://127.0.0.1:{}", self.ctrl_port)
    }
    fn data_endpoint(&self) -> String {
        format!("tcp://127.0.0.1:{}", self.data_port)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.home);
    }
}

struct Client {
    _ctx: zmq::Context,
    req: zmq::Socket,
    sub: zmq::Socket,
}

impl Client {
    fn new(d: &Daemon) -> Self {
        let ctx = zmq::Context::new();
        let req = ctx.socket(zmq::REQ).unwrap();
        req.set_linger(0).unwrap();
        req.set_rcvtimeo(3_000).unwrap();
        req.set_sndtimeo(3_000).unwrap();
        req.connect(&d.ctrl_endpoint()).unwrap();

        let sub = ctx.socket(zmq::SUB).unwrap();
        sub.set_linger(0).unwrap();
        sub.set_rcvtimeo(10_000).unwrap();
        sub.set_subscribe(b"").unwrap();
        sub.connect(&d.data_endpoint()).unwrap();

        thread::sleep(Duration::from_millis(100));
        Self {
            _ctx: ctx,
            req,
            sub,
        }
    }

    fn call(&self, cmd: Value) -> Value {
        let raw = serde_json::to_vec(&cmd).unwrap();
        self.req.send(raw, 0).unwrap();
        let bytes = self.req.recv_bytes(0).expect("CTRL recv");
        serde_json::from_slice(&bytes).expect("CTRL decode")
    }

    fn recv_pub(&self, timeout_ms: i32) -> Option<(String, Value)> {
        self.sub.set_rcvtimeo(timeout_ms).ok();
        let bytes = self.sub.recv_bytes(0).ok()?;
        let split = bytes.iter().position(|&b| b == b' ')?;
        let topic = String::from_utf8(bytes[..split].to_vec()).ok()?;
        let payload = &bytes[split + 1..];
        let v: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
        Some((topic, v))
    }

    /// Wait for a frame on `want_topic`, or fail loudly on `error` (so missing
    /// JACK is reported with the engine's own message instead of a bare timeout).
    fn wait_for_or_error(&self, want_topic: &str, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as i32;
            match self.recv_pub(remaining.max(1)) {
                Some((t, v)) if t == want_topic => return v,
                Some((t, v)) if t == "error" => {
                    let msg = v
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("(no message)");
                    panic!("daemon error before {want_topic}: {msg}");
                }
                Some(_) => continue,
                None => panic!("timeout waiting for {want_topic}"),
            }
        }
        panic!("deadline waiting for {want_topic}");
    }
}

#[test]
#[ignore = "needs a live JACK server — see ARCHITECTURE.md"]
fn loopback_ir_recovers_sharp_peak() {
    let routing = Routing::from_env();
    let chain = routing.describe();
    eprintln!("loopback_ir: {chain}");

    let d = Daemon::spawn_jack(&routing);
    let c = Client::new(&d);

    // Sweep long enough that the round-trip-latency bound below genuinely
    // binds instead of saturating at the window's own edge (#361 —
    // `DEFAULT_DURATION_S`'s doc comment). `window_len` is sized to
    // comfortably contain both the gate centre (placed by `extract_irs`
    // at the sweep endpoint) and the JACK round-trip latency shift (one
    // JACK period for a self-connected client) plus a wide pre-impulse
    // stretch that's clear of the bandlimited-sinc skirts.
    let ack = c.call(json!({
        "cmd":        "plot_ir",
        "f1_hz":      50.0,
        "f2_hz":      16_000.0,
        "duration":   routing.duration_s,
        "level_dbfs": routing.level_dbfs,
        "tail_s":     0.2,
        "n_harmonics": 3,
        "window_len":  16_384,
    }));
    assert_eq!(ack["ok"], json!(true), "plot_ir REQ rejected: {ack}");

    let frame = c.wait_for_or_error("measurement/impulse_response", Duration::from_secs(15));
    let data = &frame["data"];
    let sample_rate_hz = data["sample_rate_hz"].as_f64();
    // `window_len_used` is an array, one entry per harmonic order (order 1
    // first) — see ZMQ.md and `sweep.rs`'s `DeconvolvedIrs::window_len_used`.
    // Index [0] is the linear IR's actual gate length, which is what the
    // peak-position bound below is derived from.
    let window_len_used = frame["window_len_used"][0].as_u64();
    let ir: Vec<f64> = data["linear_ir"]
        .as_array()
        .expect("linear_ir array")
        .iter()
        .map(|v| v.as_f64().expect("linear_ir element f64"))
        .collect();
    assert!(
        ir.len() >= 256,
        "linear_ir suspiciously short: {}",
        ir.len()
    );
    // The frame's declared gate length must match the array it describes.
    // Silently receiving a shorter `linear_ir` than `window_len_used[0]`
    // says (or vice versa) would make every bound below wrong without any
    // assertion catching it — see #341 acceptance criterion 5.
    assert_eq!(
        window_len_used,
        Some(ir.len() as u64),
        "window_len_used[0] ({window_len_used:?}) disagrees with linear_ir \
         length {} over {chain}",
        ir.len()
    );

    // Peak position and magnitude.
    let (peak_idx, peak_abs) = ir.iter().enumerate().fold((0usize, 0.0f64), |acc, (i, v)| {
        let a = v.abs();
        if a > acc.1 {
            (i, a)
        } else {
            acc
        }
    });
    // Floor: max-abs over the leading 1/8 of the IR window. With window_len
    // 16384, that's ~3000 samples ≥ 6000 samples ahead of the peak — far
    // enough that the bandlimited-sinc skirts of the delta have decayed
    // below the noise on a clean loopback.
    let far_end = ir.len() / 8;
    let floor = ir[..far_end]
        .iter()
        .map(|v| v.abs())
        .fold(0.0_f64, f64::max);
    let snr_db = 20.0 * (peak_abs / floor.max(1e-15)).log10();

    // The record the run exists to produce (#277): the three numbers, plus
    // the peak's offset from the window centre, which is the round-trip
    // latency of *this* chain and nothing else. Printed before *every*
    // assertion — rig time is expensive and a failure that has to be
    // characterised is a failure whose numbers you need. An assertion that
    // fires first and takes the numbers with it costs another run.
    let centre = ir.len() / 2;
    let offset = peak_idx as i64 - centre as i64;
    eprintln!("--- loopback_ir record ---");
    eprintln!("chain:        {chain}");
    eprintln!(
        "sample_rate:  {}",
        match sample_rate_hz {
            Some(sr) => format!("{sr} Hz"),
            None => "(absent from frame)".to_string(),
        }
    );
    eprintln!(
        "window_len:   {} (ir len), frame window_len_used {}",
        ir.len(),
        match window_len_used {
            Some(w) => w.to_string(),
            None => "(absent)".to_string(),
        }
    );
    eprintln!("peak_index:   {peak_idx}");
    eprintln!("peak_abs:     {peak_abs:.6e}");
    eprintln!("floor_abs:    {floor:.6e}  (max |x| over leading {far_end} samples)");
    eprintln!("snr_db:       {snr_db:.2}");
    eprintln!(
        "peak_offset:  {offset:+} samples from centre ({}){}",
        centre,
        match sample_rate_hz {
            Some(sr) if sr > 0.0 => format!(" = {:+.4} ms", offset as f64 * 1000.0 / sr),
            _ => String::new(),
        }
    );
    eprintln!("--------------------------");

    assert!(
        peak_abs > 0.0,
        "all-zero IR — loopback never delivered audio over {chain}"
    );

    // The window this bound divides is `ir.len()` — the gate length
    // `extract_irs` actually returned (checked equal to `window_len_used[0]`
    // above), not the `window_len` requested above. `per_order_window_lens`
    // clamps it to the gap between the linear IR and the order-2 IR
    // (`sweep.rs:318`, `:335`), which is `duration · ln 2 / ln(f2/f1)`
    // seconds. See #277/#341 for the measured consequence of dividing the
    // wrong one, and #361 for what happens when that gap is too small to
    // hold `MAX_ROUND_TRIP_S` at all: `round_trip_bound` refuses outright
    // (panicking below) rather than silently capping `hi_bound` at the
    // window's own last sample and admitting an edge-pinned peak.
    let sample_rate_hz = sample_rate_hz.unwrap_or_else(|| {
        panic!("sample_rate_hz absent from frame over {chain}; can't derive the round-trip bound")
    });
    let bound = round_trip_bound(ir.len(), sample_rate_hz, MAX_ROUND_TRIP_S)
        .unwrap_or_else(|e| panic!("{e} over {chain}"));
    assert!(
        bound.contains(peak_idx),
        "peak at index {peak_idx} outside expected range [{}, {}] \
         (window_len={}, centre={centre}, max_round_trip={:.1} ms) over {chain}; \
         deconvolution may have failed",
        bound.lo_bound,
        bound.hi_bound,
        ir.len(),
        MAX_ROUND_TRIP_S * 1000.0,
    );

    // SNR floor. `jackd -d dummy` is a bit-exact digital loopback — no
    // physical noise at all — so its SNR is a ceiling set purely by
    // deconvolution/windowing artefacts, not a noise floor a converter could
    // beat. Measured on that bit-exact path at these sweep parameters
    // (#277/#341): 33.37 dB at 48 kHz, 28.90 dB at 96 kHz. The gate sits
    // below the lower of those with ~4 dB margin for run-to-run jitter. Real
    // hardware with a longer sweep measures higher still — 36.89 dB on a
    // clean electrical loopback with a 2.0 s sweep (#277) — so this floor
    // stays reachable everywhere, not just on dummy.
    const SNR_FLOOR_DB: f64 = 25.0;
    assert!(
        snr_db >= SNR_FLOOR_DB,
        "loopback IR floor too high over {chain}: peak={peak_abs:.3e}, \
         far_max={floor:.3e}, SNR={snr_db:.1} dB (need ≥ {SNR_FLOOR_DB} dB)"
    );

    // Drain the trailing report + done frames so Drop doesn't race on shutdown.
    let _ = c.recv_pub(2_000);
    let _ = c.recv_pub(2_000);
}

/// Plain unit tests over the round-trip-latency bound math, not `#[ignore]`d
/// — no JACK server needed, so these run under plain `cargo test` and catch
/// a regression to #361's failure mode without a rig.
#[cfg(test)]
mod round_trip_bound_tests {
    use super::*;

    /// #361's own reproduced failure: at the old default (0.5 s), the
    /// window is too short to hold `MAX_ROUND_TRIP_S`, so `hi_bound` would
    /// have saturated at `ir_len - 1` and silently accepted the edge-pinned
    /// peak the rig actually reported (`peak_index 5709` of `ir_len 5768`,
    /// 96 kHz — see the issue's rig record). The guard must refuse to
    /// produce a bound at all — the pinned value must never come back as a
    /// validated round trip.
    #[test]
    fn refuses_window_too_short_to_hold_max_round_trip() {
        let ir_len = 5768;
        let sample_rate_hz = 96_000.0;
        let pinned_peak_idx = 5709; // #361's own reported, wrongly-accepted peak

        let result = round_trip_bound(ir_len, sample_rate_hz, MAX_ROUND_TRIP_S);
        assert!(
            result.is_err(),
            "a window too short to hold MAX_ROUND_TRIP_S must be refused, not \
             silently produce a bound the edge-pinned peak {pinned_peak_idx} \
             would pass"
        );
    }

    /// With a window genuinely large enough to hold `MAX_ROUND_TRIP_S`, a
    /// peak pinned at the window's far edge — the shape #277/#340/#361 all
    /// measured — must still be refused by the position bound itself, not
    /// accepted because the window happens to be long. Same shape as
    /// `calibrate.rs`'s `check_peak_within_window_refuses_peak_pinned_at_edge`
    /// (#340), one layer up.
    #[test]
    fn refuses_peak_pinned_at_far_edge_even_in_a_binding_window() {
        let ir_len = 200_000; // generously larger than MAX_ROUND_TRIP_S needs
        let sample_rate_hz = 96_000.0;
        let bound = round_trip_bound(ir_len, sample_rate_hz, MAX_ROUND_TRIP_S)
            .expect("this window is sized to hold MAX_ROUND_TRIP_S");
        let pinned_peak_idx = ir_len - 1; // pinned at the far edge
        let pinned_offset_samples = pinned_peak_idx as i64 - (ir_len / 2) as i64;

        assert!(
            !bound.contains(pinned_peak_idx),
            "peak pinned at the window edge must be refused, not accepted as \
             offset {pinned_offset_samples}"
        );
    }

    /// The rig's own measured round trip (#277/#340: 43.75 ms, 4200 samples
    /// at 96 kHz) must clear the position bound in a genuinely binding
    /// window — not just a dead-centre synthetic peak.
    #[test]
    fn accepts_the_rigs_measured_round_trip() {
        let ir_len = 200_000;
        let sample_rate_hz = 96_000.0;
        let bound = round_trip_bound(ir_len, sample_rate_hz, MAX_ROUND_TRIP_S)
            .expect("this window is sized to hold MAX_ROUND_TRIP_S");
        let centre = ir_len / 2;
        let peak_idx = centre + 4200; // rig's measured round trip, 96 kHz (#277/#340)

        assert!(
            bound.contains(peak_idx),
            "the rig's own measured round trip must be inside the accepted window"
        );
    }

    /// `DEFAULT_DURATION_S` must genuinely bind `MAX_ROUND_TRIP_S` at the
    /// sample rates this repo can actually exercise the loopback test at:
    /// `jackd -d dummy` self-loop configs (48 kHz) and the rig's own
    /// Babyface Pro leg (96 kHz — ARCHITECTURE.md's "Loopback IR runbook").
    /// Regression guard for #361's acceptance criterion: `hi_bound` must
    /// not saturate at any configuration that actually runs.
    #[test]
    fn default_duration_binds_max_round_trip_at_runnable_sample_rates() {
        for sample_rate_hz in [48_000.0, 96_000.0] {
            let ir_len = linear_ir_len(DEFAULT_DURATION_S, sample_rate_hz, 16_384);
            let result = round_trip_bound(ir_len, sample_rate_hz, MAX_ROUND_TRIP_S);
            assert!(
                result.is_ok(),
                "DEFAULT_DURATION_S ({DEFAULT_DURATION_S} s) does not bind \
                 MAX_ROUND_TRIP_S at {sample_rate_hz} Hz (ir_len={ir_len}): {result:?}"
            );
        }
    }
}
