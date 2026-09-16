//! Opt-in, fake-only test hooks, driven by `AC_FAKE_*` environment
//! variables. Each is read once per process and is inert when unset, so
//! the default fake lifecycle is unchanged by their existence.
//!
//! One `AC_FAKE_*` hook lives outside this file: `AC_FAKE_DEVICE_EPOCH`
//! (#461) is read by `audio::epoch`, because the device-enumeration probe is
//! keyed by backend name and runs without a `FakeEngine`.

/// Default `play_and_capture` loopback delay, unchanged from before #348's
/// test-hook addition below.
const DEFAULT_LOOPBACK_DELAY_SAMPLES: usize = 32;

/// Opt-in, fake-only test hook (QA #348 test-coverage gap on #347): lets an
/// external integration test drive `measure_tau_twice`'s two independent
/// `play_and_capture` calls to *different* delays, which the daemon-under-
/// test's `--fake-audio` subprocess reads once at first use. Without this,
/// every fake lifecycle used the same fixed constant, so the disagreement
/// branch of τ comparison (`compare_tau_readings`'s `Disagree` arm) was
/// reachable only through unit tests that hand-construct a `TauComparison`
/// directly — never through a real `measure_tau_twice` call.
///
/// `AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE`: comma-separated sample-delay list,
/// consumed one value per `play_and_capture` call in this process (0-based:
/// the first call gets the first value); a call past the end of the list
/// falls back to [`DEFAULT_LOOPBACK_DELAY_SAMPLES`]. Unset ⇒ every call
/// uses the default, i.e. byte-identical to pre-#348 behaviour.
static TAU_DELAY_CALL_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub(super) fn tau_delay_override_list() -> &'static [usize] {
    static LIST: std::sync::OnceLock<Vec<usize>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| {
        std::env::var("AC_FAKE_TAU_DELAY_SAMPLES_OVERRIDE")
            .ok()
            .map(|s| s.split(',').filter_map(|v| v.trim().parse().ok()).collect())
            .unwrap_or_default()
    })
}

/// Next `play_and_capture` loopback delay, consuming one slot of the
/// override list (see [`TAU_DELAY_CALL_COUNT`] doc above).
pub(super) fn next_loopback_delay_samples() -> usize {
    let call_idx = TAU_DELAY_CALL_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    tau_delay_override_list()
        .get(call_idx)
        .copied()
        .unwrap_or(DEFAULT_LOOPBACK_DELAY_SAMPLES)
}

/// Opt-in, fake-only test hook, paired with the delay override above: lets
/// a test give the fake backend a `period_size` (real backends report one;
/// the fake's default `AudioEngine::period_size` impl is `None`, "not
/// applicable"). Needed to reach `compare_tau_readings`'s period-shift
/// classification end-to-end, since that path requires `Some(period_size)`
/// on both readings. Unset ⇒ `None`, unchanged from before #348.
pub(super) fn period_size_override() -> Option<u32> {
    static OVERRIDE: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        std::env::var("AC_FAKE_PERIOD_SIZE_OVERRIDE")
            .ok()
            .and_then(|s| s.parse().ok())
    })
}

/// Opt-in, fake-only test hook (#363): lets a test drive the two
/// `measure_tau_twice` lifecycles across *different* declared graph
/// latencies, which is the only way `tau_result`'s
/// `disagree_declared_latency` path can go red under `--fake-audio` — the
/// fake declares nothing by default, and no reachable rig reproduces the
/// sticky one-period state this guard exists for (see #363's 2026-08-23 and
/// 2026-09-16 records).
///
/// `AC_FAKE_DECLARED_LATENCY_FRAMES_OVERRIDE`: comma-separated frame-count
/// list, one value consumed per `declared_latency_frames` call in this
/// process (0-based: the first call gets the first value); a call past the
/// end of the list yields `None`. Unset ⇒ every call yields `None`, i.e. the
/// fake declares nothing, byte-identical to before #363. `measure_tau_twice`
/// makes exactly one such call per lifecycle, so a two-value list is one
/// value per reading.
static DECLARED_LATENCY_CALL_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn declared_latency_override_list() -> &'static [u32] {
    static LIST: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| {
        std::env::var("AC_FAKE_DECLARED_LATENCY_FRAMES_OVERRIDE")
            .ok()
            .map(|s| s.split(',').filter_map(|v| v.trim().parse().ok()).collect())
            .unwrap_or_default()
    })
}

