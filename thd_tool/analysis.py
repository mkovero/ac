# analysis.py
import numpy as np
import scipy.signal as sig
from .constants import SAMPLERATE, FFT_WINDOW, NUM_HARMONICS, FUNDAMENTAL_HZ

def _find_peak(spectrum, freqs, target_hz, tol_hz=20):
    mask = np.abs(freqs - target_hz) < tol_hz
    if not np.any(mask):
        return int(np.argmin(np.abs(freqs - target_hz)))
    return int(np.where(mask)[0][np.argmax(spectrum[mask])])

def analyze(recording, sr=SAMPLERATE, fundamental=FUNDAMENTAL_HZ,
            n_harmonics=NUM_HARMONICS):
    mono  = recording[:, 0].astype(np.float64)
    win   = sig.get_window(FFT_WINDOW, len(mono))
    wc    = np.sqrt(np.mean(win ** 2))
    spec  = np.abs(np.fft.rfft(mono * win)) / (len(mono) / 2) / wc
    freqs = np.fft.rfftfreq(len(mono), 1.0 / sr)

    f1_bin = _find_peak(spec, freqs, fundamental)
    f1_amp = spec[f1_bin]

    if f1_amp < 1e-9:
        return {"error": "No signal -- check connections"}

    h_amps, h_levels = [], []
    for n in range(2, n_harmonics + 2):
        hf = fundamental * n
        if hf > sr / 2:
            break
        hb = _find_peak(spec, freqs, hf)
        h_amps.append(spec[hb])
        h_levels.append((hf, spec[hb]))

    thd = np.sqrt(sum(a**2 for a in h_amps)) / f1_amp * 100.0

    spec_nn = spec.copy()
    bw      = max(1, int(fundamental * 0.1 / (sr / len(mono))))
    spec_nn[max(0, f1_bin - bw): min(len(spec_nn), f1_bin + bw)] = 0.0
    thdn    = np.sqrt(np.mean(spec_nn ** 2)) / f1_amp * 100.0

    fund_dbfs  = 20.0 * np.log10(max(float(f1_amp), 1e-12))

    trim       = int(len(mono) * 0.05)
    linear_rms = float(np.sqrt(np.mean(mono[trim:-trim] ** 2)))

    # Noise floor: RMS of time-domain residual after subtracting fundamental
    # and all harmonics. Comparable to datasheet S/N and dynamic range figures.
    bin_hz     = sr / len(mono)
    notch_bw   = max(1, int(fundamental * 0.03 / bin_hz))  # ±3% of fundamental
    residual   = mono.copy()
    t          = np.arange(len(mono)) / sr
    for n in range(1, n_harmonics + 2):
        hf = fundamental * n
        if hf > sr / 2:
            break
        hb      = _find_peak(spec, freqs, hf)
        hf_real = freqs[hb]
        # reconstruct sine at exact measured frequency and amplitude/phase
        fft_full = np.fft.rfft(mono)
        phase    = np.angle(fft_full[hb])
        amp_time = spec[hb]  # already normalised to peak amplitude
        residual -= amp_time * np.cos(2 * np.pi * hf_real * t + phase)
    residual_rms     = float(np.sqrt(np.mean(residual[trim:-trim] ** 2)))
    noise_floor_dbfs = 20.0 * np.log10(max(residual_rms, 1e-12))

    clipping = bool(np.any(np.abs(mono[trim:-trim]) >= 0.9999))

    # AC-coupling flag: 2nd harmonic dominates (>80% of THD) at low frequencies (<50 Hz).
    # This indicates capacitor coupling causing waveform asymmetry, not real distortion.
    ac_coupled = False
    if fundamental < 50.0 and len(h_amps) >= 1:
        h2_pct = h_amps[0] / f1_amp * 100.0
        ac_coupled = (h2_pct / thd) > 0.80 if thd > 0 else False

    return {
        "fundamental_hz":    fundamental,
        "fundamental_dbfs":  fund_dbfs,
        "linear_rms":        linear_rms,
        "thd_pct":           thd,
        "thdn_pct":          thdn,
        "harmonic_levels":   h_levels,
        "noise_floor_dbfs":  noise_floor_dbfs,
        "spectrum":          spec,
        "freqs":             freqs,
        "clipping":          clipping,
        "ac_coupled":        ac_coupled,
    }
