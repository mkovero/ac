"""H1 transfer function estimator unit tests — pure numpy, no audio hardware."""
import numpy as np
import pytest
from ac.server.transfer import h1_estimate, capture_duration


SR = 48000


def _white_noise(n, seed=42):
    rng = np.random.default_rng(seed)
    return rng.standard_normal(n).astype(np.float64)


class TestH1Unity:
    """y = x (unity passthrough) — expect 0 dB, 0° phase, coherence ≈ 1."""

    def setup_method(self):
        x = _white_noise(SR * 4)
        self.result = h1_estimate(x, x, SR)

    def test_magnitude_near_zero_db(self):
        mag = self.result["magnitude_db"]
        # Exclude DC bin
        assert np.allclose(mag[1:], 0.0, atol=0.01)

    def test_phase_near_zero(self):
        phase = self.result["phase_deg"]
        assert np.allclose(phase[1:], 0.0, atol=0.1)

    def test_coherence_near_one(self):
        coh = self.result["coherence"]
        assert np.all(coh[1:] > 0.999)

    def test_delay_zero(self):
        assert self.result["delay_samples"] == 0
        assert abs(self.result["delay_ms"]) < 0.1


class TestH1KnownGain:
    """y = 2*x — expect +6.02 dB, 0° phase, coherence ≈ 1."""

    def setup_method(self):
        x = _white_noise(SR * 4)
        y = 2.0 * x
        self.result = h1_estimate(x, y, SR)

    def test_magnitude_6db(self):
        mag = self.result["magnitude_db"]
        assert np.allclose(mag[1:], 6.02, atol=0.05)

    def test_coherence_near_one(self):
        coh = self.result["coherence"]
        assert np.all(coh[1:] > 0.999)


class TestH1Delay:
    """y = delayed x — expect delay detected, phase ≈ 0 after compensation."""

    @pytest.fixture(autouse=True, params=[10, 100, 480])
    def setup_delay(self, request):
        self.delay = request.param
        x = _white_noise(SR * 4)
        y = np.roll(x, self.delay)
        self.result = h1_estimate(x, y, SR)

    def test_delay_detected(self):
        assert self.result["delay_samples"] == self.delay

    def test_magnitude_near_zero(self):
        mag = self.result["magnitude_db"]
        # Edge bins can be noisy due to roll wrap-around; check bulk
        n = len(mag)
        bulk = mag[n // 10 : n * 9 // 10]
        assert np.allclose(bulk, 0.0, atol=1.0)

    def test_phase_near_zero_after_compensation(self):
        phase = self.result["phase_deg"]
        n = len(phase)
        bulk = phase[n // 10 : n * 9 // 10]
        assert np.allclose(bulk, 0.0, atol=5.0)


class TestH1Uncorrelated:
    """Independent x, y — expect low coherence."""

    def setup_method(self):
        x = _white_noise(SR * 4, seed=1)
        y = _white_noise(SR * 4, seed=2)
        self.result = h1_estimate(x, y, SR)

    def test_low_coherence(self):
        coh = self.result["coherence"]
        assert np.mean(coh[1:]) < 0.2


class TestH1LowPass:
    """y = lowpass(x) — verify roll-off matches filter."""

    def setup_method(self):
        from scipy.signal import butter, lfilter
        x = _white_noise(SR * 8)
        b, a = butter(2, 1000.0 / (SR / 2))
        y = lfilter(b, a, x)
        self.result = h1_estimate(x, y, SR, nperseg=SR)

    def test_passband_near_zero_db(self):
        freqs = self.result["freqs"]
        mag = self.result["magnitude_db"]
        passband = mag[(freqs > 50) & (freqs < 500)]
        assert np.allclose(passband, 0.0, atol=0.5)

    def test_stopband_rolloff(self):
        freqs = self.result["freqs"]
        mag = self.result["magnitude_db"]
        # At 10 kHz, 2nd-order Butterworth with 1 kHz cutoff should be ~-40 dB
        idx_10k = np.argmin(np.abs(freqs - 10000))
        assert mag[idx_10k] < -30


class TestCaptureDuration:
    def test_basic(self):
        dur = capture_duration(16, SR, SR // 2, SR)
        # 16 averages, nperseg=48000, step=24000 → 48000 + 15*24000 = 408000 samples
        assert abs(dur - 408000 / SR) < 0.01
