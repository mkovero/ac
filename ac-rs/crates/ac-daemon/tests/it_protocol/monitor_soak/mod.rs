//! Temporal soak for the `monitor_spectrum` pipeline (#189) — the daemon-side
//! home of the I5 soak that lived in `ac-ui --headless-test` until the ac-ui
//! detach.
//!
//! I1-I4 elsewhere are single-snapshot checks: settle, read one frame,
//! judge. They are structurally blind to a bug with onset delay — ring
//! wrap, EMA state poisoning, a cadence-boundary slip, an LF band that keeps
//! updating at the wrong rate. This soak runs seeded broadband noise through
//! a `--fake-audio` daemon for longer than every internal buffer period and
//! judges **every** published frame (see [`checker`] for the invariants and
//! the frame-count clock).
//!
//! Everything the soak's timing depends on comes from the daemon's own
//! `monitor_spectrum` ack (`lf_fft_n`, `lf_overlap_pct`, `lf_avg_tau_ms`,
//! `crossover_hz`) and the first frame's `sr`. A missing field fails the
//! test by name; nothing falls back to a default, because a fallback is how
//! a derived duration quietly becomes a hardcoded one.
//!
//! On the first violation the soak receives one more frame and writes
//! frames N-1, N, N+1 as CSVs under `CARGO_TARGET_TMPDIR`, then fails with
//! the path.

mod checker;

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::common::{Client, Daemon};
use checker::{check_bounded, lf_only_edge_hz, SoakChecker, SoakFrame, Violation};

/// HF FFT length. Must be below `lf_fft_n`, or the daemon disables the LF
/// band and there is nothing temporal to soak.
const SOAK_FFT_N: u32 = 8192;

/// Stimulus level: clear of 0 dBFS (I4-t headroom) and of the floor, so
/// collapse or garbage is unambiguous. The fake backend's noise is a
/// per-channel seeded LCG, so a run replays identically.
const NOISE_DBFS: f64 = -20.0;

/// The interval is set so one LF hop spans this many ticks.
const TARGET_HOP_TICKS: f64 = 4.0;

/// Below this many ticks per hop, "frozen beyond 2x hop" and one tick of
/// recompute-phase slack can no longer be told apart; the soak refuses to
/// run rather than judge on it.
const MIN_HOP_TICKS: f64 = 3.0;

/// Soak length floor, and the multiple of the LF window it must also
/// exceed (ported from c9523d9's `I5_SOAK_MIN_FLOOR_S` /
/// `I5_SOAK_WINDOW_MARGIN`). The window term takes over by itself once
/// `lf_fft_n / sr` exceeds 1.5 s.
const SOAK_MIN_FLOOR_S: f64 = 15.0;
const SOAK_WINDOW_MARGIN: f64 = 10.0;

/// Settle: one LF window to fill, then the longer of 5 EMA time constants
/// and 3 hops (c9523d9's `run_i5_soak` rule).
const SETTLE_TAU_MULTIPLE: f64 = 5.0;
const SETTLE_HOP_MULTIPLE: f64 = 3.0;

/// Each invariant must have been judged on at least this fraction of the
/// post-settle frames, and the LF slice must have changed at least
/// [`MIN_LF_CHANGES`] times — so a soak whose LF band never engaged fails.
const MIN_CHECKED_FRACTION: f64 = 0.5;
const MIN_LF_CHANGES: usize = 5;

/// No spectrum frame for this long is a stalled stream.
const FRAME_TIMEOUT_MS: i32 = 5_000;

/// A numeric field the soak's timing depends on; missing or non-numeric
/// fails the test naming it.
fn required_f64(v: &Value, field: &str, what: &str) -> f64 {
    v.get(field).and_then(Value::as_f64).unwrap_or_else(|| {
        panic!("{what} lacks numeric `{field}` — soak cannot derive its timing: {v}")
    })
}

