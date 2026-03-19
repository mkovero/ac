# ac.py  -- client for the "ac" CLI  (routes all commands through ZMQ server)
import os
import re
import sys
import math
import json
import time
import shutil
import csv as _csv
from datetime import datetime

import numpy as np

from .parse        import parse, ParseError, USAGE
from ..config      import load as load_config, save as save_config, show as show_config
from ..config      import session_dir, SESSION_BASE
from .io           import save_csv, print_summary
from .plotting     import plot_results
from ..conversions import vrms_to_dbu, fmt_vrms


CTRL_PORT = 5556
DATA_PORT  = 5557


# ---------------------------------------------------------------------------
# ZMQ client
# ---------------------------------------------------------------------------

class AcClient:
    def __init__(self, host="localhost", ctrl_port=CTRL_PORT, data_port=DATA_PORT):
        try:
            import zmq
        except ImportError:
            print("  error: pyzmq not installed — run: pip install pyzmq")
            sys.exit(1)
        self._host      = host
        self._ctrl_port = ctrl_port
        self._ctx       = zmq.Context()

        self._ctrl = self._ctx.socket(zmq.REQ)
        self._ctrl.setsockopt(zmq.LINGER, 0)
        self._ctrl.connect(f"tcp://{host}:{ctrl_port}")

        self._data = self._ctx.socket(zmq.SUB)
        self._data.setsockopt(zmq.SUBSCRIBE, b"")
        self._data.setsockopt(zmq.LINGER, 0)
        self._data.connect(f"tcp://{host}:{data_port}")

        # Give the SUB socket a moment to register with the publisher
        time.sleep(0.05)

    def _reconnect_ctrl(self):
        """Recreate the REQ socket — required after any recv timeout (broken state)."""
        import zmq
        self._ctrl.close()
        self._ctrl = self._ctx.socket(zmq.REQ)
        self._ctrl.setsockopt(zmq.LINGER, 0)
        self._ctrl.connect(f"tcp://{self._host}:{self._ctrl_port}")

    def send_cmd(self, cmd, timeout_ms=5000):
        """Send a command dict, return reply dict or None on timeout."""
        import zmq
        self._ctrl.setsockopt(zmq.RCVTIMEO, timeout_ms)
        try:
            self._ctrl.send_json(cmd)
            return self._ctrl.recv_json()
        except zmq.Again:
            self._reconnect_ctrl()   # socket is broken after a recv timeout; must reset
            return None
        except zmq.ZMQError:
            self._reconnect_ctrl()
            return None

    def recv_data(self, timeout_ms=30000):
        """Receive one DATA frame. Returns (topic, frame) or raises TimeoutError."""
        if not self._data.poll(timeout_ms):
            raise TimeoutError("no data from server")
        msg   = self._data.recv()
        space = msg.index(b" ")
        topic = msg[:space].decode()
        frame = json.loads(msg[space + 1:])
        return topic, frame

    def close(self):
        self._ctrl.close()
        self._data.close()
        self._ctx.term()


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _get_cal(client):
    """Fetch calibration from server for the server's configured channels."""
    ack = client.send_cmd({"cmd": "get_calibration"})
    if ack and ack.get("ok") and ack.get("found"):
        return ack
    return None


def _level_to_dbfs(level, cal_info):
    """Convert a parsed level to dBFS using calibration from server."""
    if isinstance(level, tuple):
        kind, val = level
        if kind == "dbfs":
            return val
        if kind == "dbu":
            v_out = cal_info["vrms_at_0dbfs_out"] if cal_info else None
            if not v_out:
                print("  error: dBu levels require output calibration — run:  ac calibrate")
                sys.exit(1)
            from ..conversions import dbu_to_vrms
            vrms = dbu_to_vrms(val)
            return 20.0 * math.log10(vrms / v_out)
    # Vrms
    v_out = cal_info["vrms_at_0dbfs_out"] if cal_info else None
    if not v_out:
        print("  error: Vrms levels require output calibration — run:  ac calibrate")
        sys.exit(1)
    vrms = float(level)
    return max(-60.0, min(-0.5, 20.0 * math.log10(vrms / v_out)))


def _parse_vrms(raw):
    """Parse a DMM reading string to Vrms float, or None."""
    raw = raw.strip().lower().replace(" ", "")
    if not raw:
        return None
    try:
        if raw.endswith("mv") or raw.endswith("m"):
            return float(raw.rstrip("mv").rstrip("m")) / 1000.0
        if raw.endswith("v"):
            return float(raw.rstrip("v"))
        return float(raw)
    except ValueError:
        return None


def _cal_from_frame(frame):
    """Reconstruct a minimal Calibration object from a sweep_point frame."""
    from ..server.jack_calibration import Calibration
    v_out = frame.get("vrms_at_0dbfs_out")
    v_in  = frame.get("vrms_at_0dbfs_in")
    if v_out is None and v_in is None:
        return None
    cal = Calibration()
    cal.vrms_at_0dbfs_out = v_out
    cal.vrms_at_0dbfs_in  = v_in
    return cal


def _numpy_results(results):
    """Convert spectrum/freqs fields from list back to numpy arrays for plotting."""
    for r in results:
        if "spectrum" in r and isinstance(r["spectrum"], list):
            r["spectrum"] = np.array(r["spectrum"])
        if "freqs" in r and isinstance(r["freqs"], list):
            r["freqs"] = np.array(r["freqs"])
    return results


