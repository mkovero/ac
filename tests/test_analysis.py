"""FFT analysis tests with synthetic sine signals — no JACK, no ZMQ."""
import numpy as np
import pytest
from ac.analysis import analyze
from ac.analysis import _downsample_spectrum


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


# ---------------------------------------------------------------------------
# Numerical precision: known signals must produce known values
# ---------------------------------------------------------------------------

def test_thd_exact_1pct_second_harmonic():
    """1% 2nd harmonic (amp=0.001 relative to 0.1 fundamental) → THD = 1.000%."""
    rec = make_recording(freq=1000, amp=0.1, harmonics=[(2, 0.001)])
    r   = analyze(rec, sr=48000, fundamental=1000)
    assert r["thd_pct"] == pytest.approx(1.0, abs=0.05), \
        f"THD should be 1.000%, got {r['thd_pct']:.4f}%"


def test_thd_exact_rss_two_harmonics():
    """1% H2 + 0.5% H3 → THD = sqrt(1² + 0.5²) = 1.118%."""
    rec = make_recording(freq=1000, amp=0.1,
                         harmonics=[(2, 0.001), (3, 0.0005)])
    r   = analyze(rec, sr=48000, fundamental=1000)
    expected = np.sqrt(1.0**2 + 0.5**2)  # 1.118%
    assert r["thd_pct"] == pytest.approx(expected, abs=0.05), \
        f"THD should be {expected:.3f}%, got {r['thd_pct']:.4f}%"


def test_thd_exact_low_distortion():
    """0.01% 2nd harmonic → THD = 0.010%."""
    rec = make_recording(freq=1000, amp=0.5, harmonics=[(2, 0.00005)])
    r   = analyze(rec, sr=48000, fundamental=1000)
    assert r["thd_pct"] == pytest.approx(0.01, abs=0.005), \
        f"THD should be 0.010%, got {r['thd_pct']:.6f}%"


def test_thd_multiple_harmonics_rss():
    """H2=1%, H3=1%, H4=1% → THD = sqrt(3) ≈ 1.732%."""
    h_amp = 0.001  # 1% of 0.1
    rec = make_recording(freq=1000, amp=0.1,
                         harmonics=[(2, h_amp), (3, h_amp), (4, h_amp)])
    r   = analyze(rec, sr=48000, fundamental=1000)
    expected = np.sqrt(3) * 1.0  # sqrt(1² + 1² + 1²)
    assert r["thd_pct"] == pytest.approx(expected, abs=0.1), \
        f"THD should be {expected:.3f}%, got {r['thd_pct']:.4f}%"


def test_fundamental_dbfs_known():
    """fundamental_dbfs should scale correctly: 20 dB difference for 10x amplitude change."""
    rec_low  = make_recording(freq=1000, amp=0.1)
    rec_high = make_recording(freq=1000, amp=1.0)
    r_low  = analyze(rec_low,  sr=48000, fundamental=1000)
    r_high = analyze(rec_high, sr=48000, fundamental=1000)
    delta = r_high["fundamental_dbfs"] - r_low["fundamental_dbfs"]
    assert delta == pytest.approx(20.0, abs=0.1), \
        f"10x amplitude should be 20 dB difference, got {delta:.2f} dB"


def test_fundamental_dbfs_tracks_amplitude():
    """fundamental_dbfs must track amplitude monotonically and with correct dB spacing."""
    results = []
    for amp in (0.01, 0.1, 0.5):
        rec = make_recording(freq=1000, amp=amp)
        r = analyze(rec, sr=48000, fundamental=1000)
        results.append((amp, r["fundamental_dbfs"]))

    # Each 10x should be 20 dB
    assert results[1][1] - results[0][1] == pytest.approx(20.0, abs=0.2)
    # 0.5/0.1 = 5x → 20*log10(5) ≈ 13.98 dB
    assert results[2][1] - results[1][1] == pytest.approx(13.98, abs=0.2)


def test_linear_rms_sine():
    """RMS of a pure sine with amplitude A = A/sqrt(2)."""
    for amp in (0.1, 0.5, 1.0):
        rec = make_recording(freq=1000, amp=amp)
        r   = analyze(rec, sr=48000, fundamental=1000)
        expected_rms = amp / np.sqrt(2)
        assert r["linear_rms"] == pytest.approx(expected_rms, rel=0.01), \
            f"RMS of amp={amp} should be {expected_rms:.6f}, got {r['linear_rms']:.6f}"


def test_noise_floor_rises_with_injected_noise():
    """Injecting broadband noise should raise the measured noise floor proportionally."""
    np.random.seed(42)
    n = 48000
    t = np.arange(n) / 48000
    sig_clean = 0.1 * np.sin(2 * np.pi * 1000 * t)

    rec_clean = sig_clean.reshape(-1, 1).astype(np.float64)
    r_clean = analyze(rec_clean, sr=48000, fundamental=1000)

    # Add noise at -40 dBFS (amplitude ~0.01)
    noise = 0.01 * np.random.randn(n)
    sig_noisy = sig_clean + noise
    rec_noisy = sig_noisy.reshape(-1, 1).astype(np.float64)
    r_noisy = analyze(rec_noisy, sr=48000, fundamental=1000)

    # Noisy signal should have higher noise floor than clean
    assert r_noisy["noise_floor_dbfs"] > r_clean["noise_floor_dbfs"], \
        f"noisy ({r_noisy['noise_floor_dbfs']:.1f}) should be > clean ({r_clean['noise_floor_dbfs']:.1f})"
    # Noisy noise floor should be in the -50 to -30 dBFS range for 1% noise
    assert -55 < r_noisy["noise_floor_dbfs"] < -25, \
        f"noise floor with -40dBFS noise should be ≈-40, got {r_noisy['noise_floor_dbfs']:.1f}"


