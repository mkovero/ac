# plotting.py
import warnings
import numpy as np
import matplotlib
matplotlib.use("TkAgg")
import matplotlib.pyplot as plt
import matplotlib.gridspec as gridspec
from ..conversions import vrms_to_dbu, fmt_vrms, dbfs_to_vrms
from ..constants import SAMPLERATE

# Suppress noisy matplotlib warnings
warnings.filterwarnings("ignore", category=UserWarning, module="matplotlib")

_DARK_BG    = "#0e1117"
_PANEL_BG   = "#161b22"
_GRID       = "#222222"
_TEXT       = "#aaaaaa"
_TITLE      = "#dddddd"
_SPINE      = "#333333"
_BLUE       = "#4a9eff"
_ORANGE     = "#e67e22"
_PURPLE     = "#a29bfe"
_RED        = "#e74c3c"

_FREQ_TICKS = [20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000]


def _hz_fmt(x, _):
    if x >= 1000:
        v = x / 1000
        return f"{v:.0f}k" if v == int(v) else f"{v:.1f}k"
    return f"{x:.0f}"


def _style_ax(ax):
    ax.set_facecolor(_PANEL_BG)
    ax.tick_params(colors=_TEXT)
    ax.xaxis.label.set_color(_TEXT)
    ax.yaxis.label.set_color(_TEXT)
    ax.title.set_color(_TITLE)
    for sp in ax.spines.values():
        sp.set_edgecolor(_SPINE)
    ax.grid(True, color=_GRID, linestyle="--", alpha=0.5)


def _pct_fmt(y, _):
    return f"{y:.4f}%"


def plot_results(results, device_name="DUT", output_path=None, cal=None, show=False):
    if not results:
        return

    # Exclude clipped and AC-coupled points from plots
    valid = [r for r in results
             if not r.get("clipping") and not r.get("ac_coupled")] or results

    is_freq_sweep = "freq" in valid[0] and len(set(r.get("drive_db", 0) for r in valid)) <= 2

    if is_freq_sweep:
        _plot_freq_sweep(valid, device_name, output_path, cal, show=show)
    else:
        _plot_level_sweep(valid, device_name, output_path, cal, show=show)


# ---------------------------------------------------------------------------
# Frequency sweep plot
# ---------------------------------------------------------------------------

def _plot_freq_sweep(results, device_name, output_path, cal, show=False):
    use_in = cal is not None and cal.input_ok

    freqs_x = [r["freq"] for r in results]
    thd     = [r["thd_pct"]  for r in results]
    thdn    = [r["thdn_pct"] for r in results]
    gain    = [r["gain_db"]  if r.get("gain_db") is not None else np.nan
               for r in results]

    fig = plt.figure(figsize=(14, 10), facecolor=_DARK_BG)
    fig.suptitle(f"Frequency Sweep — {device_name}",
                 color="white", fontsize=14, fontweight="bold", y=0.99)

    gs      = gridspec.GridSpec(3, 1, figure=fig, hspace=0.55)
    ax_thd  = fig.add_subplot(gs[0])
    ax_gain = fig.add_subplot(gs[1])
    ax_thdn = fig.add_subplot(gs[2])

    x_lo = max(10, min(freqs_x) * 0.8)
    x_hi = min(SAMPLERATE / 2, max(freqs_x) * 1.2)

    for ax in [ax_thd, ax_gain, ax_thdn]:
        _style_ax(ax)
        ax.set_xscale("log")
        ax.set_xlim(x_lo, x_hi)
        ax.set_xlabel("Frequency (Hz)")
        ax.xaxis.set_major_formatter(plt.FuncFormatter(_hz_fmt))
        ax.xaxis.set_minor_formatter(plt.FuncFormatter(_hz_fmt))
        ax.set_xticks([t for t in _FREQ_TICKS if x_lo <= t <= x_hi])

    # THD
    ax_thd.plot(freqs_x, thd, color=_BLUE, linewidth=1.5, marker="o", markersize=4)
    ax_thd.set_ylabel("THD (%)")
    ax_thd.set_title("THD vs Frequency")
    ax_thd.yaxis.set_major_formatter(plt.FuncFormatter(_pct_fmt))
    if max(thd) > 0:
        ax_thd.set_ylim(bottom=0)

    # Gain / frequency response
    ax_gain.plot(freqs_x, gain, color=_PURPLE, linewidth=1.5, marker="o", markersize=4)
    ax_gain.axhline(0, color="#444444", linewidth=1.0, linestyle="--")
    ax_gain.set_ylabel("Gain (dB)" if use_in else "Level (dBFS)")
    ax_gain.set_title("Frequency Response")
    ax_gain.yaxis.set_major_formatter(plt.FuncFormatter(lambda y, _: f"{y:+.2f} dB"))

    # THD+N
    ax_thdn.plot(freqs_x, thdn, color=_ORANGE, linewidth=1.5, marker="o", markersize=4)
    ax_thdn.set_ylabel("THD+N (%)")
    ax_thdn.set_title("THD+N vs Frequency")
    ax_thdn.yaxis.set_major_formatter(plt.FuncFormatter(_pct_fmt))
    if max(thdn) > 0:
        ax_thdn.set_ylim(bottom=0)

    plt.tight_layout(rect=[0, 0, 1, 0.97])
    _save_or_show(fig, output_path, show=show)


