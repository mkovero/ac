# test.py — built-in self-tests for `ac test software` and `ac test hardware`
#
# Software tests: pure-code validation of the analysis pipeline and unit
# conversions. No audio hardware or JACK daemon required.
#
# Hardware tests: run as a server worker, require two loopback pairs
# (output_channel → input_channel and output_channel → reference_channel).
# Optionally cross-check against a DMM over SCPI.

import numpy as np


# ---------------------------------------------------------------------------
# Result container
# ---------------------------------------------------------------------------

class TestResult:
    __slots__ = ("name", "passed", "detail", "tolerance")

    def __init__(self, name, passed, detail, tolerance=""):
        self.name      = name
        self.passed    = passed
        self.detail    = detail
        self.tolerance = tolerance

    def to_dict(self):
        return {"name": self.name, "pass": bool(self.passed),
                "detail": self.detail, "tolerance": self.tolerance}


# ---------------------------------------------------------------------------
# Software tests
# ---------------------------------------------------------------------------

def _make_sine(freq, amp, sr=48000, duration=1.0, harmonics=()):
    n = int(sr * duration)
    t = np.arange(n) / sr
    sig = amp * np.sin(2 * np.pi * freq * t)
    for h_n, h_amp in harmonics:
        sig += h_amp * np.sin(2 * np.pi * freq * h_n * t)
    return sig.reshape(-1, 1).astype(np.float64)


def run_software_tests():
    """Run all software tests, yield TestResult objects."""
    from .server.analysis import analyze
    from .conversions import vrms_to_dbu, dbu_to_vrms, dbfs_to_vrms, vrms_to_vpp
    from .server.jack_calibration import Calibration

    # 1. THD of known 1% second harmonic
    rec = _make_sine(1000, 0.1, harmonics=[(2, 0.001)])
    r = analyze(rec, sr=48000, fundamental=1000)
    thd = r["thd_pct"]
    yield TestResult(
        "THD accuracy (1% H2)",
        abs(thd - 1.0) < 0.05,
        f"{thd:.4f}%", "1.000% +/-0.05%")

    # 2. THD floor on pure sine
    rec = _make_sine(1000, 0.1)
    r = analyze(rec, sr=48000, fundamental=1000)
    thd = r["thd_pct"]
    yield TestResult(
        "THD floor (pure sine)",
        thd < 0.001,
        f"{thd:.6f}%", "< 0.001%")

    # 3. THD+N >= THD
    rec = _make_sine(1000, 0.1, harmonics=[(2, 0.001), (3, 0.0005)])
    r = analyze(rec, sr=48000, fundamental=1000)
    yield TestResult(
        "THD+N >= THD",
        r["thdn_pct"] >= r["thd_pct"],
        f"THD={r['thd_pct']:.4f}%  THD+N={r['thdn_pct']:.4f}%",
        "THD+N must be >= THD")

    # 4. RMS of pure sine = amplitude / sqrt(2)
    rec = _make_sine(1000, 0.5)
    r = analyze(rec, sr=48000, fundamental=1000)
    expected_rms = 0.5 / np.sqrt(2)
    rms_err = abs(r["linear_rms"] - expected_rms) / expected_rms
    yield TestResult(
        "RMS accuracy",
        rms_err < 0.01,
        f"{r['linear_rms']:.6f} (expected {expected_rms:.6f})",
        "+/-1% relative")

    # 5. Fundamental dBFS scaling: 10x amplitude = 20 dB
    r_lo = analyze(_make_sine(1000, 0.1), sr=48000, fundamental=1000)
    r_hi = analyze(_make_sine(1000, 1.0), sr=48000, fundamental=1000)
    delta_db = r_hi["fundamental_dbfs"] - r_lo["fundamental_dbfs"]
    yield TestResult(
        "dBFS scaling (20 dB)",
        abs(delta_db - 20.0) < 0.2,
        f"{delta_db:.2f} dB", "20.0 +/-0.2 dB")

    # 6. THD across frequencies
    failed_freqs = []
    for freq in (100, 440, 1000, 5000, 10000):
        rec = _make_sine(freq, 0.1, harmonics=[(2, 0.001)])
        r = analyze(rec, sr=48000, fundamental=freq)
        if abs(r["thd_pct"] - 1.0) >= 0.15:
            failed_freqs.append(f"{freq}Hz={r['thd_pct']:.3f}%")
    yield TestResult(
        "THD across frequencies",
        len(failed_freqs) == 0,
        "all within tolerance" if not failed_freqs else f"FAILED: {', '.join(failed_freqs)}",
        "1.0% +/-0.15% at 100-10kHz")

    # 7. THD level-independent
    failed_levels = []
    for amp in (0.01, 0.1, 0.5, 0.9):
        rec = _make_sine(1000, amp, harmonics=[(2, amp * 0.01)])
        r = analyze(rec, sr=48000, fundamental=1000)
        if abs(r["thd_pct"] - 1.0) >= 0.15:
            failed_levels.append(f"amp={amp}: {r['thd_pct']:.3f}%")
    yield TestResult(
        "THD level-independent",
        len(failed_levels) == 0,
        "all within tolerance" if not failed_levels else f"FAILED: {', '.join(failed_levels)}",
        "1.0% +/-0.15% at all levels")

    # 8. No-signal detection
    rec = np.zeros((48000, 1), dtype=np.float64)
    r = analyze(rec, sr=48000, fundamental=1000)
    yield TestResult(
        "No-signal detection",
        "error" in r,
        "error returned" if "error" in r else "NO ERROR returned",
        "must return error dict")

    # 9. Unit conversion roundtrips
    max_err = 0
    for v in (0.001, 0.1, 0.5, 1.0, 5.0):
        rt = dbu_to_vrms(vrms_to_dbu(v))
        max_err = max(max_err, abs(rt - v) / v)
    yield TestResult(
        "dBu/Vrms roundtrip",
        max_err < 1e-9,
        f"max relative error: {max_err:.2e}",
        "< 1e-9 relative")

    # 10. dBFS → Vrms
    result = dbfs_to_vrms(-20.0, vrms_at_0dbfs=1.0)
    yield TestResult(
        "dBFS to Vrms",
        abs(result - 0.1) < 1e-9,
        f"{result:.10f} (expected 0.1)",
        "exact to 1e-9")

    # 11. Calibration math
    cal = Calibration()
    cal.vrms_at_0dbfs_out = 2.0
    out_vrms = cal.out_vrms(-6.0)
    expected = 2.0 * 10 ** (-6.0 / 20.0)
    yield TestResult(
        "Calibration out_vrms",
        abs(out_vrms - expected) < 1e-9,
        f"{out_vrms:.6f} (expected {expected:.6f})",
        "exact to 1e-9")

    # 12. Vpp conversion
    vpp = vrms_to_vpp(1.0)
    expected_vpp = 2 * np.sqrt(2)
    yield TestResult(
        "Vrms to Vpp",
        abs(vpp - expected_vpp) < 1e-9,
        f"{vpp:.6f} (expected {expected_vpp:.6f})",
        "exact to 1e-9")


