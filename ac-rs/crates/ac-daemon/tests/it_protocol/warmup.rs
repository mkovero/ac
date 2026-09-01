use serde_json::json;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use crate::common::{Client, Daemon};

/// Pull the per-ring `occ=[..]` list off one `AC_DRAIN_TELEMETRY` raw line
/// (#208 D1). Returns `None` for anything that is not a per-tick record — the
/// window summary lines and any other daemon stderr.
///
/// The list itself is parsed, not the pre-reduced `occ_min`/`occ_max` fields,
/// so the test can tell "every ring agreed" from "no ring was reported at
/// all": both give `occ_min == occ_max`, and only one of them means anything.
fn parse_occ(line: &str) -> Option<Vec<usize>> {
    if !line.starts_with("drain-tick ") {
        return None;
    }
    let body = line.split_once("occ=[")?.1.split_once(']')?.0.trim();
    if body.is_empty() {
        return Some(Vec::new());
    }
    body.split(',')
        .map(|t| t.trim().parse::<usize>().ok())
        .collect()
}

/// Issue #216: every capture ring must come out of the session's warmup flush
/// holding the same number of samples.
///
/// The rig evidence was exactly this telemetry: `occ=[5120, 24320]` — each
/// reference ring a constant 19200 samples (0.2 s at 96 kHz) above meas,
/// unchanged across 929 ticks of two runs. The cause is the warmup
/// `capture_block(0.2)`, which clears the measurement ring only. Nothing
/// afterwards re-syncs: `capture_multi_contiguous` pops `min_occupied()` from
/// every ring, and a constant offset survives that untouched. The skew is what
/// put `delay_ms` 200 ms negative, coherence at ~0.64 and `magnitude_db` 2.5 dB
/// out on every ring-backed session since #207.
///
/// `fake_ring` is what makes it reproducible without hardware — the default
/// on-demand fake generator has no ring at all and is structurally incapable of
/// holding a skew (see `FakeRings`' docs). The stimulus is left at the default
/// on purpose: the defect lives in the capture path, not in the stimulus, and a
/// correlated pair used to take a different warmup branch that hid it. This
/// test therefore covers the branch that was actually broken.
#[test]
fn warmup_leaves_every_capture_ring_at_the_same_phase() {
    let d = Daemon::spawn_with_env(&[("AC_DRAIN_TELEMETRY", "1")]);
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd": "transfer_stream", "meas_channel": 0, "ref_channel": 1,
        "weighting": "Z", "integration": "fast",
        // Zero processing gap: this test is about the warmup's phase, not
        // about the per-tick backlog `process_secs` exists to model.
        "fake_ring": {"process_secs": 0.0},
    }));
    assert_eq!(r["ok"], json!(true), "transfer_stream start: {r}");
    thread::sleep(Duration::from_millis(600));
    let _ = c.call(json!({"cmd": "stop"}));

    let log = d.log();
    let ticks: Vec<Vec<usize>> = log.lines().filter_map(parse_occ).collect();
    assert!(
        ticks.len() >= 3,
        "expected AC_DRAIN_TELEMETRY per-tick lines, parsed {} from:\n{log}",
        ticks.len()
    );
    // Without this the whole test is vacuous: a backend that reports no
    // occupancy at all trivially has no spread between its rings. This is how
    // the test first passed against the unfixed daemon — `FakeEngine`
    // inherited the trait's empty `last_drain_occupancy`.
    assert!(
        ticks.iter().all(|occ| occ.len() >= 2),
        "telemetry must report meas + at least one ref per tick, got {:?}",
        &ticks[..ticks.len().min(3)]
    );

    let skewed: Vec<&Vec<usize>> = ticks
        .iter()
        .filter(|occ| occ.iter().min() != occ.iter().max())
        .collect();
    assert!(
        skewed.is_empty(),
        "{} of {} ticks show a meas/ref ring skew: {:?} — the warmup flush \
         must clear and pop meas and every ref together (#216)",
        skewed.len(),
        ticks.len(),
        &skewed[..skewed.len().min(5)]
    );
}

