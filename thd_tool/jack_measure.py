# jack_measure.py  -- measurement functions using JackEngine
# Drop-in replacements for the sounddevice-based sweep.py / live.py functions.

import time
import numpy as np
from .audio     import JackEngine, find_ports, port_name
from .analysis  import analyze
from .signal    import make_sine
from .conversions import vrms_to_dbu, fmt_vrms
def _warmup(engine, n_blocks=4):
    """Let the output settle for n_blocks of audio."""
    dur = engine.blocksize / engine.samplerate * n_blocks
    engine.capture_block(dur)


def _measure_one(engine, freq, duration=1.0, cal=None):
    """Capture one block and return analysis result dict."""
    data = engine.capture_block(duration)
    rec  = data.reshape(-1, 1)   # analyze() wants (frames, channels)
    r    = analyze(rec, sr=engine.samplerate, fundamental=freq)
    if "error" in r:
        return r
    r["out_vrms"] = cal.out_vrms(20 * np.log10(engine._tone_pos and 1 or 1)) if cal else None
    # Use cal to convert linear_rms -> physical Vrms
    r["in_vrms"]  = cal.in_vrms(r["linear_rms"]) if (cal and cal.input_ok) else None
    r["in_dbu"]   = vrms_to_dbu(r["in_vrms"])    if r["in_vrms"] else None
    return r


def _engine_from_cfg(cfg):
    """Create and connect a JackEngine from a config dict."""
    playback, capture = find_ports()
    out_port = port_name(playback, cfg["output_channel"])
    in_port  = port_name(capture,  cfg["input_channel"])
    engine   = JackEngine()
    engine.start(output_port=out_port, input_port=in_port)
    return engine, out_port, in_port


# ------------------------------------------------------------------
# Level sweep
# ------------------------------------------------------------------

def jack_sweep_level(cfg, freq, start_dbfs, stop_dbfs, step_db, cal=None,
                     duration=1.0):
    import math
    levels_db = np.arange(start_dbfs, stop_dbfs + step_db * 0.5, step_db)
    results   = []
    have_cal  = cal is not None and cal.input_ok

    print("\n" + "─"*78)
    print(f"  Level sweep: {start_dbfs:.0f} -> {stop_dbfs:.0f} dBFS  "
          f"step {step_db:.1f} dB  |  {freq:.0f} Hz")
    if cal:
        cal.summary()
    print("─"*78)

    if have_cal:
        print("\n  " + "  ".join([f"{'Drive':>8}", f"{'Out Vrms':>12}", f"{'Out dBu':>8}",
                                   f"{'In Vrms':>12}", f"{'In dBu':>8}",
                                   f"{'Gain':>8}", f"{'THD%':>9}", f"{'THD+N%':>9}"]))
        print("  " + "  ".join(["─"*8, "─"*12, "─"*8, "─"*12, "─"*8, "─"*8, "─"*9, "─"*9]))
    else:
        print("\n  " + "  ".join([f"{'Drive':>8}", f"{'THD%':>9}", f"{'THD+N%':>9}"]))

    engine, _, _ = _engine_from_cfg(cfg)

    try:
        for level_db in levels_db:
            amplitude = 10.0 ** (level_db / 20.0)
            engine.set_tone(freq, amplitude)
            _warmup(engine)

            data = engine.capture_block(duration)
            rec  = data.reshape(-1, 1)
            r    = analyze(rec, sr=engine.samplerate, fundamental=freq)

            if "error" in r:
                print(f"  {level_db:>7.1f} dBFS  !! {r['error']}")
                continue

            r["drive_db"] = level_db
            r["out_vrms"] = cal.out_vrms(level_db)       if cal else None
            r["out_dbu"]  = vrms_to_dbu(r["out_vrms"])   if r["out_vrms"] else None
            r["in_vrms"]  = cal.in_vrms(r["linear_rms"]) if cal else None
            r["in_dbu"]   = vrms_to_dbu(r["in_vrms"])    if r["in_vrms"]  else None
            r["gain_db"]  = (r["in_dbu"] - r["out_dbu"]
                             if r["in_dbu"] is not None and r["out_dbu"] is not None
                             else None)

            if have_cal:
                out_s  = fmt_vrms(r["out_vrms"]) if r["out_vrms"] else "  -"
                in_s   = fmt_vrms(r["in_vrms"])  if r["in_vrms"]  else "  -"
                odbu   = f"{r['out_dbu']:+.2f}"  if r["out_dbu"]  is not None else "  -"
                idbu   = f"{r['in_dbu']:+.2f}"   if r["in_dbu"]   is not None else "  -"
                gain_s = f"{r['gain_db']:+.2f}dB" if r["gain_db"] is not None else "  -"
                clip_f = "  [CLIP]" if r.get("clipping") else ""
                print(f"  {level_db:>7.1f}dB  {out_s:>12}  {odbu:>8}  "
                      f"{in_s:>12}  {idbu:>8}  {gain_s:>8}  "
                      f"{r['thd_pct']:>9.4f}  {r['thdn_pct']:>9.4f}{clip_f}")
            else:
                print(f"  {level_db:>7.1f}dBFS  "
                      f"{r['thd_pct']:>9.4f}  {r['thdn_pct']:>9.4f}")

            results.append(r)

        if engine.xruns:
            print(f"\n  !! {engine.xruns} xrun(s) during sweep -- results may be affected")
    finally:
        engine.set_silence()
        engine.stop()

    return results