# ---------------------------------------------------------------------------
# Hardware tests (called from server worker)
# ---------------------------------------------------------------------------

def run_noise_floor(engine, in_port_a, in_port_b):
    """Measure noise floor on both inputs with silence on output."""
    from .server.analysis import analyze
    engine.set_silence()
    import time; time.sleep(0.1)

    floors = {}
    for label, port in [("A", in_port_a), ("B", in_port_b)]:
        engine.reconnect_input(port)
        engine._ringbuf.read(engine._ringbuf.read_space)
        import time; time.sleep(0.05)
        data = engine.capture_block(0.5)
        rms = float(np.sqrt(np.mean(data.astype(np.float64) ** 2)))
        dbfs = 20.0 * np.log10(max(rms, 1e-12))
        floors[label] = dbfs

    return TestResult(
        "Noise floor",
        floors["A"] < -80 and floors["B"] < -80,
        f"{floors['A']:.1f} dBFS / {floors['B']:.1f} dBFS",
        "< -80 dBFS")


def run_level_linearity(engine, out_port, in_port):
    """Sweep -42 to -6 dBFS in 6 dB steps, check monotonicity and step accuracy.

    Avoids 0 dBFS (clipping) and below -42 dBFS (noise/quantization affects step accuracy).
    Top step (-12 → -6) uses relaxed tolerance — some interfaces compress near full scale.
    """
    from .server.analysis import analyze
    levels_dbfs = list(range(-42, -5, 6))  # -42, -36, ..., -6
    measured = []

    engine.reconnect_input(in_port)
    for level in levels_dbfs:
        amp = 10.0 ** (level / 20.0)
        engine.set_tone(1000.0, amp)
        engine._ringbuf.read(engine._ringbuf.read_space)
        import time; time.sleep(0.1)
        data = engine.capture_block(1.0)
        rec = data.reshape(-1, 1)
        r = analyze(rec, sr=engine.samplerate, fundamental=1000.0)
        if "error" in r:
            measured.append(float("nan"))
        else:
            measured.append(float(r["fundamental_dbfs"]))

    # Check monotonicity (each step should be higher than the last)
    valid = [(l, m) for l, m in zip(levels_dbfs, measured) if not np.isnan(m)]
    monotonic = all(valid[i][1] < valid[i+1][1] for i in range(len(valid)-1))

    # Check step accuracy: deltas between consecutive measurements should be ~6 dB
    # Top step gets 1.5 dB tolerance (some interfaces compress near full scale)
    deltas = [(valid[i][0], valid[i+1][0], valid[i+1][1] - valid[i][1])
              for i in range(len(valid)-1)]
    max_step_err = 0
    for i, (a, b, d) in enumerate(deltas):
        tol = 1.5 if i == len(deltas) - 1 else 1.0
        max_step_err = max(max_step_err, abs(d - 6.0) / tol)
    passed = monotonic and max_step_err <= 1.0

    step_detail = ", ".join(f"{a}→{b}:{d:.2f}" for a, b, d in deltas)
    return TestResult(
        "Level linearity",
        passed,
        f"[{step_detail}]",
        "monotonic, step error < 1 dB (1.5 dB top step)")


