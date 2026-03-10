# plotting.py
import os
import numpy as np
import matplotlib.pyplot as plt
import matplotlib.gridspec as gridspec
from .conversions import vrms_to_dbu, fmt_vrms, vrms_to_vpp, dbfs_to_vrms
from .constants import SAMPLERATE

def plot_results(results, device_name="DUT", output_path=None, cal=None):
    if not results:
        return

    clean = [r for r in results if not r.get("clipping", False)] or results

    use_dbu = cal is not None and cal.output_ok
    use_in  = cal is not None and cal.input_ok

    x_vals  = ([vrms_to_dbu(dbfs_to_vrms(r["drive_db"], cal.vrms_at_0dbfs_out))
                for r in clean] if use_dbu
               else [r["drive_db"] for r in clean])
    x_label = "Output Level (dBu)" if use_dbu else "Drive Level (dBFS)"

    in_vals  = ([r["gain_db"] if r.get("gain_db") is not None else np.nan
                 for r in clean] if use_in
                else [r["fundamental_dbfs"] for r in clean])
    in_label = "Gain (dB)" if use_in else "Recorded Level (dBFS)"

    thd  = [r["thd_pct"]  for r in clean]
    thdn = [r["thdn_pct"] for r in clean]

    fig = plt.figure(figsize=(14, 11), facecolor="#0e1117")
    fig.suptitle(f"Distortion Measurement — {device_name}",
                 color="white", fontsize=14, fontweight="bold", y=0.99)

    gs       = gridspec.GridSpec(3, 2, figure=fig, hspace=0.55, wspace=0.32)
    ax_thd   = fig.add_subplot(gs[0, 0])
    ax_thdn  = fig.add_subplot(gs[0, 1])
    ax_level = fig.add_subplot(gs[1, :])
    ax_spec  = fig.add_subplot(gs[2, :])

    for ax in [ax_thd, ax_thdn, ax_level, ax_spec]:
        ax.set_facecolor("#161b22")
        ax.tick_params(colors="#aaaaaa")
        ax.xaxis.label.set_color("#aaaaaa")
        ax.yaxis.label.set_color("#aaaaaa")
        ax.title.set_color("#dddddd")
        for sp in ax.spines.values():
            sp.set_edgecolor("#333333")

    x_min = min(x_vals)
    x_max = max(x_vals)
    x_pad = (x_max - x_min) * 0.05
    x_lo  = x_min - x_pad
    x_hi  = x_max + x_pad

    import math
    step  = 5 if (x_max - x_min) > 20 else 2
    ticks = list(range(math.ceil(x_min / step) * step,
                       math.floor(x_max / step) * step + step, step))
    # always include the first and last data point as labelled ticks
    for v in (x_min, x_max):
        rounded = round(v, 1)
        if not any(abs(t - rounded) < step * 0.4 for t in ticks):
            ticks.append(rounded)
    ticks = sorted(ticks)

    ax_thd.plot(x_vals, thd, color="#4a9eff", linewidth=1.5, marker="o",
                markersize=4)
    ax_thd.set_xlabel(x_label)
    ax_thd.set_ylabel("THD (%)")
    ax_thd.set_title("THD vs Output Level")
    ax_thd.set_xlim(x_lo, x_hi)
    ax_thd.set_xticks(ticks)
    ax_thd.set_xticklabels([str(t) for t in ticks], rotation=45, ha="right", fontsize=8)
    ax_thd.yaxis.set_major_locator(plt.MaxNLocator(nbins=6))
    ax_thd.yaxis.set_major_formatter(plt.FuncFormatter(lambda y, _: f"{y:.4f}%"))
    ax_thd.grid(True, color="#222222", linestyle="--", alpha=0.5)

    ax_thdn.plot(x_vals, thdn, color="#e67e22", linewidth=1.5, marker="o",
                 markersize=4)
    ax_thdn.set_xlabel(x_label)
    ax_thdn.set_ylabel("THD+N (%)")
    ax_thdn.set_title("THD+N vs Output Level")
    ax_thdn.set_xlim(x_lo, x_hi)
    ax_thdn.set_xticks(ticks)
    ax_thdn.set_xticklabels([str(t) for t in ticks], rotation=45, ha="right", fontsize=8)
    ax_thdn.yaxis.set_major_locator(plt.MaxNLocator(nbins=6))
    ax_thdn.yaxis.set_major_formatter(plt.FuncFormatter(lambda y, _: f"{y:.4f}%"))
    ax_thdn.grid(True, color="#222222", linestyle="--", alpha=0.5)

    ax_level.plot(x_vals, in_vals, color="#a29bfe", linewidth=1.8,
                  marker="o", markersize=4, label="DUT output (received)")
    ax_level.axhline(0, color="#444444", linewidth=1.0,
                     linestyle="--", label="Unity gain (0 dB)")

    ax_level.set_xlabel(x_label)
    ax_level.set_ylabel(in_label)
    ax_level.set_title("Signal Level: Sent -> Received  (gain / compression / clipping)")
    ax_level.grid(True, color="#222222", linestyle="--", alpha=0.5)
    ax_level.legend(facecolor="#1e2530", labelcolor="white", fontsize=8)

    last     = clean[-1]
    spec     = last["spectrum"].copy()
    freqs    = last["freqs"]
    sr_bin   = freqs[1] - freqs[0]  # Hz per bin
    f1       = last["fundamental_hz"]
    # notch the fundamental: zero out +/- 3% of f1
    notch_hz = f1 * 0.03
    mask     = np.abs(freqs - f1) < notch_hz
    spec[mask] = 1e-12
    spec_db  = 20.0 * np.log10(np.maximum(spec, 1e-12))
    ax_spec.plot(freqs, spec_db, color="#4a9eff", linewidth=0.8)
    for i, (hf, ha) in enumerate(last["harmonic_levels"][:6]):
        h_db = 20.0 * np.log10(max(ha, 1e-12))
        ax_spec.axvline(hf, color="#e74c3c", linestyle="--",
                        linewidth=0.8, alpha=0.6,
                        label=f"H{i+2}" if i < 4 else None)
        ax_spec.annotate(f"H{i+2}\n{h_db:.0f}dB",
                         xy=(hf, h_db), xytext=(4, 0),
                         textcoords="offset points",
                         color="#e74c3c", fontsize=6, va="center")

    ax_spec.set_xscale("log")
    ax_spec.set_xlim(20, SAMPLERATE / 2)
    ax_spec.set_ylim(-140, 10)
    ax_spec.set_xlabel("Frequency (Hz)")
    ax_spec.set_ylabel("Level (dBFS)")

    last_x  = f"{x_vals[-1]:+.1f} dBu" if use_dbu else f"{x_vals[-1]:.0f} dBFS"
    last_in = ""
    if use_in and last.get("in_vrms"):
        last_in = (f"  |  DUT out: {fmt_vrms(last['in_vrms'])}"
                   f" = {vrms_to_dbu(last['in_vrms']):+.1f} dBu")
    ax_spec.set_title(
        f"Spectrum at {last_x}{last_in}"
        f"  --  THD={last['thd_pct']:.3f}%  THD+N={last['thdn_pct']:.3f}%"
    )
    ax_spec.grid(True, color="#222222", linestyle="--", alpha=0.5)
    ax_spec.legend(facecolor="#1e2530", labelcolor="white", fontsize=8, ncol=5)

    plt.tight_layout(rect=[0, 0, 1, 0.97])

    if output_path:
        plt.savefig(output_path, dpi=150, bbox_inches="tight",
                    facecolor=fig.get_facecolor())
        print(f"  Plot saved -> {output_path}")
    else:
        plt.show()
    plt.close()
