"""FFT analysis tests with synthetic sine signals — no JACK, no ZMQ."""
import numpy as np
import pytest
from ac.server.analysis import analyze


# ---------------------------------------------------------------------------
# Helper
# ---------------------------------------------------------------------------

def make_recording(freq=1000, amp=0.1, sr=48000, duration=1.0, harmonics=()):
    """Return a (n, 1) float64 array suitable for analyze()."""
    n   = int(sr * duration)
    t   = np.arange(n) / sr
    sig = amp * np.sin(2 * np.pi * freq * t)
    for h_n, h_amp in harmonics:
        sig += h_amp * np.sin(2 * np.pi * freq * h_n * t)
    return sig.reshape(-1, 1).astype(np.float64)


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

def test_pure_sine_keys():
    rec = make_recording(freq=1000, amp=0.1)
    r   = analyze(rec, sr=48000, fundamental=1000)
    assert "error" not in r
    for key in ("thd_pct", "thdn_pct", "fundamental_hz", "fundamental_dbfs",
                "linear_rms", "harmonic_levels", "noise_floor_dbfs",
                "spectrum", "freqs", "clipping", "ac_coupled"):
        assert key in r, f"missing key: {key}"


def test_pure_sine_low_thd():
    rec = make_recording(freq=1000, amp=0.1)
    r   = analyze(rec, sr=48000, fundamental=1000)
    assert r["thd_pct"] < 0.01


def test_sine_with_harmonics():
    rec = make_recording(
        freq=1000, amp=0.1,
        harmonics=[(2, 0.001), (3, 0.0005)]   # 1 % and 0.5 %
    )
    r = analyze(rec, sr=48000, fundamental=1000)
    assert "error" not in r
    # THD should be around sqrt(0.001^2 + 0.0005^2)/0.1 * 100 ≈ 1.1 %
    assert 0.5 < r["thd_pct"] < 5.0
    assert len(r["harmonic_levels"]) >= 2


def test_no_signal_returns_error():
    rec = np.zeros((48000, 1), dtype=np.float64)
    r   = analyze(rec, sr=48000, fundamental=1000)
    assert "error" in r


def test_clipping_detected():
    # Amplitude of 1.0 → peaks reach 1.0 ≥ 0.9999
    rec = make_recording(freq=1000, amp=1.0)
    r   = analyze(rec, sr=48000, fundamental=1000)
    assert "error" not in r
    assert r["clipping"] is True


def test_no_clipping_at_low_level():
    rec = make_recording(freq=1000, amp=0.1)
    r   = analyze(rec, sr=48000, fundamental=1000)
    assert r["clipping"] is False


def test_ac_coupled_flag():
    # fundamental < 50 Hz + 2nd harmonic dominant → ac_coupled = True
    rec = make_recording(freq=30, amp=0.1, harmonics=[(2, 0.05)])
    r   = analyze(rec, sr=48000, fundamental=30)
    assert "error" not in r
    assert r["ac_coupled"]


def test_no_ac_coupled_at_high_freq():
    # fundamental ≥ 50 Hz → ac_coupled stays False even with 2nd harmonic
    rec = make_recording(freq=1000, amp=0.1, harmonics=[(2, 0.05)])
    r   = analyze(rec, sr=48000, fundamental=1000)
    assert r["ac_coupled"] is False


def test_spectrum_shape():
    rec = make_recording(freq=1000, amp=0.1)
    r   = analyze(rec, sr=48000, fundamental=1000)
    assert isinstance(r["freqs"],    np.ndarray)
    assert isinstance(r["spectrum"], np.ndarray)
    assert r["freqs"][0] == 0.0
    assert len(r["freqs"]) == len(r["spectrum"])


def test_fundamental_hz_matches_input():
    for freq in (100, 1000, 10000):
        rec = make_recording(freq=freq, amp=0.1)
        r   = analyze(rec, sr=48000, fundamental=float(freq))
        assert "error" not in r
        assert abs(r["fundamental_hz"] - freq) < 1.0


def test_noise_floor_below_fundamental():
    rec = make_recording(freq=1000, amp=0.1)
    r   = analyze(rec, sr=48000, fundamental=1000)
    # Noise floor should be meaningfully below the fundamental
    assert r["noise_floor_dbfs"] < r["fundamental_dbfs"] - 10.0


# ---------------------------------------------------------------------------
# THD+N tests
# ---------------------------------------------------------------------------

def test_thdn_ge_thd():
    """THD+N must always be >= THD since THD+N includes noise in addition to harmonics."""
    rec = make_recording(freq=1000, amp=0.1, harmonics=[(2, 0.005), (3, 0.002)])
    r   = analyze(rec, sr=48000, fundamental=1000)
    assert "error" not in r
    assert r["thdn_pct"] >= r["thd_pct"], (
        f"THD+N ({r['thdn_pct']:.6f}%) < THD ({r['thd_pct']:.6f}%) — impossible"
    )


def test_thdn_known_value():
    """Synthetic signal with known harmonics: THD+N should be close to THD, not ~150x smaller."""
    # 1% 2nd harmonic + 0.5% 3rd harmonic  → THD ≈ 1.118%
    # THD+N should be within an order of magnitude of THD (not 100x smaller)
    rec = make_recording(freq=1000, amp=0.5,
                         harmonics=[(2, 0.005), (3, 0.0025)])
    r   = analyze(rec, sr=48000, fundamental=1000)
    assert "error" not in r
    thd  = r["thd_pct"]
    thdn = r["thdn_pct"]
    # THD+N must be at least half of THD (noise floor adds a little)
    assert thdn >= thd * 0.5, (
        f"THD+N ({thdn:.6f}%) is more than 2x smaller than THD ({thd:.6f}%) — "
        "likely np.mean vs np.sum bug"
    )
    # And not absurdly large (less than 10x THD)
    assert thdn < thd * 10.0


def test_thdn_pure_sine_reasonable():
    """Pure sine THD+N should be >= THD and less than 1%."""
    rec = make_recording(freq=1000, amp=0.1)
    r   = analyze(rec, sr=48000, fundamental=1000)
    assert "error" not in r
    thdn = r["thdn_pct"]
    # THD+N must always be >= THD (it includes noise + harmonics)
    assert thdn >= r["thd_pct"], (
        f"THD+N ({thdn:.8f}%) < THD ({r['thd_pct']:.8f}%) — impossible"
    )
    # Sanity: not unreasonably large for a clean sine
    assert thdn < 1.0