def run_thd_floor(engine, out_port, in_port):
    """THD at 1 kHz across levels — find the sweet spot."""
    from .server.analysis import analyze
    levels = [-40, -30, -20, -10, -3]
    results = []

    engine.reconnect_input(in_port)
    for level in levels:
        amp = 10.0 ** (level / 20.0)
        engine.set_tone(1000.0, amp)
        engine._ringbuf.read(engine._ringbuf.read_space)
        import time; time.sleep(0.05)
        data = engine.capture_block(1.0)
        rec = data.reshape(-1, 1)
        r = analyze(rec, sr=engine.samplerate, fundamental=1000.0)
        if "error" not in r:
            results.append((level, r["thd_pct"], r["thdn_pct"]))

    best_thd = min((thd for _, thd, _ in results), default=float("inf"))
    parts = [f"{lev}dBFS: THD={thd:.4f}% THD+N={thdn:.4f}%" for lev, thd, thdn in results]

    return TestResult(
        "THD floor (1 kHz)",
        best_thd < 0.05,
        f"best {best_thd:.4f}%  [{', '.join(f'{l}:{t:.4f}%' for l,t,_ in results)}]",
        "best THD < 0.05%"), results


def run_freq_response(engine, out_port, in_port):
    """Frequency response at -10 dBFS — should be flat across audio band."""
    from .server.analysis import analyze
    freqs = [50, 100, 500, 1000, 5000, 10000, 20000]
    amp = 10.0 ** (-10.0 / 20.0)
    results = []

    engine.reconnect_input(in_port)
    for freq in freqs:
        engine.set_tone(float(freq), amp)
        engine._ringbuf.read(engine._ringbuf.read_space)
        dur = max(0.5, 20.0 / freq)  # enough cycles for low freqs
        import time; time.sleep(0.05)
        data = engine.capture_block(dur)
        rec = data.reshape(-1, 1)
        r = analyze(rec, sr=engine.samplerate, fundamental=float(freq))
        if "error" not in r:
            results.append((freq, float(r["fundamental_dbfs"])))

    if len(results) < 2:
        return TestResult("Frequency response", False, "insufficient data", "")

    # Reference: 1 kHz level
    ref_db = next((db for f, db in results if f == 1000), results[0][1])
    deviations = [(f, db - ref_db) for f, db in results]
    max_dev = max(abs(d) for _, d in deviations)

    detail_parts = [f"{f}Hz:{d:+.2f}dB" for f, d in deviations]
    return TestResult(
        "Frequency response",
        max_dev < 1.0,
        f"max deviation {max_dev:.2f} dB  [{', '.join(detail_parts)}]",
        "< 1.0 dB vs 1 kHz ref")