# ------------------------------------------------------------------
# Frequency sweep
# ------------------------------------------------------------------

def jack_sweep_frequency(cfg, start_hz, stop_hz, level_dbfs, ppd=10,
                         cal=None, duration=1.0):
    n_decades = np.log10(stop_hz / start_hz)
    n_points  = max(2, int(round(n_decades * ppd)))
    freqs     = np.unique(np.round(np.geomspace(start_hz, stop_hz, n_points)).astype(int))
    have_cal  = cal is not None and cal.input_ok
    results   = []

    print("\n" + "─"*78)
    print(f"  Freq sweep: {start_hz:.0f} -> {stop_hz:.0f} Hz  "
          f"{ppd} pts/decade  |  {level_dbfs:.1f} dBFS")
    if cal:
        cal.summary()
    print("─"*78)

    if have_cal:
        print("\n  " + "  ".join([f"{'Freq':>8}", f"{'Out Vrms':>12}", f"{'Out dBu':>8}",
                                   f"{'In Vrms':>12}", f"{'In dBu':>8}",
                                   f"{'Gain':>8}", f"{'THD%':>9}", f"{'THD+N%':>9}"]))
        print("  " + "  ".join(["─"*8, "─"*12, "─"*8, "─"*12, "─"*8, "─"*8, "─"*9, "─"*9]))
    else:
        print("\n  " + "  ".join([f"{'Freq':>8}", f"{'THD%':>9}", f"{'THD+N%':>9}"]))

    amplitude = 10.0 ** (level_dbfs / 20.0)
    engine, _, _ = _engine_from_cfg(cfg)

    try:
        for freq in freqs:
            dur = max(duration, 10.0 / freq)   # at least 10 cycles
            engine.set_tone(float(freq), amplitude)
            _warmup(engine)

            data = engine.capture_block(dur)
            rec  = data.reshape(-1, 1)
            r    = analyze(rec, sr=engine.samplerate, fundamental=float(freq))

            if "error" in r:
                print(f"  {freq:>7.0f} Hz  !! {r['error']}")
                continue

            r["freq"]     = float(freq)
            r["drive_db"] = level_dbfs
            r["out_vrms"] = cal.out_vrms(level_dbfs)     if cal else None
            r["out_dbu"]  = vrms_to_dbu(r["out_vrms"])   if r["out_vrms"] else None
            r["in_vrms"]  = cal.in_vrms(r["linear_rms"]) if cal else None
            r["in_dbu"]   = vrms_to_dbu(r["in_vrms"])    if r["in_vrms"]  else None
            r["gain_db"]  = (r["in_dbu"] - r["out_dbu"]
                             if r["in_dbu"] is not None and r["out_dbu"] is not None
                             else None)

            if have_cal:
                out_s  = fmt_vrms(r["out_vrms"]) if r["out_vrms"] else "  -"
                in_s   = fmt_vrms(r["in_vrms"])  if r["in_vrms"]  else "  -"
                odbu   = f"{r['out_dbu']:+.2f}"  if r["out_dbu"]  is not None else "  -"
                idbu   = f"{r['in_dbu']:+.2f}"   if r["in_dbu"]   is not None else "  -"
                gain_s = f"{r['gain_db']:+.2f}dB" if r["gain_db"] is not None else "  -"
                flag = "  [CLIP]" if r.get("clipping") else ("  [AC]" if r.get("ac_coupled") else "")
                print(f"  {freq:>7.0f} Hz  {out_s:>12}  {odbu:>8}  "
                      f"{in_s:>12}  {idbu:>8}  {gain_s:>8}  "
                      f"{r['thd_pct']:>9.4f}  {r['thdn_pct']:>9.4f}{flag}")
            else:
                flag = "  [AC]" if r.get("ac_coupled") else ""
                print(f"  {freq:>7.0f} Hz  "
                      f"{r['thd_pct']:>9.4f}  {r['thdn_pct']:>9.4f}{flag}")

            results.append(r)

        if engine.xruns:
            print(f"\n  !! {engine.xruns} xrun(s) during sweep")
    finally:
        engine.set_silence()
        engine.stop()

    return results