/// Received frames against `wall elapsed / interval`, as information only.
/// The receiver cannot tell a PUB drop from a tick that ran longer than
/// `interval` (a debug build on a loaded host measured 44 ms per 34 ms tick
/// with every invariant green), and a slow tick is harmless to the frame
/// clock, so this is never a pass/fail input.
fn pacing_readout(received: usize, clock_wall_s: f64, interval: f64) -> String {
    let expected = clock_wall_s / interval;
    let ms_per_tick = if received > 0 {
        1000.0 * clock_wall_s / received as f64
    } else {
        f64::NAN
    };
    format!(
        "received {received} frames in {clock_wall_s:.2} s, {expected:.0} at wall/interval \
         ({:.0} %), mean {ms_per_tick:.1} ms/tick for {:.1} ms requested",
        100.0 * received as f64 / expected,
        1000.0 * interval
    )
}

/// A frame as received, with what the dump needs beside the checker input.
struct Received {
    /// Index in the checker's clock; `None` for the frame read before the
    /// interval was set.
    frame_idx: Option<usize>,
    wall_s: f64,
    frame: SoakFrame,
}

/// Next channel-0 `visualize/spectrum` frame with a non-empty spectrum, or
/// `None` if none arrives within [`FRAME_TIMEOUT_MS`].
fn next_spectrum(c: &Client) -> Option<Value> {
    let deadline = Instant::now() + Duration::from_millis(FRAME_TIMEOUT_MS as u64);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let (topic, payload) = c.recv_pub(remaining.as_millis().max(1) as i32)?;
        if topic != "data"
            || payload.get("type").and_then(Value::as_str) != Some("visualize/spectrum")
            || payload.get("channel").and_then(Value::as_u64) != Some(0)
        {
            continue;
        }
        if payload
            .get("spectrum")
            .and_then(Value::as_array)
            .is_some_and(|s| !s.is_empty())
        {
            return Some(payload);
        }
    }
}

/// Wire frame → checker input. `spectrum` is linear amplitude on the wire
/// (ZMQ.md → `visualize/spectrum`); the checker judges dBFS, so each value
/// becomes `20·log10(amplitude)`. Non-numeric entries become NaN so I4-t
/// reports them as garbage instead of the parse hiding them.
fn soak_frame(payload: &Value) -> SoakFrame {
    let col = |key: &str| -> Vec<f64> {
        payload[key]
            .as_array()
            .unwrap_or_else(|| panic!("spectrum frame lacks `{key}` array: {payload}"))
            .iter()
            .map(|v| v.as_f64().unwrap_or(f64::NAN))
            .collect()
    };
    let freqs = col("freqs");
    let spectrum: Vec<f64> = col("spectrum")
        .into_iter()
        .map(|a| 20.0 * a.log10())
        .collect();
    assert_eq!(
        freqs.len(),
        spectrum.len(),
        "`freqs` and `spectrum` lengths differ"
    );
    SoakFrame { freqs, spectrum }
}

/// Where a failing soak writes its dump, under `CARGO_TARGET_TMPDIR`.
fn dump_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()))
}