def run_channel_match(engine, out_port, in_port_a, in_port_b):
    """Same stimulus, measure both channels — should agree."""
    from .server.analysis import analyze
    amp = 10.0 ** (-10.0 / 20.0)
    engine.set_tone(1000.0, amp)

    measurements = {}
    for label, port in [("A", in_port_a), ("B", in_port_b)]:
        engine.reconnect_input(port)
        engine._ringbuf.read(engine._ringbuf.read_space)
        import time; time.sleep(0.1)
        data = engine.capture_block(1.0)
        rec = data.reshape(-1, 1)
        r = analyze(rec, sr=engine.samplerate, fundamental=1000.0)
        if "error" in r:
            return TestResult("Channel match", False, f"ch {label}: no signal", "")
        measurements[label] = r

    delta_db = abs(measurements["A"]["fundamental_dbfs"] - measurements["B"]["fundamental_dbfs"])
    delta_thd = abs(measurements["A"]["thd_pct"] - measurements["B"]["thd_pct"])

    return TestResult(
        "Channel match",
        delta_db < 0.5 and delta_thd < 0.01,
        f"delta level: {delta_db:.3f} dB  delta THD: {delta_thd:.4f}%",
        "level < 0.5 dB, THD < 0.01%")


def run_channel_isolation(engine, out_port, ref_out_port, in_port_b):
    """Analog crosstalk test: tone on primary output only, measure on reference input.

    Temporarily disconnects the reference output so only the primary output
    sends signal. Measures crosstalk into in_port_b (looped from ref_out_port).
    """
    if ref_out_port == out_port:
        return TestResult(
            "Channel isolation",
            True,
            "skipped — same output feeds both inputs",
            "(only testable with separate outputs)")

    import time

    # Disconnect reference output so only primary sends signal
    ref_port_idx = 1  # ref_out_port is the second output port
    engine.disconnect_output(ref_out_port, port_index=ref_port_idx)

    amp = 10.0 ** (-10.0 / 20.0)
    engine.set_tone(1000.0, amp)
    time.sleep(0.2)

    engine.reconnect_input(in_port_b)
    engine._ringbuf.read(engine._ringbuf.read_space)
    time.sleep(0.05)
    data = engine.capture_block(0.5)
    rms = float(np.sqrt(np.mean(data.astype(np.float64) ** 2)))
    level_dbfs = float(20.0 * np.log10(max(rms, 1e-12)))
    isolation = -10.0 - level_dbfs

    # Reconnect reference output
    engine.connect_output(ref_out_port, port_index=ref_port_idx)

    return TestResult(
        "Channel isolation",
        level_dbfs < -60,
        f"{level_dbfs:.1f} dBFS (isolation: {isolation:.1f} dB)",
        "< -60 dBFS on reference input")


def run_repeatability(engine, out_port, in_port, n_reps=5):
    """Same measurement N times — check variance."""
    from .server.analysis import analyze
    amp = 10.0 ** (-10.0 / 20.0)
    engine.set_tone(1000.0, amp)

    levels = []
    thds = []
    engine.reconnect_input(in_port)
    for _ in range(n_reps):
        engine._ringbuf.read(engine._ringbuf.read_space)
        import time; time.sleep(0.02)
        data = engine.capture_block(1.0)
        rec = data.reshape(-1, 1)
        r = analyze(rec, sr=engine.samplerate, fundamental=1000.0)
        if "error" not in r:
            levels.append(r["fundamental_dbfs"])
            thds.append(r["thd_pct"])

    if len(levels) < 3:
        return TestResult("Repeatability", False, "insufficient measurements", "")

    level_std = float(np.std(levels))
    thd_std = float(np.std(thds))

    return TestResult(
        "Repeatability",
        level_std < 0.05 and thd_std < 0.005,
        f"level sigma={level_std:.4f} dB  THD sigma={thd_std:.6f}%  ({len(levels)}x)",
        "level sigma < 0.05 dB, THD sigma < 0.005%")


# ---------------------------------------------------------------------------
# DMM cross-check tests
# ---------------------------------------------------------------------------

