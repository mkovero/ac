# ac.py  -- main entry point for the "ac" CLI  (JACK backend only)
import os
import sys
import math
from datetime import datetime

from .parse            import parse, ParseError, USAGE
from .config           import load as load_config, save as save_config, show as show_config
from .jack_calibration import run_calibration_jack
from .jack_calibration import Calibration
from .jack_measure     import jack_sweep_level, jack_sweep_frequency, jack_monitor
from .audio            import find_ports, port_name, JackEngine
from .io               import save_csv, print_summary
from .plotting         import plot_results
from .conversions      import vrms_to_dbu


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _require_jack():
    try:
        import jack
        c = jack.Client("ac-probe", no_start_server=True)
        c.close()
    except Exception:
        print("\n  error: JACK server not running.")
        print("  Start it with:  jackd -d alsa -d hw:0 -r 48000 -p 1024 -n 2\n")
        sys.exit(1)


def _load_cal(cfg, freq):
    cal = Calibration.load(output_channel=cfg["output_channel"],
                           input_channel=cfg["input_channel"],
                           freq=freq)
    if cal is not None:
        print("  Loaded calibration:")
        cal.summary()
    else:
        print("  No calibration found for this setup/freq.")
        print("  Run:  ac calibrate")
    return cal


def _level_to_vrms(level, cal):
    from .conversions import dbu_to_vrms, get_dbu_ref
    if isinstance(level, tuple):
        kind, val = level
        if kind == "dbu":
            return dbu_to_vrms(val)
        if kind == "dbfs":
            if cal and cal.output_ok:
                return cal.out_vrms(val)
            return get_dbu_ref() * 10.0 ** (val / 20.0)
    return float(level)


def _level_to_dbfs(level, cal):
    from .conversions import get_dbu_ref
    if isinstance(level, tuple):
        kind, val = level
        if kind == "dbfs":
            return val
        if kind == "dbu":
            vrms = _level_to_vrms(level, cal)
            if cal and cal.output_ok:
                return 20.0 * math.log10(vrms / cal.vrms_at_0dbfs_out)
            return 20.0 * math.log10(vrms / get_dbu_ref()) - 13.0
    vrms = float(level)
    if cal and cal.output_ok:
        return max(-60.0, min(-0.5, 20.0 * math.log10(vrms / cal.vrms_at_0dbfs_out)))
    return max(-60.0, 20.0 * math.log10(vrms / get_dbu_ref()) - 13.0)


def _print_ports(cfg):
    playback, capture = find_ports()
    out_port = playback[cfg["output_channel"]] if cfg["output_channel"] < len(playback) else "??"
    in_port  = capture[cfg["input_channel"]]   if cfg["input_channel"]  < len(capture)  else "??"
    print(f"  Output: ch {cfg['output_channel']}  ({out_port})")
    print(f"  Input:  ch {cfg['input_channel']}  ({in_port})")
    return playback, capture


def _save_results(results, label, cfg, cal, show_plot=False):
    ts       = datetime.now().strftime("%Y%m%d_%H%M%S")
    safe     = label.replace(" ", "_")
    out_dir  = cfg.get("output_dir", ".")
    os.makedirs(out_dir, exist_ok=True)
    csv_path  = os.path.join(out_dir, f"{safe}_{ts}.csv")
    plot_path = os.path.join(out_dir, f"{safe}_{ts}.png")
    save_csv(results, csv_path)
    plot_results(results, device_name=label, output_path=plot_path, cal=cal)
    if show_plot:
        import subprocess
        viewer_cmds = [
            ["eog", "--fullscreen", plot_path],
            ["feh", plot_path],
            ["xdg-open", plot_path],
            ["display", plot_path],
        ]
        for cmd_args in viewer_cmds:
            try:
                subprocess.Popen(cmd_args,
                                 stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL)
                break
            except FileNotFoundError:
                continue


# ---------------------------------------------------------------------------
# Command handlers
# ---------------------------------------------------------------------------

