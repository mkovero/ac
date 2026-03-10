# sweep.py
import time
import numpy as np
import sounddevice as sd
from .signal import make_sine
from .analysis import analyze
from .conversions import vrms_to_dbu, fmt_vrms
from .constants import SAMPLERATE, WARMUP_REPS, DURATION

def measure_sweep(input_device, output_device,
                  level_db_range=(-40, 0, 2),
                  fundamental=1000,
                  input_channel=0, output_channel=0,
                  cal=None):

    start_db, stop_db, step_db = level_db_range
    levels_db = np.arange(start_db, stop_db + step_db * 0.5, step_db)
    results   = []
    have_cal  = cal is not None and cal.input_ok

    print(f"\n{'─'*78}")
    print(f"  Sweep: {start_db} -> {stop_db} dBFS  step {step_db} dB  |  "
          f"Tone: {fundamental:.0f} Hz")
    if cal:
        cal.summary()
    print(f"{'─'*78}")

    if have_cal:
        print(f"\n  {'Drive':>8}  {'Out Vrms':>12}  {'Out dBu':>8}  "
              f"{'In Vrms':>12}  {'In dBu':>8}  {'Gain':>8}  {'THD%':>9}  {'THD+N%':>9}")
        print(f"  {'─'*8}  {'─'*12}  {'─'*8}  {'─'*12}  {'─'*8}  {'─'*8}  {'─'*9}  {'─'*9}")
    else:
        print(f"\n  {'Drive':>8}  {'In dBFS':>8}  {'THD%':>9}  {'THD+N%':>9}")
        print(f"  {'─'*8}  {'─'*8}  {'─'*9}  {'─'*9}")

    for level_db in levels_db:
        amplitude = 10.0 ** (level_db / 20.0)
        tone      = make_sine(amplitude, fundamental, DURATION)

        for _ in range(WARMUP_REPS):
            sd.play(tone, samplerate=SAMPLERATE, device=output_device)
            sd.wait()

        try:
            rec = sd.playrec(tone, samplerate=SAMPLERATE,
                             input_mapping=[input_channel + 1],
                             output_mapping=[output_channel + 1],
                             device=(input_device, output_device),
                             dtype="float32")
            sd.wait()
        except Exception as e:
            print(f"  {level_db:>7.1f} dB  !! {e}")
            continue

        r = analyze(rec, sr=SAMPLERATE, fundamental=fundamental)
        if "error" in r:
            print(f"  {level_db:>7.1f} dB  !! {r['error']}")
            continue

        r["drive_db"] = level_db
        r["out_vrms"] = cal.out_vrms(level_db)       if cal else None
        r["out_dbu"]  = vrms_to_dbu(r["out_vrms"])   if r["out_vrms"] else None
        r["in_vrms"]  = cal.in_vrms(r["linear_rms"]) if cal else None
        r["in_dbu"]   = vrms_to_dbu(r["in_vrms"])    if r["in_vrms"] else None
        # gain: 0.00 dB = same as loopback (in_vrms already compensates hardware loss)
        if r["in_dbu"] is not None and r["out_dbu"] is not None:
            r["gain_db"] = r["in_dbu"] - r["out_dbu"]
        else:
            r["gain_db"] = None

        if have_cal:
            out_s     = fmt_vrms(r["out_vrms"]) if r["out_vrms"] else "  -"
            in_s      = fmt_vrms(r["in_vrms"])  if r["in_vrms"]  else "  -"
            odbu      = f"{r['out_dbu']:+.2f}"  if r["out_dbu"] is not None else "  -"
            idbu      = f"{r['in_dbu']:+.2f}"   if r["in_dbu"]  is not None else "  -"
            gain_s    = f"{r['gain_db']:+.2f}dB" if r["gain_db"] is not None else "  -"
            clip_flag = "  [CLIP]" if r.get("clipping") else ""
            print(f"  {level_db:>7.1f}dB  {out_s:>12}  {odbu:>8}  "
                  f"{in_s:>12}  {idbu:>8}  {gain_s:>8}  "
                  f"{r['thd_pct']:>9.4f}  {r['thdn_pct']:>9.4f}{clip_flag}")
        else:
            print(f"  {level_db:>7.1f}dB  {r['fundamental_dbfs']:>+8.1f}  "
                  f"{r['thd_pct']:>9.4f}  {r['thdn_pct']:>9.4f}")

        results.append(r)
        time.sleep(0.05)

    return results
