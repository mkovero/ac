# live.py
import sys
import time
import sounddevice as sd
from .signal import make_sine
from .conversions import fmt_vrms, fmt_vpp, vrms_to_dbu
from .analysis import analyze
from .conversions import fmt_vrms, vrms_to_dbu, vrms_to_vpp
from .constants import SAMPLERATE, LIVE_DURATION, WARMUP_REPS

def _thd_bar(thd_pct, width=20):
    import math
    try:
        norm = (math.log10(max(thd_pct, 0.0001)) + 3) / 3.0
    except Exception:
        norm = 0.0
    norm    = max(0.0, min(norm, 1.0))
    filled  = int(norm * width)
    bar     = "█" * filled + "░" * (width - filled)

    if norm < 0.4:
        color = "\033[32m"
    elif norm < 0.7:
        color = "\033[33m"
    else:
        color = "\033[31m"
    reset = "\033[0m"
    return f"{color}[{bar}]{reset}"

def _trim_bar(delta_db, width=30):
    """Visual bar showing how far input dBu is from target. Centre = on target."""
    import math
    # Scale: ±6 dB range maps to full bar width
    range_db  = 6.0
    norm      = delta_db / range_db          # -1.0 .. +1.0
    norm      = max(-1.0, min(1.0, norm))
    centre    = width // 2
    pos       = max(0, min(width - 1, int(centre + norm * centre)))  # clamp to bar
    bar       = list("─" * width)
    bar[centre] = "┼"                        # centre marker = target
    needle    = "▶" if delta_db > 0 else ("◀" if delta_db < 0 else "●")
    bar[pos]  = needle
    bar_str   = "".join(bar)
    if abs(delta_db) < 0.1:
        color = "\033[32m"   # green = on target
    elif abs(delta_db) < 0.5:
        color = "\033[33m"   # yellow = close
    else:
        color = "\033[31m"   # red = far off
    reset = "\033[0m"
    return f"{color}[{bar_str}]{reset}"


def live_monitor(input_device, output_device,
                 input_channel=0, output_channel=0,
                 level_dbfs=-12.0, freq=1000,
                 cal=None, target_vrms=None):
    amplitude   = 10.0 ** (level_dbfs / 20.0)
    tone        = make_sine(amplitude, freq, LIVE_DURATION)

    if cal and cal.output_ok:
        out_v    = cal.out_vrms(level_dbfs)
        tone_str = (f"{freq:.0f} Hz  {level_dbfs:.0f} dBFS  "
                    f"{fmt_vrms(out_v)}  {vrms_to_dbu(out_v):+.1f} dBu  {fmt_vpp(out_v)}")
    else:
        tone_str = f"{freq:.0f} Hz  {level_dbfs:.0f} dBFS"

    print(f"\n{'='*72}")
    print(f"  LIVE MONITOR  --  {tone_str}")
    print(f"  Ctrl+C to stop")
    print(f"{'='*72}\n")

    if cal and cal.input_ok:
        print(f"  {'In Vrms':>12}  {'In dBu':>8}  {'In Vpp':>10}  {'Gain':>8}  "
              f"{'THD %':>9}  {'THD+N %':>9}  {'Noise floor':>12}")
        print(f"  {'─'*12}  {'─'*8}  {'─'*10}  {'─'*8}  {'─'*9}  {'─'*9}  {'─'*12}")
    else:
        print(f"  {'In dBFS':>8}  {'THD %':>9}  {'THD+N %':>9}  {'Noise floor':>12}")
        print(f"  {'─'*8}  {'─'*9}  {'─'*9}  {'─'*12}")

    print("  [settling...]", end="\r")
    for _ in range(WARMUP_REPS * 2):
        sd.play(tone, samplerate=SAMPLERATE, device=output_device,
                mapping=[output_channel + 1])
        sd.wait()

    try:
        while True:
            rec = sd.playrec(
                tone,
                samplerate=SAMPLERATE,
                input_mapping=[input_channel + 1],
                output_mapping=[output_channel + 1],
                device=(input_device, output_device),
                dtype="float32",
            )
            sd.wait()

            r = analyze(rec, sr=SAMPLERATE, fundamental=freq)

            if "error" in r:
                print(f"  !! {r['error']}", end="\r")
                continue

            noise_str = f"{r['noise_floor_dbfs']:+.1f} dBFS"

            if cal and cal.input_ok:
                in_v    = cal.in_vrms(r["linear_rms"])
                in_dbu  = vrms_to_dbu(in_v)
                in_vpp  = vrms_to_vpp(in_v)
                out_v   = cal.out_vrms(level_dbfs)
                gain_db  = (vrms_to_dbu(in_v) - vrms_to_dbu(out_v)) if out_v else None
                gain_s   = f"{gain_db:>+6.2f}dB" if gain_db is not None else "      -"
                thd_bar  = _thd_bar(r["thd_pct"])
                if target_vrms is not None:
                    target_dbu = vrms_to_dbu(target_vrms)
                    delta_db   = in_dbu - target_dbu
                    trim       = _trim_bar(delta_db)
                    trim_s     = f"  {delta_db:>+5.2f}dB {trim}"
                else:
                    trim_s = ""
                line = (f"  {fmt_vrms(in_v):>12}  {in_dbu:>+8.2f}  "
                        f"{fmt_vpp(in_v):>10}  {gain_s}  "
                        f"{r['thd_pct']:>8.4f}%  "
                        f"{r['thdn_pct']:>8.4f}%  "
                        f"{noise_str:>12}  {thd_bar}{trim_s}")
            else:
                thd_bar = _thd_bar(r["thd_pct"])
                line = (f"  {r['fundamental_dbfs']:>+8.1f}  "
                        f"{r['thd_pct']:>8.4f}%  "
                        f"{r['thdn_pct']:>8.4f}%  "
                        f"{noise_str:>12}  {thd_bar}")

            sys.stdout.write(f"\r{line:<100}")
            sys.stdout.flush()

    except KeyboardInterrupt:
        sd.stop()
        print(f"\n\n  Stopped.\n")