def cmd_devices(_cmd, cfg):
    _require_jack()
    playback, capture = find_ports()
    print("\n  JACK ports:")
    print(f"  Configured:  output ch {cfg['output_channel']}  ->  "
          f"{playback[cfg['output_channel']] if cfg['output_channel'] < len(playback) else '??'}")
    print(f"               input  ch {cfg['input_channel']}  ->  "
          f"{capture[cfg['input_channel']] if cfg['input_channel'] < len(capture) else '??'}")
    print("\n  Playback:")
    for i, p in enumerate(playback):
        mark = "  <--" if i == cfg["output_channel"] else ""
        print(f"    {i:>3}  {p}{mark}")
    print("\n  Capture:")
    for i, p in enumerate(capture):
        mark = "  <--" if i == cfg["input_channel"] else ""
        print(f"    {i:>3}  {p}{mark}")
    print()


def cmd_setup(cmd, cfg):
    update = {}
    if "device"       in cmd: update["device"]         = cmd["device"]
    if "output"       in cmd: update["output_channel"] = cmd["output"]
    if "input"        in cmd: update["input_channel"]  = cmd["input"]
    if "dbu_ref_vrms" in cmd: update["dbu_ref_vrms"]   = cmd["dbu_ref_vrms"]
    if not update:
        show_config(cfg)
        return
    new_cfg = save_config(update)
    show_config(new_cfg)
    print("  Saved.")


def cmd_calibrate(cmd, cfg):
    _require_jack()
    freq  = cmd["freq"]
    level = cmd["level"]
    if isinstance(level, tuple) and level[0] == "dbfs":
        ref_dbfs = level[1]
    else:
        cal      = Calibration.load(output_channel=cfg["output_channel"],
                                    input_channel=cfg["input_channel"], freq=freq)
        ref_dbfs = _level_to_dbfs(level, cal) if cal else -10.0

    _print_ports(cfg)
    run_calibration_jack(
        output_channel = cfg["output_channel"],
        input_channel  = cfg["input_channel"],
        ref_dbfs       = ref_dbfs,
        freq           = freq,
    )


def cmd_sweep_level(cmd, cfg):
    _require_jack()
    freq     = cmd["freq"]
    cal      = _load_cal(cfg, freq)
    start_db = _level_to_dbfs(cmd["start"], cal)
    stop_db  = _level_to_dbfs(cmd["stop"],  cal)
    step     = cmd["step"]
    step_db  = step[1] if isinstance(step, tuple) else float(step)
    _print_ports(cfg)
    results  = jack_sweep_level(cfg, freq, start_db, stop_db, step_db, cal=cal)
    if not results:
        return
    print_summary(results, "DUT", cal=cal)
    _save_results(results, "sweep_level", cfg, cal, show_plot=cmd.get("show_plot", False))


def cmd_sweep_frequency(cmd, cfg):
    _require_jack()
    cal      = _load_cal(cfg, 1000.0)
    level_db = _level_to_dbfs(cmd["level"], cal)
    _print_ports(cfg)
    results  = jack_sweep_frequency(cfg, cmd["start"], cmd["stop"], level_db,
                                    ppd=cmd["ppd"], cal=cal)
    if not results:
        return
    print_summary(results, "DUT", cal=cal)
    _save_results(results, "sweep_frequency", cfg, cal, show_plot=cmd.get("show_plot", False))


def cmd_monitor_thd(cmd, cfg):
    _require_jack()
    freq     = cmd["freq"]
    cal      = _load_cal(cfg, freq)
    level_db = _level_to_dbfs(cmd["level"], cal)
    _print_ports(cfg)
    jack_monitor(cfg, freq, level_db, cal=cal, interval=cmd["interval"])