/// Write frames N-1, N, N+1 as `freq_hz,dbfs` CSVs with a comment header,
/// plus a `violation.txt` sidecar, into `dir`. `frames` holds what was
/// received, oldest first; a missing N-1 or N+1 is written as a header-only
/// file saying so.
fn dump_violation(
    dir: PathBuf,
    frames: &[&Received],
    have_next: bool,
    v: &Violation,
    elapsed_frames: usize,
    wall_s: f64,
    pacing: &str,
) -> PathBuf {
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));

    let mut summary = String::new();
    let _ = writeln!(summary, "invariant={}", v.invariant.label());
    let _ = writeln!(summary, "class={}", v.class);
    let _ = writeln!(summary, "frame_index={}", v.frame_idx);
    let _ = writeln!(summary, "elapsed_frames={elapsed_frames}");
    let _ = writeln!(summary, "elapsed_wall_s={wall_s:.3}");
    let _ = writeln!(summary, "detail={}", v.detail);
    let _ = writeln!(summary, "pacing (information only)={pacing}");

    let write = |name: &str, body: &str| {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap_or_else(|e| panic!("write {}: {e}", p.display()));
    };
    write("violation.txt", &summary);

    let n_idx = frames.len() - usize::from(have_next) - 1;
    if n_idx == 0 {
        write(
            "frame_N-1.csv",
            "# missing: frame N was the first frame received\nfreq_hz,dbfs\n",
        );
    }
    for (i, r) in frames.iter().enumerate() {
        let name = match i as isize - n_idx as isize {
            -1 => "frame_N-1.csv",
            0 => "frame_N.csv",
            1 => "frame_N+1.csv",
            _ => continue,
        };
        let mut body = String::new();
        match r.frame_idx {
            Some(k) => {
                let _ = writeln!(body, "# frame_index={k}");
            }
            None => {
                let _ = writeln!(body, "# frame_index=pre-interval");
            }
        }
        let _ = writeln!(body, "# elapsed_wall_s={:.3}", r.wall_s);
        let _ = writeln!(
            body,
            "# invariant={} class={}",
            v.invariant.label(),
            v.class
        );
        let _ = writeln!(body, "# detail={}", v.detail);
        body.push_str("freq_hz,dbfs\n");
        for (f, d) in r.frame.freqs.iter().zip(&r.frame.spectrum) {
            let _ = writeln!(body, "{f},{d}");
        }
        write(name, &body);
    }
    if !have_next {
        write(
            "frame_N+1.csv",
            "# missing: stream ended before frame N+1 arrived\nfreq_hz,dbfs\n",
        );
    }
    dir
}

/// Receive N+1 (if the stream still runs), dump, and panic. `next_frame_idx`
/// is N+1's index on the soak clock: `0` when N is the pre-interval frame,
/// which is not on the clock.
fn fail_with_dump(
    c: &Client,
    history: &VecDeque<Received>,
    v: Violation,
    next_frame_idx: usize,
    start: Instant,
    frames_seen: usize,
    pacing: &str,
) -> ! {
    let next = next_spectrum(c).map(|p| Received {
        frame_idx: Some(next_frame_idx),
        wall_s: start.elapsed().as_secs_f64(),
        frame: soak_frame(&p),
    });
    let mut frames: Vec<&Received> = history.iter().collect();
    if let Some(n) = &next {
        frames.push(n);
    }
    let wall_s = history.back().map_or(0.0, |r| r.wall_s);
    let dir = dump_violation(
        dump_dir("monitor_soak"),
        &frames,
        next.is_some(),
        &v,
        frames_seen,
        wall_s,
        pacing,
    );
    let _ = c.call(json!({"cmd": "stop"}));
    panic!(
        "monitor soak: {} ({}) at frame {} after {frames_seen} frames / {wall_s:.3} s: {}\n\
         frames N-1/N/N+1 dumped to {}",
        v.invariant.label(),
        v.class,
        v.frame_idx,
        v.detail,
        dir.display()
    );
}

