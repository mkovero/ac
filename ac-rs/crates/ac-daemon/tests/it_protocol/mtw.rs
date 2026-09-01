use serde_json::json;
use serde_json::Value;
use std::time::Duration;
use std::time::Instant;

use crate::common::{Client, Daemon};

fn f64s(v: &Value, key: &str) -> Vec<f64> {
    v[key]
        .as_array()
        .unwrap_or_else(|| panic!("mtw.{key} missing: {v}"))
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect()
}

/// Wait for a frame in which every rung has settled, so the column set spans
/// the whole ladder.
///
/// The `mtw` block itself appears as soon as the *top* rung settles (0.11 s at
/// 96 kHz) — the display fills downward rather than staying blank for the
/// bottom rung's 2.56 s — so an assertion that needs the full band must key on
/// the frame's own `settled_stages` rather than on the block's presence, and
/// certainly not on elapsed time, which would be a race.
fn wait_for_mtw_fully_settled(c: &Client, timeout: Duration) -> Value {
    wait_for_mtw_where(c, timeout, |m| {
        m["settled_stages"]
            .as_array()
            .is_some_and(|s| !s.is_empty() && s.iter().all(|v| v == &json!(true)))
    })
}

fn wait_for_mtw_where(c: &Client, timeout: Duration, ok: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(left.max(1)) {
            Some((t, v)) if t == "data" && v["type"] == json!("transfer_stream") => {
                if v["mtw"].is_object() && ok(&v["mtw"]) {
                    return v;
                }
            }
            Some(_) => continue,
            None => break,
        }
    }
    panic!("no matching mtw block within {timeout:?}");
}

/// End-to-end ground truth for the ladder: a known flat `H1` must come back
/// flat across every rung, with every column backed by real bins and carrying
/// the resolution, window and averaging that produced it.
///
/// `fake_correlated_pair` makes meas a known `gain`-scaled, delayed copy of
/// ref, so `|H1| = gain` and coherence ~1 are checkable ground truth rather
/// than a noise-over-noise ratio.
#[test]
fn mtw_columns_are_backed_by_bins_and_carry_their_provenance() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let gain = 0.5_f64;
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
        "fake_correlated_pair": {"gain": gain, "delay_samples": 200},
    }));
    assert_eq!(r["ok"], json!(true), "transfer_stream start: {r}");

    let frame = wait_for_mtw_fully_settled(&c, Duration::from_secs(30));
    let _ = c.call(json!({"cmd": "stop"}));
    let m = &frame["mtw"];

    let freqs = f64s(m, "freqs");
    let mag = f64s(m, "magnitude_db");
    let coh = f64s(m, "coherence");
    let df = f64s(m, "df");
    let window = f64s(m, "window_s");
    let n = f64s(m, "n");
    let bins = f64s(m, "bins");
    let stage = f64s(m, "stage");
    assert!(freqs.len() > 100, "only {} columns", freqs.len());
    for (name, v) in [
        ("magnitude_db", &mag),
        ("coherence", &coh),
        ("df", &df),
        ("window_s", &window),
        ("n", &n),
        ("bins", &bins),
        ("stage", &stage),
    ] {
        assert_eq!(v.len(), freqs.len(), "mtw.{name} length");
    }

    // Criterion 1, over the wire: no column is synthesised from its
    // neighbours, so every one maps to at least one source bin.
    assert!(
        bins.iter().all(|&b| b >= 1.0),
        "columns with no source bins: {:?}",
        bins.iter().take(20).collect::<Vec<_>>()
    );

    // Criterion 1's other half: each column really is at least one bin wide.
    let lo = f64s(m, "f_lo");
    let hi = f64s(m, "f_hi");
    for i in 0..freqs.len() {
        assert!(
            hi[i] - lo[i] >= df[i] * 0.999,
            "column {i} at {} Hz spans {} Hz but Δf is {}",
            freqs[i],
            hi[i] - lo[i],
            df[i]
        );
    }

    // Ground truth, in every rung that the display range reaches.
    let mut per_stage = [0usize; 8];
    for i in 0..freqs.len() {
        per_stage[stage[i] as usize] += 1;
        if freqs[i] < 80.0 || freqs[i] > 18_000.0 {
            continue;
        }
        assert!(
            (mag[i] - 20.0 * gain.log10()).abs() < 1.5,
            "{} Hz (stage {}): {} dB, want {}",
            freqs[i],
            stage[i],
            mag[i],
            20.0 * gain.log10()
        );
        assert!(coh[i] > 0.8, "{} Hz: coherence {}", freqs[i], coh[i]);
    }
    assert!(
        per_stage[0] > 0 && per_stage[1] > 0 && per_stage[2] > 0,
        "every rung must be exercised, got {per_stage:?}"
    );

    // Deliverable 4: the provenance is real, not a constant. Windows shorten
    // with frequency and Δf coarsens, monotonically, so a reader can tell how
    // stale and how resolved any column is.
    for i in 1..freqs.len() {
        assert!(
            window[i] <= window[i - 1] + 1e-12,
            "window rose at {}",
            freqs[i]
        );
        assert!(df[i] >= df[i - 1] - 1e-12, "Δf fell at {}", freqs[i]);
    }
    assert!(
        window[0] > window[freqs.len() - 1] * 4.0,
        "the ladder must actually use different windows: {} vs {}",
        window[0],
        window[freqs.len() - 1]
    );

    // Criterion 5: N is present and equals the configured value, in every
    // column. Uniform across stages is the whole point — an N that varied with
    // frequency would put a coherence step at a fixed frequency.
    assert!(
        n.iter().all(|&v| v == 4.0),
        "N must be the configured 4 in every column, got {:?}",
        n.iter().take(8).collect::<Vec<_>>()
    );
    assert_eq!(m["n_blocks"], json!(4));

    // The stage table ships alongside so `stage` is interpretable.
    let stages = m["stages"].as_array().expect("mtw.stages");
    assert_eq!(stages.len(), 3, "48 kHz ladder is three rungs");
    assert_eq!(stages[0]["decim"], json!(1), "stage 0 is always full rate");
    assert_eq!(stages[1]["decim"], json!(4));
    assert_eq!(stages[2]["decim"], json!(12));
    // Settling: the bottom rung must not be slower than the full-rate
    // estimator it replaces (2.5 s today), and the top must be far faster.
    let settling = |i: usize| stages[i]["settling_s"].as_f64().unwrap();
    assert!(
        settling(2) < 2.6,
        "bottom rung settles in {} s",
        settling(2)
    );
    assert!(settling(0) < 0.25, "top rung settles in {} s", settling(0));
}

