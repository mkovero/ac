# live.py
import sys
import math
import numpy as np
import sounddevice as sd
from .signal import make_sine
from .conversions import fmt_vrms, fmt_vpp, vrms_to_dbu, vrms_to_vpp
from .analysis import analyze
from .constants import SAMPLERATE, DURATION, WARMUP_REPS

def _thd_bar(thd_pct, width=20):
    try:
        norm = (math.log10(max(thd_pct, 0.0001)) + 3) / 3.0
    except Exception:
        norm = 0.0
    norm   = max(0.0, min(norm, 1.0))
    filled = int(norm * width)
    bar    = "█" * filled + "░" * (width - filled)
    color  = "\033[32m" if norm < 0.4 else ("\033[33m" if norm < 0.7 else "\033[31m")
    return f"{color}[{bar}]\033[0m"

def _trim_bar(delta_db, width=30):
    norm   = max(-1.0, min(1.0, delta_db / 6.0))
    centre = width // 2
    pos    = max(0, min(width - 1, int(centre + norm * centre)))
    bar    = list("─" * width)
    bar[centre] = "┼"
    bar[pos]    = "▶" if delta_db > 0 else ("◀" if delta_db < 0 else "●")
    color  = "\033[32m" if abs(delta_db) < 0.1 else ("\033[33m" if abs(delta_db) < 0.5 else "\033[31m")
    return f"{color}[{''.join(bar)}]\033[0m"


def live_monitor(input_device, output_device,
                 input_channel=0, output_channel=0,
                 level_dbfs=-12.0, freq=1000,
                 cal=None, target_vrms=None,
                 interval=1.0):
    # Always capture DURATION seconds per playrec for consistent FFT resolution.
    # interval controls how many seconds between display updates — if interval <
    # DURATION, every block is still analyzed but display only updates every
    # ceil(interval / DURATION) blocks. If interval > DURATION, skip blocks.
    amplitude  = 10.0 ** (level_dbfs / 20.0)
    tone       = make_sine(amplitude, freq, DURATION)
    # How many DURATION-sized blocks between display updates
    update_every = max(1, round(interval / DURATION))

    if cal and cal.output_ok:
        out_v    = cal.out_vrms(level_dbfs)
        tone_str = (f"{freq:.0f} Hz  {level_dbfs:.0f} dBFS  "
                    f"{fmt_vrms(out_v)}  {vrms_to_dbu(out_v):+.1f} dBu  {fmt_vpp(out_v)}")
    else:
        tone_str = f"{freq:.0f} Hz  {level_dbfs:.0f} dBFS"

    actual_interval = update_every * DURATION
    print(f"\n{'='*72}")
    print(f"  LIVE MONITOR  --  {tone_str}  [update every {actual_interval:.1f}s]")
    print(f"  Ctrl+C to stop")
    print(f"{'='*72}\n")

    if cal and cal.input_ok:
        print(f"  {'In Vrms':>12}  {'In dBu':>8}  {'In Vpp':>10}  {'Gain':>8}  "
              f"{'THD %':>9}  {'THD+N %':>9}  {'Noise floor':>12}")
        print(f"  {'─'*12}  {'─'*8}  {'─'*10}  {'─'*8}  {'─'*9}  {'─'*9}  {'─'*12}")
    else:
        print(f"  {'In dBFS':>8}  {'THD %':>9}  {'THD+N %':>9}  {'Noise floor':>12}")
        print(f"  {'─'*8}  {'─'*9}  {'─'*9}  {'─'*12}")

    print("  [settling...] ", end="\r")
    for _ in range(WARMUP_REPS * 2):
        sd.playrec(tone, samplerate=SAMPLERATE,
                   input_mapping=[input_channel + 1],
                   output_mapping=[output_channel + 1],
                   device=(input_device, output_device),
                   dtype="float32")
        sd.wait()

    block = 0
    try:
        while True:
            rec = sd.playrec(tone, samplerate=SAMPLERATE,
                             input_mapping=[input_channel + 1],
                             output_mapping=[output_channel + 1],
                             device=(input_device, output_device),
                             dtype="float32")
            sd.wait()
            block += 1
            if block % update_every != 0:
                continue

            r = analyze(rec, sr=SAMPLERATE, fundamental=freq)
            if "error" in r:
                sys.stdout.write(f"\r  !! {r['error']:<80}")
                sys.stdout.flush()
                continue

            noise_str = f"{r['noise_floor_dbfs']:+.1f} dBFS"

            if cal and cal.input_ok:
                in_v    = cal.in_vrms(r["linear_rms"])
                in_dbu  = vrms_to_dbu(in_v)
                out_v   = cal.out_vrms(level_dbfs)
                gain_db = (vrms_to_dbu(in_v) - vrms_to_dbu(out_v)) if out_v else None
                gain_s  = f"{gain_db:>+6.2f}dB" if gain_db is not None else "      -"
                thd_bar = _thd_bar(r["thd_pct"])
                trim_s  = ""
                if target_vrms is not None:
                    delta_db = in_dbu - vrms_to_dbu(target_vrms)
                    trim_s   = f"  {delta_db:>+5.2f}dB {_trim_bar(delta_db)}"
                line = (f"  {fmt_vrms(in_v):>12}  {in_dbu:>+8.2f}  "
                        f"{fmt_vpp(in_v):>10}  {gain_s}  "
                        f"{r['thd_pct']:>8.4f}%  "
                        f"{r['thdn_pct']:>8.4f}%  "
                        f"{noise_str:>12}  {thd_bar}{trim_s}")
            else:
                line = (f"  {r['fundamental_dbfs']:>+8.1f}  "
                        f"{r['thd_pct']:>8.4f}%  "
                        f"{r['thdn_pct']:>8.4f}%  "
                        f"{noise_str:>12}  {_thd_bar(r['thd_pct'])}")

            sys.stdout.write(f"\r{line:<120}")
            sys.stdout.flush()

    except KeyboardInterrupt:
        sd.stop()

    print(f"\n\n  Stopped.\n")