#[test]
fn monitor_spectrum_soak_holds_every_frame() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    let ack = c.call(json!({
        "cmd": "monitor_spectrum",
        "channels": [0],
        "fft_n": SOAK_FFT_N,
        "fake_noise_dbfs": NOISE_DBFS,
    }));
    assert_eq!(ack["ok"], json!(true), "monitor_spectrum ack: {ack}");
    let lf_fft_n = required_f64(&ack, "lf_fft_n", "monitor_spectrum ack");
    let lf_overlap_pct = required_f64(&ack, "lf_overlap_pct", "monitor_spectrum ack");
    let lf_avg_tau_ms = required_f64(&ack, "lf_avg_tau_ms", "monitor_spectrum ack");
    let crossover_hz = required_f64(&ack, "crossover_hz", "monitor_spectrum ack");
    assert!(
        (SOAK_FFT_N as f64) < lf_fft_n,
        "fft_n {SOAK_FFT_N} must be below lf_fft_n {lf_fft_n}, or the LF band is disabled"
    );

    let start = Instant::now();
    let first = next_spectrum(&c).unwrap_or_else(|| {
        panic!(
            "no spectrum frame within {FRAME_TIMEOUT_MS} ms. daemon log:\n{}",
            d.log_tail()
        )
    });
    let sr = required_f64(&first, "sr", "first spectrum frame");
    let first = Received {
        frame_idx: None,
        wall_s: start.elapsed().as_secs_f64(),
        frame: soak_frame(&first),
    };
    let mut history: VecDeque<Received> = VecDeque::with_capacity(3);
    if let Some(v) = check_bounded(&first.frame, 0) {
        history.push_back(first);
        fail_with_dump(
            &c,
            &history,
            v,
            0,
            start,
            0,
            "no frames on the soak clock yet",
        );
    }
    history.push_back(first);

    // Frame-count clock: pick the interval so one LF hop is
    // TARGET_HOP_TICKS ticks, and derive hop_ticks back from the echoed
    // interval — independent of the daemon's own `lf_recompute_every`.
    let lf_window_s = lf_fft_n / sr;
    let hop_s = lf_window_s * (1.0 - lf_overlap_pct / 100.0);
    let want_interval = hop_s / TARGET_HOP_TICKS;
    let r = c.call(json!({"cmd": "set_monitor_params", "interval": want_interval}));
    assert_eq!(r["ok"], json!(true), "set_monitor_params: {r}");
    let interval = required_f64(&r, "interval", "set_monitor_params reply");
    assert!(
        (interval - want_interval).abs() <= 1e-9 * want_interval,
        "set_monitor_params echoed interval {interval}, asked {want_interval}"
    );
    let hop_ticks = hop_s / interval;
    assert!(
        hop_ticks >= MIN_HOP_TICKS,
        "hop_ticks {hop_ticks:.3} < {MIN_HOP_TICKS}: hop {hop_s:.4} s (lf_fft_n {lf_fft_n}, \
         sr {sr}, overlap {lf_overlap_pct} %) vs interval {interval:.4} s"
    );

    let tau_s = lf_avg_tau_ms / 1000.0;
    let settle_s = lf_window_s + (SETTLE_TAU_MULTIPLE * tau_s).max(SETTLE_HOP_MULTIPLE * hop_s);
    let settle_frames = (settle_s / interval).ceil() as usize;
    let soak_s = SOAK_MIN_FLOOR_S.max(SOAK_WINDOW_MARGIN * lf_window_s);
    let soak_frames = (soak_s / interval).ceil() as usize;
    let total_frames = settle_frames + soak_frames;
    let lf_edge_hz = lf_only_edge_hz(crossover_hz);
    eprintln!(
        "monitor soak: sr={sr} lf_fft_n={lf_fft_n} (window {lf_window_s:.3} s) \
         overlap={lf_overlap_pct} % (hop {hop_s:.4} s) tau={tau_s:.3} s \
         crossover={crossover_hz} Hz lf_edge={lf_edge_hz:.1} Hz interval={interval:.4} s hop_ticks={hop_ticks:.3} \
         settle={settle_frames} frames soak={soak_frames} frames"
    );

    let mut checker = SoakChecker::new(crossover_hz, lf_edge_hz, hop_ticks, settle_frames);
    let clock_start = Instant::now();
    for idx in 0..total_frames {
        let payload = next_spectrum(&c).unwrap_or_else(|| {
            panic!(
                "monitor soak: spectrum stream stalled at frame {idx} of {total_frames} \
                 (no frame within {FRAME_TIMEOUT_MS} ms). daemon log:\n{}",
                d.log_tail()
            )
        });
        let rec = Received {
            frame_idx: Some(idx),
            wall_s: start.elapsed().as_secs_f64(),
            frame: soak_frame(&payload),
        };
        let verdict = checker.check_frame(&rec.frame);
        history.push_back(rec);
        while history.len() > 2 {
            history.pop_front();
        }
        if let Some(v) = verdict {
            let pacing = pacing_readout(idx + 1, clock_start.elapsed().as_secs_f64(), interval);
            eprintln!(
                "monitor soak healthy margin (information only, to the violation): {}",
                checker.stats().readout()
            );
            fail_with_dump(&c, &history, v, idx + 1, start, idx + 1, &pacing);
        }
    }
    let clock_wall_s = clock_start.elapsed().as_secs_f64();
    let _ = c.call(json!({"cmd": "stop"}));

    eprintln!(
        "monitor soak pacing (information only): {}",
        pacing_readout(total_frames, clock_wall_s, interval)
    );
    eprintln!(
        "monitor soak healthy margin (information only): {}",
        checker.stats().readout()
    );

    let k = checker.counts();
    let need = (MIN_CHECKED_FRACTION * soak_frames as f64).ceil() as usize;
    for (name, n) in [
        ("I4-t bounded", k.bounded),
        ("I2-t continuity", k.continuity),
        ("I5a liveness", k.liveness),
        ("I5c rate", k.rate),
        ("I5b plausibility", k.plausibility),
    ] {
        assert!(
            n >= need,
            "monitor soak: {name} judged on {n} frames, need >= {need} of {soak_frames} \
             post-settle — the invariant did not engage. counts: {k:?}"
        );
    }
    assert!(
        k.lf_changes >= MIN_LF_CHANGES,
        "monitor soak: LF slice changed {} times post-settle, need >= {MIN_LF_CHANGES}. \
         counts: {k:?}",
        k.lf_changes
    );
}