/// The ladder is additive. Everything the frame carried before it must be
/// untouched — in particular `spl` (criterion 7, the conformance guard) and
/// the calibrated per-channel spectra, which are absolute levels and so are
/// deliberately **not** routed through the ladder.
#[test]
fn mtw_does_not_disturb_the_existing_frame() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
        "fake_correlated_pair": {"gain": 0.5, "delay_samples": 200},
    }));
    assert_eq!(r["ok"], json!(true), "{r}");
    let frame = wait_for_mtw_fully_settled(&c, Duration::from_secs(30));
    let _ = c.call(json!({"cmd": "stop"}));

    for key in [
        "freqs",
        "magnitude_db",
        "phase_deg",
        "coherence",
        "spec_freqs",
        "meas_spectrum",
        "ref_spectrum",
    ] {
        assert!(
            frame[key].as_array().is_some_and(|a| !a.is_empty()),
            "{key} went missing or empty when the ladder landed"
        );
    }
    for key in [
        "delay_samples",
        "delay_ms",
        "sr",
        "spl_weighting",
        "spl_integration",
    ] {
        assert!(!frame[key].is_null(), "{key} went missing");
    }
    // The pre-existing H1 arrays are the full-rate Welch estimate and must
    // still be the 2000-point decimation of a 1 Hz grid — the ladder is a
    // second view, not a replacement.
    let freqs = f64s(&frame, "freqs");
    assert!(
        freqs.len() > 1_900 && freqs.len() <= 2_000,
        "full-rate H1 grid changed: {} points",
        freqs.len()
    );
    // And it is a linear grid, unlike the ladder's log columns.
    let d0 = freqs[1] - freqs[0];
    let d1 = freqs[freqs.len() - 1] - freqs[freqs.len() - 2];
    assert!(
        (d0 - d1).abs() < d0 * 0.5,
        "full-rate grid stopped being linear"
    );
}

/// Density is a parameter; the crossovers are not. Raising `mtw_ppo` adds
/// columns where the ladder can back them and leaves the rung boundaries
/// exactly where they were — so two captures at different densities remain
/// comparable.
#[test]
fn mtw_density_is_a_parameter_that_does_not_move_the_crossovers() {
    fn run(ppo: Option<f64>) -> Value {
        let d = Daemon::spawn();
        let c = Client::new(&d);
        let mut cmd = json!({
            "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
            "weighting": "Z", "integration": "fast",
            "fake_correlated_pair": {"gain": 0.5, "delay_samples": 200},
        });
        if let Some(p) = ppo {
            cmd["mtw_ppo"] = json!(p);
        }
        let r = c.call(cmd);
        assert_eq!(r["ok"], json!(true), "{r}");
        let f = wait_for_mtw_fully_settled(&c, Duration::from_secs(30));
        let _ = c.call(json!({"cmd": "stop"}));
        f["mtw"].clone()
    }

    let base = run(None);
    let dense = run(Some(192.0));
    assert!(
        f64s(&dense, "freqs").len() > f64s(&base, "freqs").len(),
        "a denser request must add columns"
    );
    assert_eq!(
        base["stages"], dense["stages"],
        "display density must not move the ladder's crossovers"
    );

    // Below the deepest rung's validity edge both grids are Δf-limited, so the
    // extra density buys nothing — which is the honest outcome, and the point
    // of dropping the interpolation branch.
    let edge = base["stages"][2]["f_valid"].as_f64().unwrap();
    let below = |m: &Value| f64s(m, "freqs").iter().filter(|&&f| f < edge).count();
    assert_eq!(
        below(&base),
        below(&dense),
        "columns below the validity edge must not multiply with density"
    );
}

// ---------------------------------------------------------------------------
// #216 — warmup ring phase
// ---------------------------------------------------------------------------