def run_dmm_absolute(engine, out_port, dmm_host, cal):
    """Generate -10 dBFS 1 kHz, read DMM, compare to calibration prediction."""
    from .server import dmm as _dmm
    if cal is None or not cal.output_ok:
        return TestResult("DMM absolute level", False,
                          "no output calibration", "requires calibration")

    amp = 10.0 ** (-10.0 / 20.0)
    engine.set_tone(1000.0, amp)
    import time; time.sleep(0.5)

    try:
        vrms_dmm = _dmm.read_ac_vrms(dmm_host, n=5)
    except Exception as e:
        return TestResult("DMM absolute level", False, f"DMM error: {e}", "")

    vrms_predicted = cal.out_vrms(-10.0)
    err_pct = abs(vrms_dmm - vrms_predicted) / vrms_predicted * 100

    return TestResult(
        "DMM absolute level",
        err_pct < 1.0,
        f"DMM: {vrms_dmm*1000:.3f} mVrms  predicted: {vrms_predicted*1000:.3f} mVrms  delta: {err_pct:.2f}%",
        "< 1% error")


def run_dmm_tracking(engine, out_port, dmm_host, cal):
    """Sweep level, compare each step against DMM Vrms."""
    from .server import dmm as _dmm
    if cal is None or not cal.output_ok:
        return TestResult("DMM level tracking", False,
                          "no output calibration", "requires calibration")

    levels = [-40, -30, -20, -10, -6, -3, 0]
    max_err = 0
    results = []

    for level in levels:
        amp = 10.0 ** (level / 20.0)
        engine.set_tone(1000.0, amp)
        import time; time.sleep(0.4)
        try:
            vrms_dmm = _dmm.read_ac_vrms(dmm_host, n=3)
        except Exception:
            continue
        vrms_pred = cal.out_vrms(float(level))
        err_pct = abs(vrms_dmm - vrms_pred) / vrms_pred * 100
        max_err = max(max_err, err_pct)
        results.append((level, vrms_dmm, vrms_pred, err_pct))

    return TestResult(
        "DMM level tracking",
        max_err < 2.0 and len(results) >= 5,
        f"max error {max_err:.2f}% over {len(results)} points",
        "< 2% error at all levels")


def run_dmm_freq_response(engine, out_port, dmm_host):
    """Same level at multiple frequencies, check DMM reads flat."""
    from .server import dmm as _dmm
    freqs = [100, 1000, 5000, 10000, 20000]
    amp = 10.0 ** (-10.0 / 20.0)
    readings = []

    for freq in freqs:
        engine.set_tone(float(freq), amp)
        import time; time.sleep(0.5)
        try:
            vrms = _dmm.read_ac_vrms(dmm_host, n=3)
            readings.append((freq, vrms))
        except Exception:
            pass

    if len(readings) < 3:
        return TestResult("DMM freq response", False, "insufficient readings", "")

    ref_vrms = next((v for f, v in readings if f == 1000), readings[0][1])
    deviations = [(f, 20 * np.log10(v / ref_vrms)) for f, v in readings]
    max_dev = max(abs(d) for _, d in deviations)

    parts = [f"{f}Hz:{d:+.2f}dB" for f, d in deviations]
    return TestResult(
        "DMM freq response",
        max_dev < 1.0,
        f"max deviation {max_dev:.2f} dB  [{', '.join(parts)}]",
        "< 1.0 dB vs 1 kHz ref")


# ---------------------------------------------------------------------------
# DUT characterization tests (called from server worker)
# ---------------------------------------------------------------------------

def _dbfs_to_dbu(dbfs, cal):
    """Convert dBFS to dBu string using calibration, or return dBFS string."""
    from .conversions import vrms_to_dbu, dbfs_to_vrms
    if cal and cal.input_ok:
        vrms = dbfs_to_vrms(dbfs, cal.vrms_at_0dbfs_in)
        dbu = vrms_to_dbu(vrms)
        return f"{dbu:+.1f} dBu"
    return f"{dbfs:.1f} dBFS"


def _dbfs_to_vrms_str(dbfs, cal):
    """Convert dBFS to Vrms string using calibration."""
    from .conversions import dbfs_to_vrms, fmt_vrms
    if cal and cal.input_ok:
        vrms = dbfs_to_vrms(dbfs, cal.vrms_at_0dbfs_in)
        return fmt_vrms(vrms)
    return f"{dbfs:.1f} dBFS"