def cmd_generate_sine(cmd, cfg):
    _require_jack()
    from .conversions import fmt_vrms, fmt_vpp

    freq  = cmd["freq"]
    cal   = _load_cal(cfg, freq)
    vrms  = _level_to_vrms(cmd["level"], cal)
    dbfs  = max(-60.0, min(-0.5, _level_to_dbfs(cmd["level"], cal)))

    playback, _ = find_ports()
    out_port    = port_name(playback, cfg["output_channel"])
    print(f"  Output: ch {cfg['output_channel']}  ({out_port})")
    print(f"  {freq:.0f} Hz  |  {fmt_vrms(vrms)}  =  {vrms_to_dbu(vrms):+.2f} dBu"
          f"  =  {fmt_vpp(vrms)}  ({dbfs:.1f} dBFS)")
    print(f"\n  Playing... Ctrl+C to stop.\n")

    engine = JackEngine()
    engine.set_tone(freq, 10.0 ** (dbfs / 20.0))
    engine.start(output_port=out_port)
    try:
        import threading
        threading.Event().wait()
    except KeyboardInterrupt:
        pass
    finally:
        engine.set_silence()
        engine.stop()
        print("\n  Stopped.")


def cmd_monitor_level(cmd, cfg):
    _require_jack()
    from .conversions import fmt_vrms
    from .analysis import analyze
    from .constants import DURATION

    freq     = cmd["freq"]
    cal      = _load_cal(cfg, freq)
    vrms_out = _level_to_vrms(cmd["level"], cal)
    dbfs     = max(-60.0, min(-0.5, _level_to_dbfs(cmd["level"], cal)))

    playback, capture = find_ports()
    out_port = port_name(playback, cfg["output_channel"])
    in_port  = port_name(capture,  cfg["input_channel"])
    print(f"  Output: ch {cfg['output_channel']}  ({out_port})")
    print(f"  Input:  ch {cfg['input_channel']}  ({in_port})")
    print(f"  Tone: {freq:.0f} Hz  |  {fmt_vrms(vrms_out)}  =  {vrms_to_dbu(vrms_out):+.2f} dBu"
          f"  ({dbfs:.1f} dBFS)")
    print(f"\n  Ctrl+C to stop.\n")

    engine       = JackEngine()
    engine.set_tone(freq, 10.0 ** (dbfs / 20.0))
    engine.start(output_port=out_port, input_port=in_port)
    duration     = max(DURATION, cmd["interval"])
    update_every = max(1, round(cmd["interval"] / DURATION))
    block        = 0

    try:
        while True:
            data  = engine.capture_block(duration)
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
            if in_vrms and cal and cal.input_ok:
                delta = in_dbu - vrms_to_dbu(vrms_out)
                col   = "\033[32m" if abs(delta) < 0.1 else "\033[33m" if abs(delta) < 0.5 else "\033[31m"
                print(f"  In: {fmt_vrms(in_vrms):>12}  {in_dbu:>+7.2f} dBu  {col}{delta:+.2f} dB\033[0m",
                      end="\r", flush=True)
            else:
                print(f"  In: {r['fundamental_dbfs']:>+7.2f} dBFS", end="\r", flush=True)
    except KeyboardInterrupt:
        engine.set_silence()
        engine.stop()
        print("\n\n  Stopped.")


# ---------------------------------------------------------------------------
# Dispatch table
# ---------------------------------------------------------------------------

HANDLERS = {
    "devices":          cmd_devices,
    "setup":            cmd_setup,
    "calibrate":        cmd_calibrate,
    "sweep_level":      cmd_sweep_level,
    "sweep_frequency":  cmd_sweep_frequency,
    "monitor_thd":      cmd_monitor_thd,
    "monitor_level":    cmd_monitor_level,
    "monitor_spectrum": cmd_monitor_thd,   # TODO
    "generate_sine":    cmd_generate_sine,
}


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main():
    if len(sys.argv) < 2 or sys.argv[1] in ("-h", "--help", "help"):
        print(USAGE)
        return

    try:
        cmd = parse(sys.argv[1:])
    except ParseError as e:
        print(f"\n  error: {e}\n", file=sys.stderr)
        print(USAGE, file=sys.stderr)
        sys.exit(1)

    cfg = load_config()
    from .conversions import set_dbu_ref
    set_dbu_ref(cfg.get("dbu_ref_vrms", 0.77459667))

    handler = HANDLERS.get(cmd["cmd"])
    if handler is None:
        print(f"  error: no handler for {cmd['cmd']!r}", file=sys.stderr)
        sys.exit(1)

    handler(cmd, cfg)