# ------------------------------------------------------------------
# Live monitor
# ------------------------------------------------------------------

def jack_monitor(cfg, freq, level_dbfs, cal=None, interval=1.0,
                 target_vrms=None):
    import math

    amplitude = 10.0 ** (level_dbfs / 20.0)
    engine, _, _ = _engine_from_cfg(cfg)
    engine.set_tone(freq, amplitude)

    duration     = max(1.0, interval)
    update_every = max(1, round(interval / 1.0))

    print(f"  {freq:.0f} Hz  |  {level_dbfs:.1f} dBFS  |  Ctrl+C to stop\n")

    block = 0
    try:
        while True:
            data = engine.capture_block(duration)
            block += 1
            if block % update_every != 0:
                continue

            rec = data.reshape(-1, 1)
            r   = analyze(rec, sr=engine.samplerate, fundamental=freq)
            if "error" in r:
                print(f"  !! {r['error']}", end="\r")
                continue

            in_vrms = cal.in_vrms(r["linear_rms"]) if (cal and cal.input_ok) else None
            in_dbu  = vrms_to_dbu(in_vrms) if in_vrms else None
            out_dbu = vrms_to_dbu(cal.out_vrms(level_dbfs)) if (cal and cal.output_ok) else None
            gain_s  = (f"{in_dbu - out_dbu:+.2f}dB"
                       if in_dbu is not None and out_dbu is not None else "  -")

            thd = r["thd_pct"]
            if thd < 0.01:   col = "\033[32m"
            elif thd < 0.1:  col = "\033[33m"
            else:             col = "\033[31m"
            rst = "\033[0m"

            xr = f"  xruns:{engine.xruns}" if engine.xruns else ""
            print(f"  {in_dbu:>+7.2f} dBu  gain:{gain_s}  "
                  f"THD:{col}{thd:>8.4f}%{rst}  "
                  f"THD+N:{r['thdn_pct']:>8.4f}%{xr}",
                  end="\r", flush=True)

    except KeyboardInterrupt:
        engine.set_silence()
        engine.stop()
        print("\n\n  Stopped.")