def _out_dbfs_to_dbu(dbfs, cal):
    """Convert output dBFS to dBu string using output calibration."""
    from .conversions import vrms_to_dbu, dbfs_to_vrms
    if cal and cal.output_ok:
        vrms = dbfs_to_vrms(dbfs, cal.vrms_at_0dbfs_out)
        dbu = vrms_to_dbu(vrms)
        return f"{dbu:+.1f} dBu"
    return f"{dbfs:.1f} dBFS"


def run_dut_noise_floor(engine, cal=None):
    """Measure DUT output noise with no stimulus."""
    import time
    engine.set_silence()
    time.sleep(0.2)
    data = engine.capture_block(1.0)
    rms = float(np.sqrt(np.mean(data.astype(np.float64) ** 2)))
    dbfs = float(20.0 * np.log10(max(rms, 1e-12)))
    level_str = _dbfs_to_dbu(dbfs, cal)
    return TestResult("Noise floor", True, level_str, "DUT output noise")


def run_dut_gain(engine, level_dbfs=-20.0, cal=None):
    """Measure DUT gain at 1 kHz by comparing measurement vs reference channels."""
    import time
    from .server.analysis import analyze
    amp = 10.0 ** (level_dbfs / 20.0)
    engine.set_tone(1000.0, amp)
    time.sleep(0.2)
    stereo = engine.capture_block_stereo(1.0)
    meas = stereo[:, 0].reshape(-1, 1)
    ref = stereo[:, 1].reshape(-1, 1)

    r_meas = analyze(meas, sr=engine.samplerate, fundamental=1000.0)
    r_ref = analyze(ref, sr=engine.samplerate, fundamental=1000.0)
    if "error" in r_meas:
        return TestResult("Gain", False, "no signal at measurement input", "")
    if "error" in r_ref:
        return TestResult("Gain", False, "no signal at reference input", "")

    meas_db = float(r_meas["fundamental_dbfs"])
    ref_db = float(r_ref["fundamental_dbfs"])
    gain = meas_db - ref_db
    ref_str = _out_dbfs_to_dbu(ref_db, cal)
    meas_str = _dbfs_to_dbu(meas_db, cal)
    return TestResult(
        "Gain",
        True,
        f"{gain:+.1f} dB  (ref: {ref_str} \u2192 meas: {meas_str})",
        "at 1 kHz")


def run_dut_thd_vs_level(engine, cal=None, levels=None):
    """THD and gain at 1 kHz across multiple drive levels."""
    import time
    from .server.analysis import analyze
    if levels is None:
        levels = [-40, -30, -20, -10, -6, -3]
    results = []

    for level in levels:
        amp = 10.0 ** (level / 20.0)
        engine.set_tone(1000.0, amp)
        time.sleep(0.1)
        stereo = engine.capture_block_stereo(1.0)
        meas = stereo[:, 0].reshape(-1, 1)
        ref = stereo[:, 1].reshape(-1, 1)
        r_meas = analyze(meas, sr=engine.samplerate, fundamental=1000.0)
        r_ref = analyze(ref, sr=engine.samplerate, fundamental=1000.0)
        if "error" not in r_meas and "error" not in r_ref:
            gain = float(r_meas["fundamental_dbfs"]) - float(r_ref["fundamental_dbfs"])
            results.append({
                "level": level,
                "thd": float(r_meas["thd_pct"]),
                "thdn": float(r_meas["thdn_pct"]),
                "gain": gain,
                "meas_dbfs": float(r_meas["fundamental_dbfs"]),
            })

    if not results:
        return TestResult("THD vs level", False, "no valid measurements", ""), []

    best_thd = min(r["thd"] for r in results)
    # Show drive level in dBu if calibrated
    parts = []
    for r in results:
        drive = _out_dbfs_to_dbu(float(r["level"]), cal)
        parts.append(f"{drive}:{r['thd']:.4f}%/{r['gain']:+.1f}dB")
    return TestResult(
        "THD vs level",
        True,
        f"best {best_thd:.4f}%  [{', '.join(parts)}]",
        "THD%/gain at each drive level"), results