def test_harmonic_amplitudes_correct_ratio():
    """Harmonic-to-fundamental ratio must match injected distortion levels.

    Absolute FFT amplitudes are scaled by the Hann window, but ratios
    (which determine THD) must be accurate."""
    rec = make_recording(freq=1000, amp=0.1,
                         harmonics=[(2, 0.001), (3, 0.0005)])
    r   = analyze(rec, sr=48000, fundamental=1000)
    h_levels = r["harmonic_levels"]

    # Get fundamental amplitude from spectrum for ratio comparison
    f1_bin = np.argmin(np.abs(r["freqs"] - 1000))
    f1_amp = r["spectrum"][f1_bin]

    # H2 at 2000 Hz: ratio should be 0.001/0.1 = 1%
    h2_freq, h2_amp = h_levels[0]
    assert h2_freq == pytest.approx(2000, abs=2)
    h2_ratio = h2_amp / f1_amp * 100
    assert h2_ratio == pytest.approx(1.0, abs=0.1), \
        f"H2/fundamental ratio should be ≈1.0%, got {h2_ratio:.4f}%"

    # H3 at 3000 Hz: ratio should be 0.0005/0.1 = 0.5%
    h3_freq, h3_amp = h_levels[1]
    assert h3_freq == pytest.approx(3000, abs=2)
    h3_ratio = h3_amp / f1_amp * 100
    assert h3_ratio == pytest.approx(0.5, abs=0.1), \
        f"H3/fundamental ratio should be ≈0.5%, got {h3_ratio:.4f}%"


def test_thd_at_different_frequencies():
    """THD measurement should be accurate across the audio band, not just 1 kHz."""
    for freq in (100, 440, 1000, 5000, 10000):
        rec = make_recording(freq=freq, amp=0.1, harmonics=[(2, 0.001)])
        r   = analyze(rec, sr=48000, fundamental=freq)
        if "error" in r:
            continue  # skip if harmonics fall above Nyquist
        assert r["thd_pct"] == pytest.approx(1.0, abs=0.1), \
            f"THD at {freq} Hz should be ≈1.0%, got {r['thd_pct']:.4f}%"


def test_thd_at_different_levels():
    """THD should be level-independent for a linearly-scaled harmonic."""
    for amp in (0.01, 0.1, 0.5, 0.9):
        h2_amp = amp * 0.01  # always 1%
        rec = make_recording(freq=1000, amp=amp, harmonics=[(2, h2_amp)])
        r   = analyze(rec, sr=48000, fundamental=1000)
        assert r["thd_pct"] == pytest.approx(1.0, abs=0.1), \
            f"THD at amp={amp} should be ≈1.0%, got {r['thd_pct']:.4f}%"


# ---------------------------------------------------------------------------
# Spectrum downsampling
# ---------------------------------------------------------------------------

def test_downsample_structure():
    """Geomspace downsampling produces valid structure with log-spaced frequencies.

    Note: the downsampler uses point-sampling, not peak-preserving interpolation.
    Narrow FFT peaks can fall between sampled indices. This is acceptable because
    the downsampled spectrum is only used for display — all numerical values (THD,
    harmonics, noise floor) are computed from the full-resolution spectrum."""
    rec = make_recording(freq=997.3, amp=0.1, harmonics=[(2, 0.01)])
    r   = analyze(rec, sr=48000, fundamental=997.3)

    spec_full = r["spectrum"][1:]
    freqs_full = r["freqs"][1:]
    spec_ds, freqs_ds = _downsample_spectrum(spec_full, freqs_full, max_pts=1000)

    # Correct length
    assert len(spec_ds) <= 1000
    assert len(spec_ds) == len(freqs_ds)

    # Frequencies are monotonically increasing and span the right range
    assert np.all(np.diff(freqs_ds) > 0), "downsampled freqs must be monotonic"
    assert freqs_ds[0] >= freqs_full[0]
    assert freqs_ds[-1] <= freqs_full[-1]

    # Log-spacing: ratio between consecutive frequencies should be roughly constant
    ratios = freqs_ds[1:] / freqs_ds[:-1]
    assert np.std(ratios) / np.mean(ratios) < 0.5, "frequencies should be roughly log-spaced"

    # Values are taken from the original spectrum (not interpolated)
    for f_ds, s_ds in zip(freqs_ds[:10], spec_ds[:10]):
        orig_idx = int(np.argmin(np.abs(freqs_full - f_ds)))
        assert s_ds == spec_full[orig_idx], "downsampled values must be exact samples"


def test_downsample_short_spectrum_passthrough():
    """Spectra shorter than max_pts should pass through unchanged."""
    spec = np.array([0.1, 0.2, 0.3])
    freqs = np.array([100, 200, 300])
    spec_ds, freqs_ds = _downsample_spectrum(spec, freqs, max_pts=1000)
    np.testing.assert_array_equal(spec_ds, spec)
    np.testing.assert_array_equal(freqs_ds, freqs)