/// #254 — a `transfer_stream` over three or more **distinct** capture channels
/// replies `ok: true` and then publishes nothing, forever.
///
/// **This test is differential on purpose, and that is the whole design.** The
/// obvious shape — request three channels, wait, assert a frame arrived — is
/// the shape that cannot tell the defect from a slow machine, and this project
/// has shipped that mistake before: `FakeEngine` inherited the trait's empty
/// `last_drain_occupancy`, so the one mode built to reproduce ring defects
/// reported `occ=[]` and the ring test passed against the unfixed daemon
/// (`ring_drain_keeps_meas_and_ref_in_lockstep` above, and the comment at
/// `handlers/transfer.rs`'s drain arm). A timeout is not an observation.
///
/// So a **two-channel control session runs first, on the same daemon, in the
/// same process, against the same clock**, and its time-to-first-frame sets
/// the budget for the three-channel session. That cancels machine speed: if
/// the control produces a frame and the three-channel session produces neither
/// a frame nor an error in several times that budget, the difference is the
/// session shape and nothing else.
///
/// Three outcomes, all of them stated rather than inferred:
///
/// - control silent → **fails as inconclusive**, naming itself as unable to
///   judge #254. It must never pass by both sessions being silent.
/// - three channels silent while the control spoke → **fails as #254**, which
///   is the state of `main` today.
/// - three channels publish both pairs, or the launch is refused with a named
///   error → **passes**. Both are acceptable fixes; direction 1 in the issue
///   is the refusal, direction 2 is the fake modelling N channels. A refusal
///   that names the mismatch is a recoverable client error. Silence is not.
#[test]
fn three_distinct_channels_publish_or_refuse_but_never_stall() {
    let d = Daemon::spawn();
    let c = Client::new(&d);

    // --- control: two distinct channels, the shape that works today ---------
    let started = Instant::now();
    let r = c.call(json!({
        "cmd":        "transfer_stream",
        "pairs":      [[0, 1]],
        "drive":      true,
        "level_dbfs": -12.0,
    }));
    assert_eq!(r["ok"], json!(true), "control session refused: {r:?}");

    let mut control_first: Option<Duration> = None;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                control_first = Some(started.elapsed());
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));
    let _ = c.wait_for_topic("done", Duration::from_secs(5));

    let control_first = control_first.expect(
        "INCONCLUSIVE, not a #254 failure: the two-channel control session \
         published no transfer_stream frame in 15 s. This test cannot say \
         anything about three-channel behaviour until the two-channel path \
         works here — fix the control first.",
    );

    // Budget generously against the control, so the verdict is about session
    // shape rather than about how loaded this machine is. Six ticks' worth of
    // slack, floored at 5 s for a fast control and capped so a pathologically
    // slow control cannot hang the suite.
    let budget = (control_first * 6).clamp(Duration::from_secs(5), Duration::from_secs(30));

    // --- the case: three distinct channels, {0, 1, 3} -----------------------
    // `[[3,3],[0,3]]` — the converter-constant shape rig session 3 ran — is
    // two distinct channels and is unaffected. `[[0,3],[1,3]]` is a second
    // measurement position against the same reference, which the rig has
    // already produced results from (`rig-session-results.md`, Run 5), and it
    // is three.
    let d2 = Daemon::spawn();
    let c2 = Client::new(&d2);
    let r2 = c2.call(json!({
        "cmd":        "transfer_stream",
        "pairs":      [[0, 3], [1, 3]],
        "drive":      true,
        "level_dbfs": -12.0,
    }));

    // A refusal at launch is a pass: it is the recoverable outcome direction 1
    // asks for. Anything else must go on to publish.
    if r2["ok"] != json!(true) {
        let msg = r2["error"].as_str().unwrap_or_default().to_string()
            + r2["message"].as_str().unwrap_or_default();
        assert!(
            !msg.trim().is_empty(),
            "three-channel launch was refused without a message: {r2:?}"
        );
        return;
    }

    let mut meas_seen: Vec<u64> = Vec::new();
    let mut error_msg: Option<String> = None;
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c2.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "error" => {
                error_msg = Some(v.to_string());
                break;
            }
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                if let Some(ch) = v["meas_channel"].as_u64() {
                    if !meas_seen.contains(&ch) {
                        meas_seen.push(ch);
                    }
                }
                if meas_seen.len() >= 2 {
                    break;
                }
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c2.call(json!({"cmd": "stop"}));

    if error_msg.is_some() {
        return; // loud refusal mid-session: recoverable, and visible.
    }

    meas_seen.sort_unstable();
    assert!(
        !meas_seen.is_empty(),
        "#254: `pairs=[[0,3],[1,3]]` (three distinct channels) replied ok:true \
         and then published no transfer_stream frame and no error in {:?}, \
         while the two-channel control on this same machine published its \
         first frame in {:?}. The session shape is the only difference. \
         `capture_multi` returns two buffers regardless of the session's \
         channel count (audio/fake.rs), ring 2 never reaches `nperseg`, and \
         the warmup gate in handlers/transfer.rs `continue`s forever.",
        budget,
        control_first,
    );
    assert_eq!(
        meas_seen.len(),
        2,
        "#254: only measurement channel(s) {meas_seen:?} published; both 0 and \
         1 must appear. A session that publishes one pair of a two-pair request \
         and silently drops the other is the same defect one pair further in.",
    );
}