def run_dut_freq_response(engine, level_dbfs=-20.0, cal=None):
    """Measure DUT frequency response using H1 transfer function estimate."""
    import time
    from .server.transfer import h1_estimate
    amp = 10.0 ** (level_dbfs / 20.0)
    engine.set_pink_noise(amp)
    time.sleep(0.3)

    # Capture 4 seconds for good averaging
    stereo = engine.capture_block_stereo(4.0)
    meas = stereo[:, 0].astype(np.float64)
    ref = stereo[:, 1].astype(np.float64)

    engine.set_silence()

    try:
        tf = h1_estimate(ref, meas, engine.samplerate)
    except Exception as e:
        return TestResult("Frequency response", False, f"H1 failed: {e}", ""), None

    freqs = tf["freqs"]
    mag = tf["magnitude_db"]
    coh = tf["coherence"]

    # Characterize within 50 Hz - 20 kHz
    mask = (freqs >= 50) & (freqs <= 20000)
    if not np.any(mask):
        return TestResult("Frequency response", False, "no data in 50-20kHz", ""), None

    mag_band = mag[mask]
    coh_band = coh[mask]
    ref_db = float(np.median(mag_band))  # use median as reference
    dev_plus = float(np.max(mag_band) - ref_db)
    dev_minus = float(np.min(mag_band) - ref_db)
    avg_coh = float(np.mean(coh_band))

    tf_data = {
        "freqs": tf["freqs"].tolist(),
        "magnitude_db": tf["magnitude_db"].tolist(),
        "phase_deg": tf["phase_deg"].tolist(),
        "coherence": tf["coherence"].tolist(),
        "delay_ms": float(tf["delay_ms"]),
    }

    level_str = _out_dbfs_to_dbu(level_dbfs, cal)
    return TestResult(
        "Frequency response",
        True,
        f"{dev_plus:+.1f}/{dev_minus:+.1f} dB  (50-20kHz, coh {avg_coh:.3f}, delay {tf['delay_ms']:.2f}ms)  at {level_str}",
        "H1 transfer function"), tf_data


def run_dut_clipping_point(engine, cal=None):
    """Find the input level where DUT THD exceeds 1% (clipping onset)."""
    import time
    from .server.analysis import analyze
    from .conversions import dbfs_to_vrms, fmt_vrms
    levels = list(range(-30, 1, 3))  # -30, -27, ..., 0
    last_clean = None
    last_clean_meas_dbfs = None
    clip_level = None
    clip_meas_dbfs = None

    for level in levels:
        amp = 10.0 ** (level / 20.0)
        engine.set_tone(1000.0, amp)
        time.sleep(0.1)
        stereo = engine.capture_block_stereo(0.5)
        meas = stereo[:, 0].reshape(-1, 1)
        r = analyze(meas, sr=engine.samplerate, fundamental=1000.0)
        if "error" in r:
            continue

        thd = float(r["thd_pct"])
        meas_dbfs = float(r["fundamental_dbfs"])
        clipping = r.get("clipping", False)
        if thd > 1.0 or clipping:
            clip_level = level
            clip_meas_dbfs = meas_dbfs
            break
        last_clean = level
        last_clean_meas_dbfs = meas_dbfs

    engine.set_silence()

    if clip_level is not None:
        onset_out = _out_dbfs_to_dbu(float(clip_level), cal)
        clean_out = _out_dbfs_to_dbu(float(last_clean), cal) if last_clean is not None else "?"
        # Show clipping in Vrms at DUT output if calibrated
        clip_vrms = ""
        if cal and cal.input_ok and clip_meas_dbfs is not None:
            v = dbfs_to_vrms(clip_meas_dbfs, cal.vrms_at_0dbfs_in)
            clip_vrms = f"  DUT out: {fmt_vrms(v)}"
        return TestResult(
            "Clipping point",
            True,
            f"onset at {onset_out} (last clean: {clean_out}){clip_vrms}",
            "THD > 1% threshold")
    elif last_clean is not None:
        clean_out = _out_dbfs_to_dbu(float(last_clean), cal)
        clean_vrms = ""
        if cal and cal.input_ok and last_clean_meas_dbfs is not None:
            v = dbfs_to_vrms(last_clean_meas_dbfs, cal.vrms_at_0dbfs_in)
            clean_vrms = f"  DUT out: {fmt_vrms(v)}"
        return TestResult(
            "Clipping point",
            True,
            f"clean through {clean_out} (no clipping detected){clean_vrms}",
            "THD > 1% threshold")
    else:
        return TestResult("Clipping point", False, "no valid measurements", "")