/// Next declared latency, consuming one slot of the override list (see
/// [`DECLARED_LATENCY_CALL_COUNT`] doc above).
pub(super) fn next_declared_latency_frames() -> Option<u32> {
    let call_idx = DECLARED_LATENCY_CALL_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    declared_latency_override_list().get(call_idx).copied()
}

/// Opt-in, fake-only test hooks (#368): let an external integration test
/// simulate a low/no-SNR capture — the muted-route rig case #368's AC3
/// needs reachable under `--fake-audio`, which by default always returns a
/// clean, noiseless delayed copy of the played signal (the loopback shape
/// every other τ test relies on).
///
/// `AC_FAKE_TAU_GAIN_OVERRIDE`: models the loopback cable's own gain, so it
/// scales both `play_and_capture`'s played-signal copy (the τ ESS) and
/// `capture_block`'s tone synthesis (`calibrate` step 2's captured level,
/// via `capture_rms`) — the same cable, read by two different captures.
/// `1.0` (unset) keeps the existing unity loopback on both paths; `0.0`
/// simulates a fully muted route. Before PR #384's codex-qa finding this
/// scaled only `play_and_capture`, so an off-unity gain never reached step
/// 2's `captured_dbfs`/`loopback` fields.
/// `AC_FAKE_TAU_NOISE_AMPLITUDE_OVERRIDE`: peak amplitude of broadband
/// dither added to every sample of `play_and_capture`'s output. `0.0`
/// (unset) is byte-identical to pre-#368 behaviour — with the gain also at
/// its default, `out[j] = 0.0 + s * 1.0 == s`. Combined with a `0.0` gain,
/// the deconvolved IR then contains only the dither at every position, so
/// the peak the daemon finds is indistinguishable from its own noise
/// floor, matching a real muted route's low pre-impulse SNR.
pub(super) fn tau_gain_override() -> f32 {
    static OVERRIDE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        std::env::var("AC_FAKE_TAU_GAIN_OVERRIDE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1.0)
    })
}

pub(super) fn tau_noise_amplitude_override() -> f32 {
    static OVERRIDE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        std::env::var("AC_FAKE_TAU_NOISE_AMPLITUDE_OVERRIDE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0)
    })
}

/// Default reference-leg delay for `play_and_capture_with_reference` (#460).
/// Deliberately different from [`DEFAULT_LOOPBACK_DELAY_SAMPLES`]: a causal
/// bound built by mistake from the measurement leg's peak, or from a stored τ,
/// then lands on a different index than one built from the reference leg, and
/// a test can tell them apart. At 48 kHz, 20 samples of reference τ plus a few
/// centimetres of flight stays below the 32-sample measurement peak (bound
/// enforced); a larger distance crosses it (the at-or-after-peak decline).
pub(super) const DEFAULT_REF_DELAY_SAMPLES: usize = 20;

/// `AC_FAKE_REF_DELAY_SAMPLES`: the reference leg's own delay, in samples.
/// Unset ⇒ [`DEFAULT_REF_DELAY_SAMPLES`].
pub(super) fn ref_delay_samples() -> usize {
    static OVERRIDE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        std::env::var("AC_FAKE_REF_DELAY_SAMPLES")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(DEFAULT_REF_DELAY_SAMPLES)
    })
}