/// #254, the half that converts rig work into desk work: `--fake-audio` must
/// actually *run* a three-channel session, not merely refuse it loudly.
///
/// The test above accepts a named refusal as a pass, because for a backend
/// that genuinely cannot capture N channels a refusal is the right answer.
/// That is deliberately too weak here: a regression that returned the fake to
/// two buffers would trip the handler guard, produce a clean error, and leave
/// that test green. This one takes no error for an answer.
///
/// `pairs=[[0,3],[1,3]]` is a second measurement position against a shared
/// reference — the shape `rig-session-results.md` Run 5 already produced
/// results from on hardware, and the shape nothing desk-side could rehearse
/// while this bug stood.
#[test]
fn three_distinct_channels_publish_both_pairs_on_fake_audio() {
    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":        "transfer_stream",
        "pairs":      [[0, 3], [1, 3]],
        "drive":      true,
        "level_dbfs": -12.0,
    }));
    assert_eq!(r["ok"], json!(true), "unexpected REP: {r:?}");

    let mut meas_seen: Vec<u64> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && meas_seen.len() < 2 {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "error" => errors.push(v.to_string()),
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                let meas = v["meas_channel"]
                    .as_u64()
                    .expect("frame without meas_channel");
                let refch = v["ref_channel"]
                    .as_u64()
                    .expect("frame without ref_channel");
                assert_eq!(refch, 3, "both pairs reference channel 3: {v}");
                if !meas_seen.contains(&meas) {
                    meas_seen.push(meas);
                }
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));

    meas_seen.sort_unstable();
    assert!(
        errors.is_empty(),
        "the fake backend must run three channels, not refuse them: {errors:?}"
    );
    assert_eq!(
        meas_seen,
        vec![0, 1],
        "expected a frame for each measurement channel against the shared \
         reference; saw {meas_seen:?}"
    );
}