# ---------------------------------------------------------------------------
# Level sweep plot
# ---------------------------------------------------------------------------

def _draw_spectrum(ax, r, title):
    """Draw spectrum for result r into ax (used for initial render and click updates)."""
    if r.get("spectrum") is None:
        return
    ax.cla()
    _style_ax(ax)
    spec  = r["spectrum"].copy()
    freqs = r["freqs"]
    f1    = r.get("fundamental_hz", 0) or 1
    mask  = np.abs(freqs - f1) < f1 * 0.03
    spec[mask] = 1e-12
    spec_db = 20.0 * np.log10(np.maximum(spec, 1e-12))
    ax.plot(freqs, spec_db, color=_BLUE, linewidth=0.8)

    labeled = 0
    for i, (hf, ha) in enumerate((r.get("harmonic_levels") or [])[:6]):
        h_db  = 20.0 * np.log10(max(ha, 1e-12))
        label = f"H{i+2}" if labeled < 4 else None
        ax.axvline(hf, color=_RED, linestyle="--", linewidth=0.8, alpha=0.6, label=label)
        ax.annotate(f"H{i+2}\n{h_db:.0f}dB",
                    xy=(hf, h_db), xytext=(4, 0),
                    textcoords="offset points",
                    color=_RED, fontsize=6, va="center")
        labeled += 1

    ax.set_xscale("log")
    ax.set_xlim(20, SAMPLERATE / 2)
    ax.set_ylim(-140, 10)
    ax.set_xlabel("Frequency (Hz)")
    ax.set_ylabel("Level (dBFS)")
    ax.xaxis.set_major_formatter(plt.FuncFormatter(_hz_fmt))
    ax.set_xticks([t for t in _FREQ_TICKS if t <= SAMPLERATE / 2])
    ax.set_title(title)
    if labeled > 0:
        ax.legend(facecolor="#1e2530", labelcolor="white", fontsize=8, ncol=5)