def _launch_ui(mode, host="localhost", data_port=DATA_PORT):
    """Spawn the pyqtgraph UI as a separate process. Returns True on success."""
    try:
        import pyqtgraph  # noqa: F401 — check availability only
    except ImportError:
        return False
    import subprocess
    subprocess.Popen(
        [sys.executable, "-m", "thd_tool.ui",
         "--mode", mode, "--host", host, "--port", str(data_port)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return True


def _save_results(results, label, cal=None, cfg=None, show_plot=False,
                  host="localhost", data_port=DATA_PORT):
    ts       = datetime.now().strftime("%Y%m%d_%H%M%S")
    safe     = label.replace(" ", "_")
    active   = (cfg or {}).get("session")
    out_dir  = session_dir(active) if active else (cfg or {}).get("output_dir", ".")
    os.makedirs(out_dir, exist_ok=True)
    csv_path  = os.path.join(out_dir, f"{safe}_{ts}.csv")
    plot_path = os.path.join(out_dir, f"{safe}_{ts}.png")
    save_csv(results, csv_path)
    # show=True opens an interactive matplotlib window after saving (only when
    # pyqtgraph is absent — the pyqtgraph UI is already running from before the sweep).
    plot_results(results, device_name=label, output_path=plot_path, cal=cal,
                 show=(show_plot and not _has_pyqtgraph()))


def _make_q_listener():
    """
    Spawn a daemon thread that watches stdin for 'q'.
    Returns (stop_event, restore_fn).
    - stop_event: threading.Event, set when 'q' is pressed or restore is called
    - restore_fn: call in finally to restore terminal attrs
    """
    import threading
    import select
    import termios
    import tty
    stop_event = threading.Event()
    fd = sys.stdin.fileno()
    try:
        saved = termios.tcgetattr(fd)
        tty.setcbreak(fd)          # individual keypresses; Ctrl+C still sends SIGINT
    except Exception:
        return stop_event, lambda: None   # not a real tty (CI, pipe, etc.)

    def _listen():
        try:
            while not stop_event.is_set():
                r, _, _ = select.select([fd], [], [], 0.05)
                if r:
                    ch = os.read(fd, 1)
                    if ch.lower() == b'q':
                        stop_event.set()
        except Exception:
            pass

    threading.Thread(target=_listen, daemon=True).start()

    def _restore():
        stop_event.set()
        try:
            termios.tcsetattr(fd, termios.TCSADRAIN, saved)
        except Exception:
            pass

    return stop_event, _restore


def _has_pyqtgraph():
    """Return True if pyqtgraph is importable."""
    try:
        import pyqtgraph  # noqa: F401
        return True
    except ImportError:
        return False


def _src_mtime():
    """Max mtime of server-side .py files."""
    client_dir = os.path.dirname(os.path.abspath(__file__))
    server_dir = os.path.join(os.path.dirname(client_dir), "server")
    return max(
        os.path.getmtime(os.path.join(server_dir, f))
        for f in os.listdir(server_dir)
        if f.endswith(".py")
    )


def _spawn_local_server(client):
    """Start a local-only server process silently, wait up to 3 s."""
    import subprocess
    subprocess.Popen(
        [sys.executable, "-m", "thd_tool", "--serve"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    for _ in range(30):
        time.sleep(0.1)
        ack = client.send_cmd({"cmd": "status"}, timeout_ms=200)
        if ack is not None:
            return
    print("  error: could not start server")
    sys.exit(1)


def _ensure_server(client):
    """Ensure a responsive, up-to-date server is running.

    For remote hosts: error if not responding.
    For localhost: silently auto-start if needed, silently restart if stale.
    """
    ack = client.send_cmd({"cmd": "status"}, timeout_ms=500)

    if ack is not None:
        # Only check staleness for the local auto-spawned server.
        # Remote servers have independent source trees with unrelated mtimes.
        if client._host in ("localhost", "127.0.0.1"):
            if ack.get("src_mtime", 0) < _src_mtime() - 0.5:
                # Local server is stale — ask it to quit then respawn
                client.send_cmd({"cmd": "quit"}, timeout_ms=1000)
                time.sleep(0.3)
                _spawn_local_server(client)
        return

    if client._host not in ("localhost", "127.0.0.1"):
        print(f"  error: server not responding at {client._host}:{client._ctrl_port}")
        sys.exit(1)

    _spawn_local_server(client)


def _check_ack(ack, context=""):
    if ack is None:
        print(f"  error: server not responding{(' — ' + context) if context else ''}")
        sys.exit(1)
    if not ack.get("ok"):
        print(f"  error: {ack.get('error', 'unknown error')}")
        sys.exit(1)
    return ack


def _collect_stream(client, cmd_name, on_data, timeout_ms=60000):
    """Receive DATA frames until done/error. Returns xruns count."""
    try:
        while True:
            topic, frame = client.recv_data(timeout_ms=timeout_ms)
            if topic == "data":
                on_data(frame)
            elif topic == "done":
                if frame.get("xruns"):
                    print(f"\n  !! {frame['xruns']} xrun(s) during {cmd_name}")
                return frame.get("xruns", 0)
            elif topic == "error":
                print(f"\n  !! {frame.get('message', 'error')}")
                return 0
    except TimeoutError:
        print(f"\n  error: timeout waiting for {cmd_name} data")
        return 0
    except KeyboardInterrupt:
        client.send_cmd({"cmd": "stop"})
        print("\n  Stopped.")
        return 0


# ---------------------------------------------------------------------------
# Command handlers
# ---------------------------------------------------------------------------

_FF400_PLAY = (
    "AN1 AN2 AN3 AN4 AN5 AN6 AN7 AN8 "
    "SPDIF-L SPDIF-R ADAT1 ADAT2 ADAT3 ADAT4 ADAT5 ADAT6 ADAT7 ADAT8"
).split()
_FF400_CAP = (
    "AN1 AN2 AN3 AN4 AN5 AN6 AN7 AN8 "
    "SPDIF-L SPDIF-R ADAT1 ADAT2 ADAT3 ADAT4 ADAT5 ADAT6 ADAT7 ADAT8"
).split()

_KNOWN_LAYOUTS = {
    "Fireface400": (_FF400_PLAY, _FF400_CAP),
}

def _detect_card_name():
    try:
        with open("/proc/asound/cards") as f:
            for line in f:
                for name in _KNOWN_LAYOUTS:
                    if name in line:
                        return name
    except OSError:
        pass
    return None


def cmd_devices(_cmd, cfg, client):
    ack = _check_ack(client.send_cmd({"cmd": "devices"}), "devices")
    playback    = ack.get("playback", [])
    capture     = ack.get("capture",  [])
    out_ch      = ack.get("output_channel", 0)
    in_ch       = ack.get("input_channel",  0)
    out_sticky  = ack.get("output_port")
    in_sticky   = ack.get("input_port")

    card = _detect_card_name()
    hw_play, hw_cap = _KNOWN_LAYOUTS.get(card, (None, None))

    def hw(names, i):
        if names and i < len(names):
            return f"  [{names[i]}]"
        return ""

    def sticky_note(sticky, ports, ch):
        if not sticky:
            return ""
        if sticky in ports:
            actual_idx = ports.index(sticky)
            if actual_idx != ch:
                return f"  (reordered: now ch {actual_idx})"
            return ""
        return "  (sticky port not found)"

    print("\n  JACK ports:")
    out_name = playback[out_ch] if out_ch < len(playback) else "??"
    in_name  = capture[in_ch]  if in_ch  < len(capture)  else "??"
    out_suf  = (f"  ->  {out_sticky}" if out_sticky else "") + sticky_note(out_sticky, playback, out_ch)
    in_suf   = (f"  ->  {in_sticky}"  if in_sticky  else "") + sticky_note(in_sticky,  capture,  in_ch)
    print(f"  Configured:  output ch {out_ch}  ->  {out_name}{hw(hw_play, out_ch)}{out_suf}")
    print(f"               input  ch {in_ch}  ->  {in_name}{hw(hw_cap, in_ch)}{in_suf}")
    print("\n  Playback:")
    for i, p in enumerate(playback):
        mark = "  <--" if i == out_ch else ""
        print(f"    {i:>3}  {p}{hw(hw_play, i)}{mark}")
    print("\n  Capture:")
    for i, p in enumerate(capture):
        mark = "  <--" if i == in_ch else ""
        print(f"    {i:>3}  {p}{hw(hw_cap, i)}{mark}")
    print()


def cmd_setup(cmd, cfg, client):
    update = {}
    if "device"       in cmd: update["device"]          = cmd["device"]
    if "output"       in cmd: update["output_channel"]  = cmd["output"]
    if "input"        in cmd: update["input_channel"]   = cmd["input"]
    if "dbu_ref_vrms" in cmd: update["dbu_ref_vrms"]    = cmd["dbu_ref_vrms"]
    if "dmm_host"     in cmd: update["dmm_host"]        = cmd["dmm_host"]
    if "gpio_port"    in cmd: update["gpio_port"]       = cmd["gpio_port"]
    if "range_start"  in cmd: update["range_start_hz"]  = cmd["range_start"]
    if "range_stop"   in cmd: update["range_stop_hz"]   = cmd["range_stop"]
    ack = _check_ack(client.send_cmd({"cmd": "setup", "update": update}))
    srv_cfg = ack.get("config", {})
    ref = srv_cfg.get("dbu_ref_vrms", 0.77459667)
    print(f"\n  -- Hardware config (server) --")
    print(f"  Device:         {srv_cfg.get('device', '?')}")
    print(f"  Output channel: {srv_cfg.get('output_channel', '?')}")
    print(f"  Input channel:  {srv_cfg.get('input_channel',  '?')}")
    print(f"  dBu reference: {ref*1000:.4f} mVrms  ({ref:.8f} V)")
    dmm = srv_cfg.get("dmm_host")
    print(f"  DMM host:      {dmm if dmm else '(not configured)'}")
    gpio = srv_cfg.get("gpio_port")
    print(f"  GPIO port:     {gpio if gpio else '(not configured)'}")
    print(f"  Range:         {srv_cfg.get('range_start_hz', 20):.0f} – {srv_cfg.get('range_stop_hz', 20000):.0f} Hz")
    if update:
        print("  Saved.")

    if "gpio_port" in cmd:
        port = cmd["gpio_port"]
        gpio_ack = client.send_cmd({"cmd": "gpio_setup", "port": port}, timeout_ms=5000)
        if gpio_ack and gpio_ack.get("ok"):
            print(f"  GPIO: {'started on ' + port if port else 'stopped'}")
        elif gpio_ack:
            print(f"  GPIO: {gpio_ack.get('error', 'error')}")
        else:
            print(f"  GPIO: server not responding")
    print()


def cmd_stop(_cmd, cfg, client):
    ack = client.send_cmd({"cmd": "stop"})
    if ack.get("ok"):
        print("  Stopped.")
    else:
        print(f"  {ack.get('error', 'unknown error')}")


def cmd_dmm_show(_cmd, cfg, client):
    from ..conversions import fmt_vrms, fmt_vpp
    ack = _check_ack(client.send_cmd({"cmd": "dmm_read"}))
    if ack.get("idn"):
        print(f"\n  {ack['idn']}")
    vrms = ack["vrms"]
    print(f"\n  AC  {fmt_vrms(vrms)}  =  {vrms_to_dbu(vrms):+.2f} dBu  =  {fmt_vpp(vrms)}\n")


def cmd_calibrate_show(_cmd, cfg, client):
    from ..server.jack_calibration import DEFAULT_CAL_PATH
    ack = _check_ack(client.send_cmd({"cmd": "list_calibrations"}))
    cals = ack.get("calibrations", [])
    if not cals:
        print(f"\n  No calibrations stored  ({DEFAULT_CAL_PATH})\n")
        return
    print(f"\n  Stored calibrations  ({DEFAULT_CAL_PATH})\n")
    for c in cals:
        print(f"  [{c['key']}]")
        v = c.get("vrms_at_0dbfs_out")
        if v:
            print(f"    Output: 0 dBFS = {fmt_vrms(v):>14}  =  {vrms_to_dbu(v):+.2f} dBu")
        else:
            print(f"    Output: not calibrated")
        v = c.get("vrms_at_0dbfs_in")
        if v:
            print(f"    Input:  0 dBFS = {fmt_vrms(v):>14}  =  {vrms_to_dbu(v):+.2f} dBu")
        else:
            print(f"    Input:  not calibrated")
        n_pts = c.get("response_pts", 0)
        if n_pts:
            rng = c.get("response_range")
            dev = c.get("response_max_dev")
            rng_s = f"{rng[0]:.0f}–{rng[1]:.0f} Hz" if rng else ""
            dev_s = f"±{dev:.2f} dB" if dev is not None else ""
            print(f"    Response: {n_pts} pts  {rng_s}  {dev_s}")
        print()


def cmd_calibrate(cmd, cfg, client):
    freq    = cmd["freq"]
    level   = cmd["level"]
    out_ch  = cmd.get("output_channel")
    in_ch   = cmd.get("input_channel")

    cal_info = _get_cal(client)
    if isinstance(level, tuple) and level[0] == "dbfs":
        ref_dbfs = level[1]
    elif cal_info:
        ref_dbfs = _level_to_dbfs(level, cal_info)
    else:
        ref_dbfs = -10.0

    c = {"cmd": "calibrate", "freq_hz": freq, "ref_dbfs": ref_dbfs}
    if out_ch is not None:
        c["output_channel"] = out_ch
    if in_ch is not None:
        c["input_channel"] = in_ch

    ack = _check_ack(client.send_cmd(c, timeout_ms=5000))
    print(f"  Calibration started: {freq:.0f} Hz  |  {ref_dbfs:.1f} dBFS")

    try:
        while True:
            topic, frame = client.recv_data(timeout_ms=120000)
            if topic == "cal_progress":
                print(f"\n  {frame.get('text', 'Working...')}", flush=True)
            elif topic == "cal_prompt":
                print(f"\n  {frame['text']}\n")
                if frame.get("dmm_vrms") is not None:
                    hint = f"{frame['dmm_vrms'] * 1000:.4f} mVrms"
                    raw  = input(f"  Enter to accept ({hint}), or override: ").strip()
                    vrms = _parse_vrms(raw) if raw else frame["dmm_vrms"]
                else:
                    while True:
                        raw = input("  DMM reading (e.g. 245mV or 0.245): ").strip()
                        vrms = _parse_vrms(raw)
                        if vrms is not None:
                            break
                        print("  Try:  0.245  or  245mV")
                client.send_cmd({"cmd": "cal_reply", "vrms": vrms})
            elif topic == "cal_done":
                print(f"\n  Calibration saved: [{frame.get('key')}]")
                v = frame.get("vrms_at_0dbfs_out")
                if v:
                    print(f"  Output: 0 dBFS = {fmt_vrms(v)}  =  {vrms_to_dbu(v):+.2f} dBu")
                v = frame.get("vrms_at_0dbfs_in")
                if v:
                    print(f"  Input:  0 dBFS = {fmt_vrms(v)}  =  {vrms_to_dbu(v):+.2f} dBu")
                n_pts = frame.get("response_pts", 0)
                if n_pts:
                    print(f"  Response curve: {n_pts} pts")
                err = frame.get("error")
                if err:
                    print(f"  Note: {err}")
                print()
                break
            elif topic == "error":
                print(f"  error: {frame.get('message')}")
                break
    except TimeoutError:
        print("  error: calibration timed out")
    except KeyboardInterrupt:
        client.send_cmd({"cmd": "stop"})
        print("\n  Calibration cancelled.")


def _print_sweep_header(have_cal):
    print("\n" + "─" * 78)
    if have_cal:
        print("  " + "  ".join([f"{'Drive':>8}", f"{'Out Vrms':>12}", f"{'Out dBu':>8}",
                                  f"{'In Vrms':>12}", f"{'In dBu':>8}",
                                  f"{'Gain':>8}", f"{'THD%':>9}", f"{'THD+N%':>9}"]))
        print("  " + "  ".join(["─"*8, "─"*12, "─"*8, "─"*12, "─"*8, "─"*8, "─"*9, "─"*9]))
    else:
        print("  " + "  ".join([f"{'Drive':>8}", f"{'THD%':>9}", f"{'THD+N%':>9}"]))


def _print_freq_header(have_cal):
    print("\n" + "─" * 78)
    if have_cal:
        print("  " + "  ".join([f"{'Freq':>8}", f"{'Out Vrms':>12}", f"{'Out dBu':>8}",
                                  f"{'In Vrms':>12}", f"{'In dBu':>8}",
                                  f"{'Gain':>8}", f"{'THD%':>9}", f"{'THD+N%':>9}"]))
        print("  " + "  ".join(["─"*8, "─"*12, "─"*8, "─"*12, "─"*8, "─"*8, "─"*9, "─"*9]))
    else:
        print("  " + "  ".join([f"{'Freq':>8}", f"{'THD%':>9}", f"{'THD+N%':>9}"]))


def _print_sweep_row(frame):
    drive = frame["drive_db"]
    thd   = frame["thd_pct"]
    thdn  = frame["thdn_pct"]
    clip  = "  [CLIP]" if frame.get("clipping") else ""
    if frame.get("out_vrms") is not None:
        out_s  = fmt_vrms(frame["out_vrms"])
        in_s   = fmt_vrms(frame["in_vrms"]) if frame.get("in_vrms") is not None else "  -"
        odbu   = f"{frame['out_dbu']:+.2f}"  if frame.get("out_dbu") is not None else "  -"
        idbu   = f"{frame['in_dbu']:+.2f}"   if frame.get("in_dbu")  is not None else "  -"
        gain_s = f"{frame['gain_db']:+.2f}dB" if frame.get("gain_db") is not None else "  -"
        print(f"  {drive:>7.1f}dB  {out_s:>12}  {odbu:>8}  "
              f"{in_s:>12}  {idbu:>8}  {gain_s:>8}  "
              f"{thd:>9.4f}  {thdn:>9.4f}{clip}")
    else:
        print(f"  {drive:>7.1f}dBFS  {thd:>9.4f}  {thdn:>9.4f}")


def _print_freq_row(frame):
    freq  = frame.get("freq_hz", frame.get("fundamental_hz", 0))
    thd   = frame["thd_pct"]
    thdn  = frame["thdn_pct"]
    flag  = "  [CLIP]" if frame.get("clipping") else ("  [AC]" if frame.get("ac_coupled") else "")
    if frame.get("out_vrms") is not None:
        out_s  = fmt_vrms(frame["out_vrms"])
        in_s   = fmt_vrms(frame["in_vrms"]) if frame.get("in_vrms") is not None else "  -"
        odbu   = f"{frame['out_dbu']:+.2f}"  if frame.get("out_dbu") is not None else "  -"
        idbu   = f"{frame['in_dbu']:+.2f}"   if frame.get("in_dbu")  is not None else "  -"
        gain_s = f"{frame['gain_db']:+.2f}dB" if frame.get("gain_db") is not None else "  -"
        print(f"  {freq:>7.0f} Hz  {out_s:>12}  {odbu:>8}  "
              f"{in_s:>12}  {idbu:>8}  {gain_s:>8}  "
              f"{thd:>9.4f}  {thdn:>9.4f}{flag}")
    else:
        print(f"  {freq:>7.0f} Hz  {thd:>9.4f}  {thdn:>9.4f}{flag}")


def cmd_sweep_level(cmd, cfg, client):
    cal_info = _get_cal(client)
    start_db = _level_to_dbfs(cmd["start"], cal_info)
    stop_db  = _level_to_dbfs(cmd["stop"],  cal_info)
    freq     = cmd["freq"]
    duration = cmd.get("duration", 1.0)

    print(f"\n  Sweep: {start_db:.1f} → {stop_db:.1f} dBFS  |  {freq:.0f} Hz  |  {duration:.1f}s")

    ack = _check_ack(client.send_cmd({
        "cmd":        "sweep_level",
        "freq_hz":    freq,
        "start_dbfs": start_db,
        "stop_dbfs":  stop_db,
        "duration":   duration,
    }))
    print(f"  Output: {ack['out_port']}")
    print(f"  Sweeping... Ctrl+C or q to stop.\n")

    q_stop, q_restore = _make_q_listener()
    try:
        while True:
            if q_stop.is_set():
                client.send_cmd({"cmd": "stop", "name": "sweep_level"})
                print("\n  Stopped.")
                return
            try:
                topic, frame = client.recv_data(timeout_ms=500)
            except TimeoutError:
                continue
            if topic == "error" and frame.get("cmd") in (None, "sweep_level"):
                print(f"\n  error: {frame.get('message')}")
                return
            if topic == "done" and frame.get("cmd") in (None, "sweep_level"):
                return
    except KeyboardInterrupt:
        client.send_cmd({"cmd": "stop", "name": "sweep_level"})
        print("\n  Stopped.")
    finally:
        q_restore()


def cmd_sweep_frequency(cmd, cfg, client):
    cal_info = _get_cal(client)
    level_db = _level_to_dbfs(cmd["level"], cal_info)
    start_hz = cmd["start"] if cmd["start"] is not None else cfg.get("range_start_hz", 20.0)
    stop_hz  = cmd["stop"]  if cmd["stop"]  is not None else cfg.get("range_stop_hz", 20000.0)
    duration = cmd.get("duration", 1.0)

    print(f"\n  Sweep: {start_hz:.0f} → {stop_hz:.0f} Hz  |  {level_db:.1f} dBFS  |  {duration:.1f}s")

    ack = _check_ack(client.send_cmd({
        "cmd":        "sweep_frequency",
        "start_hz":   start_hz,
        "stop_hz":    stop_hz,
        "level_dbfs": level_db,
        "duration":   duration,
    }))
    print(f"  Output: {ack['out_port']}")
    print(f"  Sweeping... Ctrl+C or q to stop.\n")

    q_stop, q_restore = _make_q_listener()
    try:
        while True:
            if q_stop.is_set():
                client.send_cmd({"cmd": "stop", "name": "sweep_frequency"})
                print("\n  Stopped.")
                return
            try:
                topic, frame = client.recv_data(timeout_ms=500)
            except TimeoutError:
                continue
            if topic == "error" and frame.get("cmd") in (None, "sweep_frequency"):
                print(f"\n  error: {frame.get('message')}")
                return
            if topic == "done" and frame.get("cmd") in (None, "sweep_frequency"):
                return
    except KeyboardInterrupt:
        client.send_cmd({"cmd": "stop", "name": "sweep_frequency"})
        print("\n  Stopped.")
    finally:
        q_restore()


def cmd_plot(cmd, cfg, client):
    cal_info = _get_cal(client)
    if cal_info:
        print("  Loaded calibration from server.")
    else:
        print("  No calibration found — levels in dBFS only.")
    level_db = _level_to_dbfs(cmd["level"], cal_info)

    start_hz = cmd["start"] if cmd["start"] is not None else cfg.get("range_start_hz", 20.0)
    stop_hz  = cmd["stop"]  if cmd["stop"]  is not None else cfg.get("range_stop_hz", 20000.0)

    print(f"\n  Plot: {start_hz:.0f} → {stop_hz:.0f} Hz  "
          f"{cmd['ppd']} pts/decade  |  {level_db:.1f} dBFS")
    _print_freq_header(cal_info is not None)

    if cmd.get("show_plot"):
        host = cfg.get("server_host", "localhost")
        _launch_ui("sweep_frequency", host=host, data_port=cfg.get("zmq_data_port", DATA_PORT))

    ack = _check_ack(client.send_cmd({
        "cmd":        "plot",
        "start_hz":   start_hz,
        "stop_hz":    stop_hz,
        "level_dbfs": level_db,
        "ppd":        cmd["ppd"],
    }))
    print(f"  Output: {ack['out_port']}  →  Input: {ack['in_port']}")

    results = []

    def on_data(frame):
        if frame.get("type") == "sweep_point":
            results.append(frame)
            _print_freq_row(frame)

    _collect_stream(client, "plot", on_data, timeout_ms=300000)

    if not results:
        return
    _numpy_results(results)
    cal = _cal_from_frame(results[0])
    print_summary(results, "DUT", cal=cal)
    _save_results(results, "plot", cal=cal, cfg=cfg,
                  show_plot=cmd.get("show_plot", False),
                  host=cfg.get("server_host", "localhost"),
                  data_port=cfg.get("zmq_data_port", DATA_PORT))


def cmd_monitor(cmd, cfg, client):
    import signal
    import sys
    from .tui import SpectrumRenderer

    start_freq = cmd.get("start_freq", 20.0)
    end_freq   = cmd.get("end_freq", 20000.0)
    interval   = cmd.get("interval", 0.1)

    # Convert min_y/max_y level tokens to dB for the display scale
    min_y = cmd.get("min_y")
    max_y = cmd.get("max_y")
    db_min = min_y[1] if (min_y and isinstance(min_y, tuple)) else -100
    db_max = max_y[1] if (max_y and isinstance(max_y, tuple)) else 0

    if cmd.get("show_plot"):
        host = cfg.get("server_host", "localhost")
        _launch_ui("spectrum", host=host, data_port=cfg.get("zmq_data_port", DATA_PORT))

    ack = _check_ack(client.send_cmd({
        "cmd":      "monitor_spectrum",
        "freq_hz":  start_freq,
        "interval": interval,
    }))
    print(f"  Input: {ack['in_port']}")
    print(f"  {start_freq:.0f}–{end_freq:.0f} Hz  |  Ctrl+C or q to stop")

    renderer = SpectrumRenderer(db_min=db_min, db_max=db_max,
                                start_freq=start_freq, end_freq=end_freq)
    sys.stdout.write("\033[?25l\033[2J")
    sys.stdout.flush()

    sys.stdout.write(
        f"\033[H\033[1;37m  {start_freq:.0f}–{end_freq:.0f} Hz  |  waiting for data...  [q] quit\033[0m"
    )
    sys.stdout.flush()

    def _on_resize(*_):
        sys.stdout.write("\033[2J")
        sys.stdout.flush()
    signal.signal(signal.SIGWINCH, _on_resize)

    q_stop, q_restore = _make_q_listener()
    error_msg = None
    try:
        while True:
            if q_stop.is_set():
                break
            try:
                topic, frame = client.recv_data(timeout_ms=2000)
            except TimeoutError:
                continue
            if topic == "error" and frame.get("cmd") in (None, "monitor_spectrum"):
                error_msg = frame.get("message", "unknown error")
                break
            if topic == "done" and frame.get("cmd") in (None, "monitor_spectrum"):
                break
            if topic != "data":
                continue
            if frame.get("type") != "spectrum":
                continue

            sr             = frame.get("sr", 48000)
            detected_hz    = frame.get("freq_hz", start_freq)
            harmonic_freqs = [detected_hz * (i + 1) for i in range(10)
                              if detected_hz * (i + 1) < sr / 2]
            out = renderer.render(
                np.array(frame["freqs"]),
                np.array(frame["spectrum"]),
                frame.get("thd_pct"),
                frame.get("thdn_pct"),
                frame.get("in_dbu"),
                detected_hz,
                harmonic_freqs,
                sr=sr,
            )
            sys.stdout.write("\033[H" + out)
            sys.stdout.flush()
    except KeyboardInterrupt:
        pass
    finally:
        client.send_cmd({"cmd": "stop", "name": "monitor_spectrum"})
        sys.stdout.write("\033[?25h\033[2J\033[H")
        sys.stdout.flush()
        if error_msg:
            print(f"  Error: {error_msg}")
        else:
            print("  Stopped.")
        q_restore()


def cmd_generate_sine(cmd, cfg, client):
    from ..conversions import fmt_vrms, fmt_vpp
    from .parse import _parse_channels

    freq    = cmd["freq"]
    level   = cmd["level"]
    ch_spec = cmd.get("channels")

    # Resolve None level at runtime: 0 dBu if calibrated, else -20 dBFS
    if level is None:
        default_cal = _get_cal(client)
        level = ("dbu", 0.0) if default_cal else ("dbfs", -20.0)

    # Fetch server config to know which channels + calibration
    ack_status = client.send_cmd({"cmd": "status"})
    ack_setup  = client.send_cmd({"cmd": "setup", "update": {}})
    srv_cfg    = (ack_setup or {}).get("config", {}) if ack_setup else {}

    channels   = _parse_channels(ch_spec) if ch_spec is not None else [srv_cfg.get("output_channel", 0)]

    # Show per-channel info
    print()
    first_dbfs = None
    for ch in channels:
        cal_ack = client.send_cmd({
            "cmd":            "get_calibration",
            "output_channel": ch,
        })
        cal_info = cal_ack if (cal_ack and cal_ack.get("found")) else None
        dbfs = _level_to_dbfs(level, cal_info)
        if first_dbfs is None:
            first_dbfs = dbfs
        v_out = (cal_info["vrms_at_0dbfs_out"] if cal_info else None)
        if v_out:
            vrms    = v_out * 10.0 ** (dbfs / 20.0)
            cal_tag = f"{vrms_to_dbu(vrms):+.2f} dBu"
        else:
            vrms    = None
            cal_tag = f"{dbfs:.1f} dBFS (uncal)"
        vrms_s = fmt_vrms(vrms) if vrms else "  -"
        print(f"  ch {ch:>3}  {freq:.0f} Hz  {vrms_s:>14}  {cal_tag}")

    if first_dbfs is None:
        first_dbfs = -12.0

    ack = _check_ack(client.send_cmd({
        "cmd":        "generate",
        "freq_hz":    freq,
        "level_dbfs": first_dbfs,
        "channels":   channels,
    }))
    for port in ack.get("out_ports", []):
        print(f"  → {port}")
    print(f"\n  Playing {len(channels)} channel(s)... Ctrl+C or q to stop.\n")

    q_stop, q_restore = _make_q_listener()
    try:
        while True:
            if q_stop.is_set():
                client.send_cmd({"cmd": "stop", "name": "generate"})
                print("\n  Stopped.")
                return
            try:
                topic, frame = client.recv_data(timeout_ms=500)
            except TimeoutError:
                continue   # still playing
            if topic == "error" and frame.get("cmd") in (None, "generate"):
                print(f"\n  error: {frame.get('message')}")
                return
            if topic == "done" and frame.get("cmd") in (None, "generate"):
                return
    except KeyboardInterrupt:
        client.send_cmd({"cmd": "stop", "name": "generate"})
        print("\n  Stopped.")
    finally:
        q_restore()


def cmd_generate_pink(cmd, cfg, client):
    from ..conversions import fmt_vrms
    from .parse import _parse_channels

    level   = cmd["level"]
    ch_spec = cmd.get("channels")

    # Resolve None level at runtime: 0 dBu if calibrated, else -20 dBFS
    if level is None:
        default_cal = _get_cal(client)
        level = ("dbu", 0.0) if default_cal else ("dbfs", -20.0)

    ack_setup = client.send_cmd({"cmd": "setup", "update": {}})
    srv_cfg   = (ack_setup or {}).get("config", {}) if ack_setup else {}

    channels = _parse_channels(ch_spec) if ch_spec is not None else [srv_cfg.get("output_channel", 0)]

    print()
    first_dbfs = None
    for ch in channels:
        cal_ack = client.send_cmd({
            "cmd":            "get_calibration",
            "output_channel": ch,
        })
        cal_info = cal_ack if (cal_ack and cal_ack.get("found")) else None
        dbfs = _level_to_dbfs(level, cal_info)
        if first_dbfs is None:
            first_dbfs = dbfs
        v_out = cal_info["vrms_at_0dbfs_out"] if cal_info else None
        if v_out:
            vrms    = v_out * 10.0 ** (dbfs / 20.0)
            cal_tag = f"{vrms_to_dbu(vrms):+.2f} dBu"
        else:
            vrms    = None
            cal_tag = f"{dbfs:.1f} dBFS (uncal)"
        vrms_s = fmt_vrms(vrms) if vrms else "  -"
        print(f"  ch {ch:>3}  pink noise  {vrms_s:>14}  {cal_tag}")

    if first_dbfs is None:
        first_dbfs = -12.0

    ack = _check_ack(client.send_cmd({
        "cmd":        "generate_pink",
        "level_dbfs": first_dbfs,
        "channels":   channels,
    }))
    for port in ack.get("out_ports", []):
        print(f"  → {port}")
    print(f"\n  Playing pink noise on {len(channels)} channel(s)... Ctrl+C or q to stop.\n")

    q_stop, q_restore = _make_q_listener()
    try:
        while True:
            if q_stop.is_set():
                client.send_cmd({"cmd": "stop", "name": "generate_pink"})
                print("\n  Stopped.")
                return
            try:
                topic, frame = client.recv_data(timeout_ms=500)
            except TimeoutError:
                continue
            if topic == "error" and frame.get("cmd") in (None, "generate_pink"):
                print(f"\n  error: {frame.get('message')}")
                return
            if topic == "done" and frame.get("cmd") in (None, "generate_pink"):
                return
    except KeyboardInterrupt:
        client.send_cmd({"cmd": "stop", "name": "generate_pink"})
        print("\n  Stopped.")
    finally:
        q_restore()



def cmd_server_enable(_cmd, cfg, client):
    ack = _check_ack(client.send_cmd({"cmd": "server_enable"}))
    addr = ack.get("bind_addr", "*")
    ctrl_port = cfg.get("zmq_ctrl_port", CTRL_PORT)
    data_port = cfg.get("zmq_data_port", DATA_PORT)
    print(f"  Server now listening on all interfaces  "
          f"(tcp://{addr}:{ctrl_port} + tcp://{addr}:{data_port})")
    print(f"  Config saved.")


def cmd_server_disable(_cmd, cfg, client):
    ack = _check_ack(client.send_cmd({"cmd": "server_disable"}))
    print(f"  Server now listening on localhost only")
    print(f"  Config saved.")


def cmd_server_connections(_cmd, cfg, client):
    ack = _check_ack(client.send_cmd({"cmd": "server_connections"}))
    mode    = ack.get("listen_mode", "?")
    ctrl_ep = ack.get("ctrl_endpoint", "?")
    data_ep = ack.get("data_endpoint", "?")
    clients = ack.get("clients", [])
    workers = ack.get("workers", [])
    print(f"\n  Listen:  {mode}  ({ctrl_ep} + {data_ep})")
    print(f"  Clients: {len(clients)}")
    for c in clients:
        print(f"    {c}")
    active = ', '.join(workers) if workers else "(none)"
    print(f"  Active:  {active}")
    print()


def cmd_server_set_host(cmd, cfg, client):
    """Save server host to local config — handled before AcClient."""
    # Should not reach here; handled in main().
    pass


# ---------------------------------------------------------------------------
# Session commands  (filesystem only, no ZMQ)
# ---------------------------------------------------------------------------

_SESSION_NAME_RE = re.compile(r'^[a-zA-Z0-9_-]+$')


def _validate_session_name(name):
    if not _SESSION_NAME_RE.match(name):
        print("  error: session name must be alphanumeric + hyphens/underscores only")
        sys.exit(1)
    return name


def cmd_session_new(cmd, cfg):
    name = _validate_session_name(cmd["name"])
    path = session_dir(name)
    if os.path.exists(path):
        print(f"  error: session {name!r} already exists ({path})")
        sys.exit(1)
    os.makedirs(path)
    save_config({"session": name})
    print(f"  Session created: {path}")
    print(f"  Active session:  {name}")


def cmd_session_list(cmd, cfg):
    active = cfg.get("session")
    if not os.path.isdir(SESSION_BASE):
        print("  No sessions yet.  Run: ac new <name>")
        return
    entries = sorted(e for e in os.listdir(SESSION_BASE)
                     if os.path.isdir(os.path.join(SESSION_BASE, e)))
    if not entries:
        print("  No sessions yet.  Run: ac new <name>")
        return
    print()
    for name in entries:
        d = session_dir(name)
        n_csv = sum(1 for f in os.listdir(d) if f.endswith(".csv"))
        mark = "  <-- active" if name == active else ""
        print(f"  {name:<30}  {n_csv} measurement(s){mark}")
    print()


def cmd_session_use(cmd, cfg):
    name = _validate_session_name(cmd["name"])
    path = session_dir(name)
    if not os.path.isdir(path):
        print(f"  error: session {name!r} not found — run:  ac new {name}")
        sys.exit(1)
    save_config({"session": name})
    n_csv = sum(1 for f in os.listdir(path) if f.endswith(".csv"))
    print(f"  Active session: {name}  ({n_csv} measurement(s))")


def cmd_session_rm(cmd, cfg):
    name = _validate_session_name(cmd["name"])
    path = session_dir(name)
    if not os.path.isdir(path):
        print(f"  error: session {name!r} not found")
        sys.exit(1)
    files = os.listdir(path)
    print(f"  Session: {name}  ({len(files)} file(s) in {path})")
    try:
        answer = input("  Delete? [y/N] ").strip().lower()
    except (EOFError, KeyboardInterrupt):
        print("\n  Cancelled.")
        return
    if answer != "y":
        print("  Cancelled.")
        return
    shutil.rmtree(path)
    active = cfg.get("session")
    if active == name:
        save_config({"session": None})
        print(f"  Deleted.  (was active — cleared)")
    else:
        print(f"  Deleted.")


def _load_csv(path):
    with open(path, newline="") as f:
        return list(_csv.DictReader(f))


def _latest_csv(session_name):
    d = session_dir(session_name)
    if not os.path.isdir(d):
        print(f"  error: session {session_name!r} not found")
        sys.exit(1)
    csvs = sorted(f for f in os.listdir(d) if f.endswith(".csv"))
    if not csvs:
        print(f"  error: no CSV files in session {session_name!r}")
        sys.exit(1)
    return os.path.join(d, csvs[-1])


def cmd_session_diff(cmd, cfg):
    name_a, name_b = cmd["name_a"], cmd["name_b"]
    path_a = _latest_csv(name_a)
    path_b = _latest_csv(name_b)
    rows_a = _load_csv(path_a)
    rows_b = _load_csv(path_b)

    def _floats(rows, col):
        vals = []
        for r in rows:
            v = r.get(col, "").strip()
            try:
                vals.append(float(v))
            except (ValueError, TypeError):
                vals.append(None)
        return vals

    freq_a = _floats(rows_a, "freq_hz")
    distinct_a = len({v for v in freq_a if v is not None})
    is_freq_sweep = distinct_a > 2

    if is_freq_sweep:
        freq_b    = _floats(rows_b, "freq_hz")
        thd_a     = _floats(rows_a, "thd_pct")
        thdn_a    = _floats(rows_a, "thdn_pct")
        thd_b_raw = _floats(rows_b, "thd_pct")
        thdn_b_raw= _floats(rows_b, "thdn_pct")

        # Build valid arrays for interpolation
        fa  = np.array([f for f, t, n in zip(freq_a,  thd_a,      thdn_a)     if f and t is not None and n is not None])
        ta  = np.array([t for f, t, n in zip(freq_a,  thd_a,      thdn_a)     if f and t is not None and n is not None])
        na  = np.array([n for f, t, n in zip(freq_a,  thd_a,      thdn_a)     if f and t is not None and n is not None])
        fb  = np.array([f for f, t, n in zip(freq_b,  thd_b_raw,  thdn_b_raw) if f and t is not None and n is not None])
        tb  = np.array([t for f, t, n in zip(freq_b,  thd_b_raw,  thdn_b_raw) if f and t is not None and n is not None])
        nb  = np.array([n for f, t, n in zip(freq_b,  thd_b_raw,  thdn_b_raw) if f and t is not None and n is not None])

        # Interpolate B onto A's grid (log-freq space)
        log_fa = np.log10(fa)
        log_fb = np.log10(fb)
        thd_b_interp  = np.interp(log_fa, log_fb, tb)
        thdn_b_interp = np.interp(log_fa, log_fb, nb)

        print(f"\n  Diff: {name_a}  vs  {name_b}")
        print(f"  A: {os.path.basename(path_a)}")
        print(f"  B: {os.path.basename(path_b)}")
        print()
        hdr = (f"  {'Freq':>8}  {'A THD%':>9}  {'B THD%':>9}  {'ΔTHD%':>9}"
               f"  {'A THD+N%':>10}  {'B THD+N%':>10}  {'ΔTHD+N%':>10}")
        print(hdr)
        print("  " + "─" * (len(hdr) - 2))
        d_thd_max = d_thdn_max = 0.0
        for f, ta_v, tb_v, na_v, nb_v in zip(fa, ta, thd_b_interp, na, thdn_b_interp):
            d_thd  = tb_v - ta_v
            d_thdn = nb_v - na_v
            d_thd_max  = max(d_thd_max,  abs(d_thd))
            d_thdn_max = max(d_thdn_max, abs(d_thdn))
            print(f"  {f:>7.0f} Hz  {ta_v:>9.4f}  {tb_v:>9.4f}  {d_thd:>+9.4f}"
                  f"  {na_v:>10.4f}  {nb_v:>10.4f}  {d_thdn:>+10.4f}")
        print()
        print(f"  Max |ΔTHD|:    {d_thd_max:.4f}%")
        print(f"  Max |ΔTHD+N|:  {d_thdn_max:.4f}%")
        print()

    else:
        # Level sweep diff — match by drive_db
        def _key(rows):
            out = {}
            for r in rows:
                try:
                    k = float(r.get("drive_db", "").strip())
                    out[k] = r
                except (ValueError, TypeError):
                    pass
            return out

        map_a = _key(rows_a)
        map_b = _key(rows_b)
        drives = sorted(set(map_a) & set(map_b))

        print(f"\n  Diff: {name_a}  vs  {name_b}")
        print(f"  A: {os.path.basename(path_a)}")
        print(f"  B: {os.path.basename(path_b)}")
        print()
        hdr = (f"  {'Drive':>8}  {'A THD%':>9}  {'B THD%':>9}  {'ΔTHD%':>9}"
               f"  {'A THD+N%':>10}  {'B THD+N%':>10}  {'ΔTHD+N%':>10}")
        print(hdr)
        print("  " + "─" * (len(hdr) - 2))
        d_thd_max = d_thdn_max = 0.0
        for drive in drives:
            try:
                ta_v  = float(map_a[drive].get("thd_pct",  ""))
                na_v  = float(map_a[drive].get("thdn_pct", ""))
                tb_v  = float(map_b[drive].get("thd_pct",  ""))
                nb_v  = float(map_b[drive].get("thdn_pct", ""))
            except (ValueError, TypeError):
                continue
            d_thd  = tb_v - ta_v
            d_thdn = nb_v - na_v
            d_thd_max  = max(d_thd_max,  abs(d_thd))
            d_thdn_max = max(d_thdn_max, abs(d_thdn))
            print(f"  {drive:>7.1f} dB  {ta_v:>9.4f}  {tb_v:>9.4f}  {d_thd:>+9.4f}"
                  f"  {na_v:>10.4f}  {nb_v:>10.4f}  {d_thdn:>+10.4f}")
        print()
        print(f"  Max |ΔTHD|:    {d_thd_max:.4f}%")
        print(f"  Max |ΔTHD+N|:  {d_thdn_max:.4f}%")
        print()


# ---------------------------------------------------------------------------
# Dispatch table
# ---------------------------------------------------------------------------

HANDLERS = {
    "devices":            cmd_devices,
    "setup":              cmd_setup,
    "stop":               cmd_stop,
    "dmm_show":           cmd_dmm_show,
    "calibrate":          cmd_calibrate,
    "calibrate_show":     cmd_calibrate_show,
    "sweep_level":        cmd_sweep_level,
    "sweep_frequency":    cmd_sweep_frequency,
    "plot":               cmd_plot,
    "monitor":            cmd_monitor,
    "generate_sine":      cmd_generate_sine,
    "generate_pink":      cmd_generate_pink,
    "server_enable":      cmd_server_enable,
    "server_disable":     cmd_server_disable,
    "server_connections": cmd_server_connections,
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
    from ..conversions import set_dbu_ref
    set_dbu_ref(cfg.get("dbu_ref_vrms", 0.77459667))

    # --- Commands that don't need a ZMQ connection ---
    if cmd["cmd"] == "server_set_host":
        host = cmd["host"]
        save_config({"server_host": host})
        print(f"  Server host set to: {host}")
        print(f"  All ac commands will now route through tcp://{host}:{CTRL_PORT}")
        return

    SESSION_CMDS = {
        "session_new":  cmd_session_new,
        "session_list": cmd_session_list,
        "session_use":  cmd_session_use,
        "session_rm":   cmd_session_rm,
        "session_diff": cmd_session_diff,
    }
    if cmd["cmd"] in SESSION_CMDS:
        SESSION_CMDS[cmd["cmd"]](cmd, cfg)
        return

    # --- All other commands route through ZMQ ---
    host = cfg.get("server_host", "localhost")
    ctrl_port = cfg.get("zmq_ctrl_port", CTRL_PORT)
    data_port = cfg.get("zmq_data_port", DATA_PORT)

    client = AcClient(host=host, ctrl_port=ctrl_port, data_port=data_port)
    try:
        _ensure_server(client)
        handler = HANDLERS.get(cmd["cmd"])
        if handler is None:
            print(f"  error: no handler for {cmd['cmd']!r}", file=sys.stderr)
            sys.exit(1)
        handler(cmd, cfg, client)
    finally:
        client.close()