/// #254, the part presence assertions cannot reach: **the second measurement
/// channel's delay must be the configured one.**
///
/// Every other test here is satisfied by three buffers arriving. A shared
/// measurement read cursor in the fake produces three buffers too — it
/// advances once per channel per tick, so the second channel reads a window
/// one buffer further along and reports a delay that is an artefact of call
/// order. `delay_attempts` climbs, a frame publishes, `pair_delays[1]` fills
/// in, and every presence check above goes green on a wrong number.
///
/// That is the failure mode worth guarding, because it is the one that
/// contaminates rather than blocks: an offline experiment built on fake
/// multi-position sessions would have inherited the artefact silently, with
/// nothing to distinguish it from a real delay. So this pins the value, on the
/// channel where a shared cursor would move it, against a known
/// `fake_correlated_pair`.
#[test]
fn both_measurement_channels_report_the_configured_delay() {
    let gain = 0.5_f64;
    // ~4.2 ms at 48 kHz, well inside the delay-search window — the same
    // figure `it_snapshot.rs`'s ground-truth test uses.
    let delay_samples = 200_i64;

    let d = Daemon::spawn();
    let c = Client::new(&d);
    let r = c.call(json!({
        "cmd":   "transfer_stream",
        "pairs": [[0, 3], [1, 3]],
        "fake_correlated_pair": {"gain": gain, "delay_samples": delay_samples},
    }));
    assert_eq!(r["ok"], json!(true), "unexpected REP: {r:?}");

    // One locked frame per measurement channel. Both pairs read the same
    // reference, so both must land on the same configured delay: the second
    // channel is not a second DUT, it is the same source read by another
    // capture channel.
    let mut locked: Vec<(u64, i64)> = Vec::new();
    let mut published: Vec<u64> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && locked.len() < 2 {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        match c.recv_pub(remaining.max(1)) {
            Some((t, v)) if t == "data" && v["type"].as_str() == Some("transfer_stream") => {
                let meas = v["meas_channel"]
                    .as_u64()
                    .expect("frame without meas_channel");
                if !published.contains(&meas) {
                    published.push(meas);
                }
                if v["delay_locked"].as_bool() != Some(true) {
                    continue;
                }
                let got = v["delay_samples"]
                    .as_i64()
                    .expect("frame without delay_samples");
                if !locked.iter().any(|(m, _)| *m == meas) {
                    locked.push((meas, got));
                }
            }
            Some(_) => continue,
            None => break,
        }
    }
    let _ = c.call(json!({"cmd": "stop"}));

    locked.sort_unstable();
    published.sort_unstable();

    // Split from the lock assertion so the two cannot be confused: no frames
    // at all is #254 itself regressing, and says nothing about delays.
    assert_eq!(
        published,
        vec![0, 1],
        "measurement channel(s) missing from published frames {published:?} — that \
         is the #254 stall, not a delay question"
    );

    // The shared-cursor artefact arrives by either of two roads, and both are
    // named here because the first one otherwise reads as flakiness. A shared
    // `correlated_meas_pos` shifts the second channel by a whole tick's worth
    // of samples — thousands, far outside the search window — so in practice
    // it decorrelates that channel and the estimator refuses it rather than
    // locking to a plausible wrong number. A smaller future artefact would
    // lock and land on the value check below instead. Verified red both ways.
    assert_eq!(
        locked.len(),
        2,
        "both channels published, but only {locked:?} locked within 30 s. A channel \
         that publishes and never locks is the shared-cursor artefact decorrelating \
         it: `FakeEngine::correlated_meas_pos` must be keyed per port, or the second \
         measurement channel reads a window one tick further along than the first.",
    );

    // Tolerance is one correlation bin, not a fitted margin: the estimator
    // reports whole samples and the fake's source is exact, so the honest
    // answer is the configured lag itself. A call-order artefact is off by a
    // whole tick's worth of samples — thousands — so it cannot hide in here.
    for (meas, got) in &locked {
        assert!(
            (got - delay_samples).abs() <= 1,
            "measurement channel {meas} reported delay {got}, configured {delay_samples}. \
             A delay that is wrong only on the second channel is the shared-cursor \
             artefact: `FakeEngine::correlated_meas_pos` must be keyed per port.",
        );
    }
}