def _plot_level_sweep(results, device_name, output_path, cal, show=False):
    use_dbu = cal is not None and cal.output_ok
    use_in  = cal is not None and cal.input_ok

    x_vals  = ([vrms_to_dbu(dbfs_to_vrms(r["drive_db"], cal.vrms_at_0dbfs_out))
                for r in results] if use_dbu
               else [r["drive_db"] for r in results])
    x_label = "Output Level (dBu)" if use_dbu else "Drive Level (dBFS)"

    in_vals  = ([r["gain_db"] if r.get("gain_db") is not None else np.nan
                 for r in results] if use_in
                else [r["fundamental_dbfs"] for r in results])
    in_label = "Gain (dB)" if use_in else "Recorded Level (dBFS)"

    thd  = [r["thd_pct"]  for r in results]
    thdn = [r["thdn_pct"] for r in results]

    fig = plt.figure(figsize=(14, 11), facecolor=_DARK_BG)
    fig.suptitle(f"Distortion Measurement — {device_name}",
                 color="white", fontsize=14, fontweight="bold", y=0.99)

    gs       = gridspec.GridSpec(3, 2, figure=fig, hspace=0.55, wspace=0.32)
    ax_thd   = fig.add_subplot(gs[0, 0])
    ax_thdn  = fig.add_subplot(gs[0, 1])
    ax_level = fig.add_subplot(gs[1, :])
    ax_spec  = fig.add_subplot(gs[2, :])

    for ax in [ax_thd, ax_thdn, ax_level, ax_spec]:
        _style_ax(ax)

    # Safe x-axis limits
    x_min = min(x_vals)
    x_max = max(x_vals)
    x_pad = max((x_max - x_min) * 0.05, 0.5)
    x_lo  = x_min - x_pad
    x_hi  = x_max + x_pad

    import math
    span  = x_max - x_min
    step  = 5 if span > 20 else 2 if span > 5 else 1
    ticks = list(range(math.ceil(x_min / step) * step,
                       math.floor(x_max / step) * step + step, step))
    for v in (x_min, x_max):
        rv = round(v, 1)
        if not any(abs(t - rv) < step * 0.4 for t in ticks):
            ticks.append(rv)
    ticks = sorted(ticks)

    def _set_x(ax):
        ax.set_xlim(x_lo, x_hi)
        ax.set_xticks(ticks)
        ax.set_xticklabels([str(t) for t in ticks], rotation=45, ha="right", fontsize=8)
        ax.set_xlabel(x_label)

    ax_thd.plot(x_vals, thd, color=_BLUE, linewidth=1.5, marker="o", markersize=4)
    ax_thd.set_ylabel("THD (%)")
    ax_thd.set_title("THD vs Output Level")
    ax_thd.yaxis.set_major_formatter(plt.FuncFormatter(_pct_fmt))
    _set_x(ax_thd)

    ax_thdn.plot(x_vals, thdn, color=_ORANGE, linewidth=1.5, marker="o", markersize=4)
    ax_thdn.set_ylabel("THD+N (%)")
    ax_thdn.set_title("THD+N vs Output Level")
    ax_thdn.yaxis.set_major_formatter(plt.FuncFormatter(_pct_fmt))
    _set_x(ax_thdn)

    ax_level.plot(x_vals, in_vals, color=_PURPLE, linewidth=1.8,
                  marker="o", markersize=4, label="DUT output (received)")
    ax_level.axhline(0, color="#444444", linewidth=1.0, linestyle="--", label="0 dB")
    ax_level.set_ylabel(in_label)
    ax_level.set_title("Signal Level: Sent → Received")
    ax_level.legend(facecolor="#1e2530", labelcolor="white", fontsize=8)
    _set_x(ax_level)

    # Initial spectrum: last valid point
    last_x = f"{x_vals[-1]:+.1f} dBu" if use_dbu else f"{x_vals[-1]:.0f} dBFS"
    _draw_spectrum(ax_spec, results[-1],
                   f"Spectrum at {last_x}"
                   f"  --  THD={results[-1]['thd_pct']:.3f}%  THD+N={results[-1]['thdn_pct']:.3f}%")

    # Click-to-spectrum: click a point on any top subplot to update spectrum view;
    # double-click resets to the last point.
    def _on_click(event):
        if event.inaxes not in (ax_thd, ax_thdn, ax_level):
            return
        if event.dblclick or event.xdata is None:
            idx = len(results) - 1
        else:
            idx = min(range(len(x_vals)), key=lambda i: abs(x_vals[i] - event.xdata))
        r   = results[idx]
        xv  = x_vals[idx]
        lbl = f"{xv:+.1f} dBu" if use_dbu else f"{xv:.0f} dBFS"
        _draw_spectrum(ax_spec, r,
                       f"Spectrum at {lbl}  --  THD={r['thd_pct']:.3f}%  THD+N={r['thdn_pct']:.3f}%")
        fig.canvas.draw_idle()

    fig.canvas.mpl_connect("button_press_event", _on_click)

    plt.tight_layout(rect=[0, 0, 1, 0.97])
    _save_or_show(fig, output_path, show=show)