/// The dump path runs only when the soak fails, which is exactly when it is
/// needed, so it is exercised here on synthetic frames.
#[test]
fn dump_violation_writes_n_minus_1_n_n_plus_1_and_sidecar() {
    let mk = |k: Option<usize>, v: f64| Received {
        frame_idx: k,
        wall_s: k.unwrap_or(0) as f64 * 0.1,
        frame: SoakFrame {
            freqs: vec![100.0, 200.0],
            spectrum: vec![v, v - 1.0],
        },
    };
    let (a, b, c) = (mk(Some(9), -40.0), mk(Some(10), -41.0), mk(Some(11), -42.0));
    let v = Violation {
        invariant: checker::Invariant::Liveness,
        class: "frozen",
        frame_idx: 10,
        detail: "synthetic".into(),
    };
    let read = |dir: &std::path::Path, n: &str| {
        std::fs::read_to_string(dir.join(n)).unwrap_or_else(|e| panic!("{n}: {e}"))
    };

    let dir = dump_violation(
        dump_dir("monitor_soak_selftest_full"),
        &[&a, &b, &c],
        true,
        &v,
        11,
        1.0,
        "pacing",
    );
    assert!(read(&dir, "frame_N-1.csv").contains("# frame_index=9\n"));
    assert!(read(&dir, "frame_N.csv").contains("# frame_index=10\n"));
    assert!(read(&dir, "frame_N.csv").ends_with("freq_hz,dbfs\n100,-41\n200,-42\n"));
    assert!(read(&dir, "frame_N+1.csv").contains("# frame_index=11\n"));
    let s = read(&dir, "violation.txt");
    assert!(s.contains("invariant=I5a liveness\n"), "{s}");
    assert!(
        s.contains("frame_index=10\n") && s.contains("elapsed_wall_s=1.000\n"),
        "{s}"
    );

    // Stream ended before N+1: N is still the last received frame.
    let dir = dump_violation(
        dump_dir("monitor_soak_selftest_no_next"),
        &[&a, &b],
        false,
        &v,
        11,
        1.0,
        "pacing",
    );
    assert!(read(&dir, "frame_N-1.csv").contains("# frame_index=9\n"));
    assert!(read(&dir, "frame_N.csv").contains("# frame_index=10\n"));
    assert!(read(&dir, "frame_N+1.csv").starts_with("# missing"));

    // Violation on the pre-interval frame: no N-1 exists, and N+1 is clock
    // frame 0, as `fail_with_dump` labels it.
    let pre = mk(None, -40.0);
    let first = mk(Some(0), -41.0);
    let dir = dump_violation(
        dump_dir("monitor_soak_selftest_first"),
        &[&pre, &first],
        true,
        &v,
        0,
        0.0,
        "pacing",
    );
    assert!(read(&dir, "frame_N-1.csv").starts_with("# missing"));
    assert!(read(&dir, "frame_N.csv").contains("# frame_index=pre-interval\n"));
    assert!(read(&dir, "frame_N+1.csv").contains("# frame_index=0\n"));
}