/// `AC_FAKE_REF_GAIN`: the reference cable's own gain. Unset ⇒ `1.0`. `0.0`
/// together with `AC_FAKE_TAU_NOISE_AMPLITUDE_OVERRIDE > 0` leaves only
/// dither on the reference leg, which makes the reference reading's SNR
/// refusal reachable (#460).
pub(super) fn ref_gain() -> f32 {
    static OVERRIDE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        std::env::var("AC_FAKE_REF_GAIN")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1.0)
    })
}

/// Opt-in, fake-only test hook (#369): lets a test drive one or both of
/// `measure_tau_twice`'s two lifecycles across a nonzero xrun count.
/// Without this, `FakeEngine::xruns()` never leaves the 0 it is
/// constructed with, so `tau_result`'s `refused_xrun` path — reachable
/// only when a lifecycle's own `xruns()` delta is nonzero — has no way to
/// go red under `--fake-audio`.
///
/// `AC_FAKE_XRUNS_OVERRIDE`: comma-separated delta list, one value
/// consumed per `play_and_capture` call in this process (0-based: the
/// first call gets the first value); a call past the end of the list adds
/// 0. Unset ⇒ every call adds 0, byte-identical to today's hardcoded-0
/// count. Deliberately scoped to `play_and_capture` alone — sharing this
/// counter with `capture_block` (below) would shift call indices for
/// every unrelated calibrate/monitor path that also captures via
/// `capture_block`, breaking the fixed indexing this doc promises.
static XRUNS_CALL_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn xruns_override_list() -> &'static [u32] {
    static LIST: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| {
        std::env::var("AC_FAKE_XRUNS_OVERRIDE")
            .ok()
            .map(|s| s.split(',').filter_map(|v| v.trim().parse().ok()).collect())
            .unwrap_or_default()
    })
}

/// Next `play_and_capture` xrun delta, consuming one slot of the override
/// list (see [`XRUNS_CALL_COUNT`] doc above).
pub(super) fn next_xruns_delta() -> u32 {
    let call_idx = XRUNS_CALL_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    xruns_override_list().get(call_idx).copied().unwrap_or(0)
}

/// Opt-in, fake-only test hook (#428): lets a test drive a `plot`/
/// `plot_level` sweep's `capture_block` calls across a nonzero xrun count,
/// independent of [`next_xruns_delta`] above. Without this, a sweep that
/// only calls `capture_block` (`plot`, `plot_level` — never
/// `play_and_capture`) has no way to exercise a nonzero session xrun
/// delta under `--fake-audio`, and the #428 fix (report the delta since
/// baseline, not a per-point cumulative sum) has no reproduction outside
/// unit tests.
///
/// `AC_FAKE_CAPTURE_BLOCK_XRUNS_OVERRIDE`: comma-separated delta list, one
/// value consumed per `capture_block` call in this process (0-based). A
/// `plot`/`plot_level` point issues two calls — a discarded 0.1 s warm-up,
/// then the real capture — so both consume a slot. A call past the end of
/// the list adds 0. Unset ⇒ every call adds 0, unchanged from before #428.
static CAPTURE_BLOCK_XRUNS_CALL_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn capture_block_xruns_override_list() -> &'static [u32] {
    static LIST: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| {
        std::env::var("AC_FAKE_CAPTURE_BLOCK_XRUNS_OVERRIDE")
            .ok()
            .map(|s| s.split(',').filter_map(|v| v.trim().parse().ok()).collect())
            .unwrap_or_default()
    })
}

/// Next `capture_block` xrun delta, consuming one slot of the override
/// list (see [`CAPTURE_BLOCK_XRUNS_CALL_COUNT`] doc above).
pub(super) fn next_capture_block_xruns_delta() -> u32 {
    let call_idx =
        CAPTURE_BLOCK_XRUNS_CALL_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    capture_block_xruns_override_list()
        .get(call_idx)
        .copied()
        .unwrap_or(0)
}
