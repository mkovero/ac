# transfer.py -- H1 transfer function estimator
import numpy as np
from scipy.signal import welch, csd, correlate, correlation_lags


def h1_estimate(ref, meas, sr, nperseg=None, noverlap=None, window='hann'):
    """H1 transfer function estimate via Welch averaging.

    Parameters
    ----------
    ref   : 1-D float array — reference channel (x)
    meas  : 1-D float array — measurement channel (y), same length as ref
    sr    : sample rate (Hz)
    nperseg  : FFT segment length (default: sr → 1 Hz resolution)
    noverlap : segment overlap  (default: nperseg // 2)
    window   : window function  (default: 'hann')

    Returns
    -------
    dict with keys:
        freqs         : 1-D array of frequency bins (Hz)
        magnitude_db  : 20·log10(|H1|)
        phase_deg     : unwrapped phase in degrees
        coherence     : γ²(f), 0..1
        delay_samples : estimated integer-sample delay (ref→meas)
        delay_ms      : delay in milliseconds
    """
    ref = np.asarray(ref, dtype=np.float64)
    meas = np.asarray(meas, dtype=np.float64)
    assert len(ref) == len(meas), "ref and meas must have equal length"

    if nperseg is None:
        nperseg = int(sr)
    if noverlap is None:
        noverlap = nperseg // 2

    # --- Delay estimation via cross-correlation ---
    # Use a bounded lag search to keep computation reasonable
    max_lag = min(len(ref) // 2, int(sr))  # up to 1 second of delay
    # Trim signals for correlation to avoid huge FFTs
    corr_len = min(len(ref), 4 * int(sr))  # use up to 4 seconds
    corr = correlate(meas[:corr_len], ref[:corr_len], mode='full')
    lags = correlation_lags(corr_len, corr_len, mode='full')
    # Restrict to plausible lag range
    mask = np.abs(lags) <= max_lag
    corr_masked = corr[mask]
    lags_masked = lags[mask]
    delay_samples = int(lags_masked[np.argmax(np.abs(corr_masked))])
    delay_ms = delay_samples / sr * 1000.0

    # --- Welch spectral estimates ---
    freqs, Gxx = welch(ref, fs=sr, nperseg=nperseg, noverlap=noverlap,
                       window=window)
    _, Gyy = welch(meas, fs=sr, nperseg=nperseg, noverlap=noverlap,
                   window=window)
    freqs_csd, Gxy = csd(ref, meas, fs=sr, nperseg=nperseg,
                         noverlap=noverlap, window=window)

    # --- Coherence (computed before delay compensation — unaffected) ---
    denom = Gxx * Gyy
    denom_safe = np.where(denom > 0, denom, 1.0)
    coherence = np.abs(Gxy) ** 2 / denom_safe
    coherence = np.clip(coherence, 0.0, 1.0)

    # --- Delay compensation in frequency domain ---
    # Remove linear phase ramp from path latency
    phase_shift = np.exp(1j * 2 * np.pi * freqs * delay_samples / sr)
    Gxy_comp = Gxy * phase_shift

    # --- H1 estimator ---
    Gxx_safe = np.where(Gxx > 0, Gxx, 1e-30)
    H1 = Gxy_comp / Gxx_safe

    magnitude = np.abs(H1)
    magnitude = np.clip(magnitude, 1e-6, None)  # floor at -120 dB
    magnitude_db = 20.0 * np.log10(magnitude)

    phase_deg = np.degrees(np.angle(H1))  # wrapped to ±180°

    return {
        "freqs":         freqs,
        "magnitude_db":  magnitude_db,
        "phase_deg":     phase_deg,
        "coherence":     coherence,
        "delay_samples": delay_samples,
        "delay_ms":      delay_ms,
    }


def capture_duration(n_averages, nperseg, noverlap, sr):
    """Return seconds of capture needed for n_averages Welch segments."""
    step = nperseg - noverlap
    total_samples = nperseg + step * (n_averages - 1)
    return total_samples / sr