# ---------------------------------------------------------------------------
# Transfer function plot
# ---------------------------------------------------------------------------

def plot_transfer(result, device_name="DUT", output_path=None, show=False):
    """3-panel plot: magnitude, phase, coherence vs frequency."""
    freqs = result["freqs"]
    mag   = result["magnitude_db"]
    phase = result["phase_deg"]
    coh   = result["coherence"]

    fig = plt.figure(figsize=(14, 10), facecolor=_DARK_BG)
    delay_ms = result.get("delay_ms", 0.0)
    fig.suptitle(f"Transfer Function (H1) — {device_name}   "
                 f"[delay {delay_ms:.3f} ms]",
                 color="white", fontsize=14, fontweight="bold", y=0.99)

    gs      = gridspec.GridSpec(3, 1, figure=fig, hspace=0.55)
    ax_mag  = fig.add_subplot(gs[0])
    ax_ph   = fig.add_subplot(gs[1])
    ax_coh  = fig.add_subplot(gs[2])

    x_lo = max(10, freqs[1] * 0.8) if len(freqs) > 1 else 10
    x_hi = min(SAMPLERATE / 2, freqs[-1] * 1.2)

    for ax in [ax_mag, ax_ph, ax_coh]:
        _style_ax(ax)
        ax.set_xscale("log")
        ax.set_xlim(x_lo, x_hi)
        ax.set_xlabel("Frequency (Hz)")
        ax.xaxis.set_major_formatter(plt.FuncFormatter(_hz_fmt))
        ax.xaxis.set_minor_formatter(plt.FuncFormatter(_hz_fmt))
        ax.set_xticks([t for t in _FREQ_TICKS if x_lo <= t <= x_hi])

    # Magnitude
    ax_mag.plot(freqs, mag, color=_BLUE, linewidth=1.2)
    ax_mag.axhline(0, color="#444444", linewidth=1.0, linestyle="--")
    ax_mag.set_ylabel("Magnitude (dB)")
    ax_mag.set_title("Magnitude Response")
    ax_mag.yaxis.set_major_formatter(
        plt.FuncFormatter(lambda y, _: f"{y:+.1f}"))

    # Phase
    ax_ph.plot(freqs, phase, color=_PURPLE, linewidth=1.2)
    ax_ph.axhline(0, color="#444444", linewidth=1.0, linestyle="--")
    ax_ph.set_ylabel("Phase (\u00b0)")
    ax_ph.set_title("Phase Response")
    ax_ph.yaxis.set_major_formatter(
        plt.FuncFormatter(lambda y, _: f"{y:+.0f}\u00b0"))

    # Coherence
    ax_coh.plot(freqs, coh, color=_ORANGE, linewidth=1.2)
    ax_coh.axhline(0.95, color=_RED, linewidth=1.0, linestyle="--",
                   alpha=0.7, label="\u03b3\u00b2 = 0.95")
    ax_coh.set_ylabel("Coherence (\u03b3\u00b2)")
    ax_coh.set_title("Coherence")
    ax_coh.set_ylim(-0.05, 1.05)
    ax_coh.legend(facecolor="#1e2530", labelcolor="white", fontsize=8)

    plt.tight_layout(rect=[0, 0, 1, 0.97])
    _save_or_show(fig, output_path, show=show)


def _save_or_show(fig, output_path, show=False):
    if output_path:
        fig.savefig(output_path, dpi=150, bbox_inches="tight",
                    facecolor=fig.get_facecolor())
        print(f"  Plot saved -> {output_path}")
        if show:
            try:
                plt.show()
            except Exception:
                pass
    else:
        plt.show()
    plt.close(fig)
