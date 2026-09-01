//! Opt-in, fake-only test hooks, driven by `AC_FAKE_*` environment
//! variables. Each is read once per process and is inert when unset, so
//! the default fake lifecycle is unchanged by their existence.

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

/// Opt-in, fake-only test hook (#369): lets a test drive one or both of
/// `measure_tau_twice`'s two lifecycles across a nonzero xrun count.
/// Without this, `FakeEngine::xruns()` never leaves the 0 it is
/// constructed with, so `tau_result`'s `refused_xrun` path — reachable
/// only when a lifecycle's own `xruns()` delta is nonzero — has no way to
/// go red under `--fake-audio`.
///
/// `AC_FAKE_XRUNS_OVERRIDE`: comma-separated delta list, one value
/// consumed per `play_and_capture` call in this process (0-based — same
/// call indexing as [`TAU_DELAY_CALL_COUNT`] above, so slot *N* of this
/// list and slot *N* of the delay override line up with the same
/// `measure_tau_twice` lifecycle). A call past the end of the list adds 0.
/// Unset ⇒ every call adds 0, byte-identical to today's hardcoded-0 count.
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
