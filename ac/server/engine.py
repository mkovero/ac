# server.py -- ZMQ REP+PUB server
# CTRL REP port 5556: receives commands, replies with ack
# DATA PUB port 5557: streams measurement results
import json
import os
import queue
import sys
import threading
import time
import numpy as np
from .audio            import get_engine_class, get_port_helpers
JackEngine = get_engine_class()
find_ports, port_name, resolve_port = get_port_helpers()
from .analysis         import analyze
from ..conversions     import vrms_to_dbu
from ..constants       import WARMUP_REPS
from .jack_calibration import Calibration
from ..config          import load as load_config, save as save_config

# Max mtime of any .py file in the package — used by client to detect stale servers.
_PKG_DIR   = os.path.dirname(os.path.abspath(__file__))
_SRC_MTIME = max(
    os.path.getmtime(os.path.join(_PKG_DIR, f))
    for f in os.listdir(_PKG_DIR)
    if f.endswith(".py")
)

CTRL_PORT = 5556
DATA_PORT  = 5557

# Concurrency classification
OUTPUT_CMDS = {"generate", "generate_pink", "sweep_level", "sweep_frequency"}
INPUT_CMDS  = {"monitor_thd", "monitor_spectrum"}
EXCLUSIVE   = {"plot", "calibrate"}
AUDIO_CMDS  = OUTPUT_CMDS | INPUT_CMDS | EXCLUSIVE


# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------

def _pub(q, topic, frame):
    """Enqueue a DATA frame for the main thread to send."""
    q.put(topic.encode() + b" " + json.dumps(frame).encode())


def _warmup(engine, n_blocks=WARMUP_REPS):
    dur = engine.blocksize / engine.samplerate * n_blocks
    engine.capture_block(dur)


def _downsample_spectrum(spec, freqs, max_pts=1000):
    if len(freqs) <= max_pts:
        return spec, freqs
    idx   = np.unique(np.round(np.geomspace(1, len(freqs), max_pts)).astype(int) - 1)
    return spec[idx], freqs[idx]


def _sweep_point_frame(r, cal, n, cmd_name, level_dbfs, freq_hz=None):
    """Build a sweep_point DATA frame from an analyze() result dict."""
    out_vrms  = cal.out_vrms(level_dbfs, freq_hz) if cal else None
    in_vrms   = cal.in_vrms(r["linear_rms"], freq_hz) if cal else None
    in_dbu    = vrms_to_dbu(in_vrms)  if in_vrms  is not None else None
    out_dbu   = vrms_to_dbu(out_vrms) if out_vrms is not None else None
    gain_db   = (in_dbu - out_dbu
                 if in_dbu is not None and out_dbu is not None else None)

    # Downsample spectrum for transport
    spec_ds, freqs_ds = _downsample_spectrum(r["spectrum"][1:], r["freqs"][1:])

    frame = {
        "type":              "sweep_point",
        "cmd":               cmd_name,
        "n":                 n,
        "drive_db":          float(r.get("drive_db", level_dbfs)),
        "thd_pct":           float(r["thd_pct"]),
        "thdn_pct":          float(r["thdn_pct"]),
        "fundamental_hz":    float(r["fundamental_hz"]) if "fundamental_hz" in r else None,
        "fundamental_dbfs":  float(r["fundamental_dbfs"]),
        "linear_rms":        float(r["linear_rms"]),
        "harmonic_levels":   [[float(hf), float(ha)] for hf, ha in r["harmonic_levels"]],
        "noise_floor_dbfs":  float(r["noise_floor_dbfs"]),
        "spectrum":          spec_ds.tolist(),
        "freqs":             freqs_ds.tolist(),
        "clipping":          bool(r.get("clipping", False)),
        "ac_coupled":        bool(r.get("ac_coupled", False)),
        "out_vrms":          out_vrms,
        "out_dbu":           out_dbu,
        "in_vrms":           in_vrms,
        "in_dbu":            in_dbu,
        "gain_db":           gain_db,
        "vrms_at_0dbfs_out": cal.vrms_at_0dbfs_out if cal else None,
        "vrms_at_0dbfs_in":  cal.vrms_at_0dbfs_in  if cal else None,
    }
    if freq_hz is not None:
        frame["freq_hz"] = float(freq_hz)
        frame["freq"]    = float(freq_hz)   # alias for plotting.py
    return frame


# ---------------------------------------------------------------------------
# Worker functions (run in background threads)
# ---------------------------------------------------------------------------

def _worker_sweep_level_gen(pub_q, stop_ev, cfg, cmd):
    """Output-only level sweep: ramps amplitude from start→stop over duration at fixed freq."""
    freq      = cmd["freq_hz"]
    start_db  = cmd["start_dbfs"]
    stop_db   = cmd["stop_dbfs"]
    duration  = cmd.get("duration", 1.0)
    out_port  = cmd["_out_port"]

    engine = JackEngine()
    try:
        engine.start(output_ports=out_port)
        start_amp = 10.0 ** (start_db / 20.0)
        stop_amp  = 10.0 ** (stop_db  / 20.0)
        engine.set_tone(freq, start_amp)
        t_start = time.time()
        while not stop_ev.is_set():
            elapsed = time.time() - t_start
            if elapsed >= duration:
                break
            t_norm = elapsed / duration
            db_now = start_db + (stop_db - start_db) * t_norm
            engine.set_tone(freq, 10.0 ** (db_now / 20.0))
            time.sleep(0.01)
    except Exception as e:
        _pub(pub_q, "error", {"cmd": "sweep_level", "message": str(e)})
        return
    finally:
        engine.set_silence()
        engine.stop()
    _pub(pub_q, "done", {"cmd": "sweep_level"})


def _worker_sweep_frequency_gen(pub_q, stop_ev, cfg, cmd):
    """Output-only frequency sweep: chirps from start→stop over duration at fixed level."""
    start_hz   = cmd["start_hz"]
    stop_hz    = cmd["stop_hz"]
    level_dbfs = cmd["level_dbfs"]
    duration   = cmd.get("duration", 1.0)
    amplitude  = 10.0 ** (level_dbfs / 20.0)
    out_port   = cmd["_out_port"]

    engine = JackEngine()
    try:
        engine.start(output_ports=out_port)
        engine.set_tone(float(start_hz), amplitude)
        t_start = time.time()
        while not stop_ev.is_set():
            elapsed = time.time() - t_start
            if elapsed >= duration:
                break
            t_norm = elapsed / duration
            freq = start_hz * (stop_hz / start_hz) ** t_norm
            engine.set_tone(float(freq), amplitude)
            time.sleep(0.01)
    except Exception as e:
        _pub(pub_q, "error", {"cmd": "sweep_frequency", "message": str(e)})
        return
    finally:
        engine.set_silence()
        engine.stop()
    _pub(pub_q, "done", {"cmd": "sweep_frequency"})


def _worker_plot(pub_q, stop_ev, cfg, cmd):
    """Blocking point-by-point frequency measurement (formerly sweep_frequency)."""
    start_hz   = cmd["start_hz"]
    stop_hz    = cmd["stop_hz"]
    level_dbfs = cmd["level_dbfs"]
    ppd        = cmd.get("ppd", 10)
    duration   = cmd.get("duration", 1.0)
    cal        = Calibration.load(output_channel=cfg["output_channel"],
                                  input_channel=cfg["input_channel"])

    n_decades = np.log10(stop_hz / start_hz)
    n_points  = max(2, int(round(n_decades * ppd)))
    freqs     = np.unique(np.round(np.geomspace(start_hz, stop_hz, n_points)).astype(int))
    amplitude = 10.0 ** (level_dbfs / 20.0)

    out_port  = cmd["_out_port"]
    in_port   = cmd["_in_port"]
    xruns = 0
    n = 0
    engine = JackEngine()
    try:
        engine.start(output_ports=out_port, input_port=in_port)
        for freq in freqs:
            if stop_ev.is_set():
                break
            dur = max(duration, 10.0 / freq)
            engine.set_tone(float(freq), amplitude)
            _warmup(engine)
            data = engine.capture_block(dur)
            rec  = data.reshape(-1, 1)
            r    = analyze(rec, sr=engine.samplerate, fundamental=float(freq))
            if "error" in r:
                continue
            r["drive_db"] = level_dbfs
            frame = _sweep_point_frame(r, cal, n, "plot", level_dbfs,
                                       freq_hz=float(freq))
            _pub(pub_q, "data", frame)
            n += 1
        xruns = engine.xruns
    except Exception as e:
        _pub(pub_q, "error", {"cmd": "plot", "message": str(e)})
        return
    finally:
        engine.set_silence()
        engine.stop()
    _pub(pub_q, "done", {"cmd": "plot", "n_points": n, "xruns": xruns})


def _worker_monitor_thd(pub_q, stop_ev, cfg, cmd):
    freq     = cmd["freq_hz"]
    interval = cmd.get("interval", 1.0)
    cal      = Calibration.load(output_channel=cfg["output_channel"],
                                input_channel=cfg["input_channel"])
    duration = max(0.1, interval)
    in_port  = cmd["_in_port"]
    engine = JackEngine()
    try:
        engine.start(input_port=in_port)
        while not stop_ev.is_set():
            data = engine.capture_block(duration)
            rec  = data.reshape(-1, 1)
            r    = analyze(rec, sr=engine.samplerate, fundamental=freq)
            if "error" in r:
                continue
            in_vrms = cal.in_vrms(r["linear_rms"], freq) if (cal and cal.input_ok) else None
            in_dbu  = vrms_to_dbu(in_vrms)         if in_vrms is not None    else None
            _pub(pub_q, "data", {
                "type":              "thd_point",
                "cmd":               "monitor_thd",
                "freq_hz":           freq,
                "thd_pct":           float(r["thd_pct"]),
                "thdn_pct":          float(r["thdn_pct"]),
                "fundamental_dbfs":  float(r["fundamental_dbfs"]),
                "in_dbu":            in_dbu,
                "clipping":          bool(r.get("clipping", False)),
                "xruns":             engine.xruns,
            })
    except Exception as e:
        _pub(pub_q, "error", {"cmd": "monitor_thd", "message": str(e)})
        return
    finally:
        engine.stop()
    _pub(pub_q, "done", {"cmd": "monitor_thd"})


def _worker_monitor_spectrum(pub_q, stop_ev, cfg, cmd):
    freq     = cmd["freq_hz"]
    interval = cmd.get("interval", 0.2)
    cal      = Calibration.load(output_channel=cfg["output_channel"],
                                input_channel=cfg["input_channel"])
    duration = max(0.05, interval)
    in_port  = cmd["_in_port"]
    engine = JackEngine()
    try:
        engine.start(input_port=in_port)
        while not stop_ev.is_set():
            data = engine.capture_block(duration)
            rec  = data.reshape(-1, 1)
            # Auto-detect dominant frequency: find the highest spectral peak
            # above 20 Hz so THD is correct regardless of the hint in cmd.
            mono_d    = data.astype(np.float64)
            spec_d    = np.abs(np.fft.rfft(mono_d))
            freqs_d   = np.fft.rfftfreq(len(mono_d), 1.0 / engine.samplerate)
            min_bin   = max(1, int(20.0 * len(mono_d) / engine.samplerate))
            peak_bin  = int(np.argmax(spec_d[min_bin:])) + min_bin
            detected  = float(freqs_d[peak_bin]) if spec_d[peak_bin] > 1e-6 else freq
            r    = analyze(rec, sr=engine.samplerate, fundamental=detected)
            if "error" in r:
                continue
            spec_ds, freqs_ds = _downsample_spectrum(r["spectrum"][1:], r["freqs"][1:])
            in_vrms = cal.in_vrms(r["linear_rms"], detected) if (cal and cal.input_ok) else None
            in_dbu  = vrms_to_dbu(in_vrms)         if in_vrms is not None    else None
            _pub(pub_q, "data", {
                "type":     "spectrum",
                "cmd":      "monitor_spectrum",
                "freq_hz":  detected,
                "sr":       engine.samplerate,
                "freqs":    freqs_ds.tolist(),
                "spectrum": spec_ds.tolist(),
                "thd_pct":  float(r["thd_pct"]),
                "thdn_pct": float(r["thdn_pct"]),
                "in_dbu":   in_dbu,
                "clipping": bool(r.get("clipping", False)),
                "xruns":    engine.xruns,
            })
    except Exception as e:
        _pub(pub_q, "error", {"cmd": "monitor_spectrum", "message": str(e)})
        return
    finally:
        engine.stop()
    _pub(pub_q, "done", {"cmd": "monitor_spectrum"})


def _worker_generate(pub_q, stop_ev, cfg, cmd):
    freq       = cmd["freq_hz"]
    level_dbfs = cmd["level_dbfs"]
    amplitude  = 10.0 ** (level_dbfs / 20.0)
    out_ports  = cmd["_out_ports"]   # pre-resolved in handle()

    engine = None
    try:
        engine = JackEngine()
        engine.set_tone(freq, amplitude)
        engine.start(output_ports=out_ports)
        stop_ev.wait()
    except Exception as e:
        _pub(pub_q, "error", {"cmd": "generate", "message": str(e)})
        return
    finally:
        if engine is not None:
            engine.set_silence()
            engine.stop()
    _pub(pub_q, "done", {"cmd": "generate"})


def _worker_generate_pink(pub_q, stop_ev, cfg, cmd):
    level_dbfs = cmd["level_dbfs"]
    amplitude  = 10.0 ** (level_dbfs / 20.0)
    out_ports  = cmd["_out_ports"]

    engine = None
    try:
        engine = JackEngine()
        engine.set_pink_noise(amplitude)
        engine.start(output_ports=out_ports)
        stop_ev.wait()
    except Exception as e:
        _pub(pub_q, "error", {"cmd": "generate_pink", "message": str(e)})
        return
    finally:
        if engine is not None:
            engine.set_silence()
            engine.stop()
    _pub(pub_q, "done", {"cmd": "generate_pink"})


def _worker_calibrate(pub_q, stop_ev, cal_q, cfg, cmd):
    from .jack_calibration import run_calibration_jack_zmq
    ref_dbfs       = cmd.get("ref_dbfs", -10.0)
    output_channel = cmd.get("output_channel", cfg["output_channel"])
    input_channel  = cmd.get("input_channel",  cfg["input_channel"])
    run_calibration_jack_zmq(
        pub_q=pub_q,
        cal_q=cal_q,
        output_channel=output_channel,
        input_channel=input_channel,
        ref_dbfs=ref_dbfs,
        dmm_host=cfg.get("dmm_host"),
        range_start_hz=cfg.get("range_start_hz", 20.0),
        range_stop_hz=cfg.get("range_stop_hz", 20000.0),
        output_port=cfg.get("output_port"),
        input_port=cfg.get("input_port"),
    )


# ---------------------------------------------------------------------------
# Server main loop
# ---------------------------------------------------------------------------

def run_server(ctrl_port=CTRL_PORT, data_port=DATA_PORT):
    """Start the server. Always runs silently (auto-spawned by client via --serve).

    Binds to * if server_enabled=True in config, otherwise 127.0.0.1.
    """
    try:
        import zmq
        from zmq.utils.monitor import recv_monitor_message
    except ImportError:
        return

    cfg = load_config()
    bind_addr = "*" if cfg.get("server_enabled", False) else "127.0.0.1"

    # Check whether a server is already running on this port
    _probe = zmq.Context()
    _req   = _probe.socket(zmq.REQ)
    _req.setsockopt(zmq.LINGER,   0)
    _req.setsockopt(zmq.RCVTIMEO, 500)
    _req.connect(f"tcp://localhost:{ctrl_port}")
    try:
        _req.send_json({"cmd": "status"})
        _req.recv_json()
        _req.close(); _probe.term()
        # Another server is already up — nothing to do
        return
    except zmq.Again:
        pass   # nothing there — proceed to bind
    finally:
        try: _req.close()
        except Exception: pass
        try: _probe.term()
        except Exception: pass

    ctx       = zmq.Context()
    sock_ctrl = ctx.socket(zmq.REP)
    try:
        sock_ctrl.bind(f"tcp://{bind_addr}:{ctrl_port}")
    except zmq.ZMQError:
        sock_ctrl.close(); ctx.term()
        return
    sock_data = ctx.socket(zmq.PUB)
    try:
        sock_data.bind(f"tcp://{bind_addr}:{data_port}")
    except zmq.ZMQError:
        sock_ctrl.close(); sock_data.close(); ctx.term()
        return

    # Monitor socket for tracking connected clients
    mon = sock_ctrl.get_monitor_socket(zmq.EVENT_ACCEPTED | zmq.EVENT_DISCONNECTED)
    connections = {}   # endpoint -> connect_time (float)
    state = {
        "bind_addr":    bind_addr,
        "ctrl_ep":      f"tcp://{bind_addr}:{ctrl_port}",
        "data_ep":      f"tcp://{bind_addr}:{data_port}",
        "gpio_handler": None,
    }

    pub_q       = queue.Queue()
    cal_q       = queue.Queue()
    should_quit = [False]
    workers     = {}   # {cmd_name: {"thread": Thread, "stop": Event}}

    # Auto-start GPIO if a port is configured
    if cfg.get("gpio_port"):
        try:
            from ..gpio.gpio import GpioHandler
            _gpio = GpioHandler(cfg["gpio_port"],
                                log_fn=lambda msg: pub_q.put(b"gpio " + json.dumps({"msg": msg}).encode()))
            _gpio.start()
            state["gpio_handler"] = _gpio
        except Exception as e:
            print(f"[GPIO] auto-start failed: {e}", file=sys.stderr)

    poller = zmq.Poller()
    poller.register(sock_ctrl, zmq.POLLIN)
    poller.register(mon, zmq.POLLIN)

    def _rebind(new_addr):
        """Rebind both sockets to new_addr. Rollback on failure. Returns True on success."""
        if new_addr == state["bind_addr"]:
            return True   # already in requested mode
        old_ctrl_ep = state["ctrl_ep"]
        old_data_ep = state["data_ep"]
        new_ctrl_ep = f"tcp://{new_addr}:{ctrl_port}"
        new_data_ep = f"tcp://{new_addr}:{data_port}"
        try:
            sock_ctrl.unbind(old_ctrl_ep)
            sock_ctrl.bind(new_ctrl_ep)
        except zmq.ZMQError:
            try: sock_ctrl.bind(old_ctrl_ep)
            except Exception: pass
            return False
        try:
            sock_data.unbind(old_data_ep)
            sock_data.bind(new_data_ep)
        except zmq.ZMQError:
            # rollback ctrl then restore data
            try: sock_data.bind(old_data_ep)
            except Exception: pass
            try: sock_ctrl.unbind(new_ctrl_ep)
            except Exception: pass
            try: sock_ctrl.bind(old_ctrl_ep)
            except Exception: pass
            return False
        state["bind_addr"] = new_addr
        state["ctrl_ep"]   = new_ctrl_ep
        state["data_ep"]   = new_data_ep
        return True

    def _drain_pub():
        while True:
            try:
                sock_data.send(pub_q.get_nowait())
            except queue.Empty:
                break

    def _cleanup_workers():
        dead = [n for n, w in list(workers.items()) if not w["thread"].is_alive()]
        for n in dead:
            del workers[n]

    def _can_start(name):
        _cleanup_workers()
        alive = set(workers.keys())
        if not alive:
            return True, None
        if name in EXCLUSIVE or alive & EXCLUSIVE:
            return False, f"{', '.join(alive)} running — send stop first"
        if name in OUTPUT_CMDS and alive & OUTPUT_CMDS:
            return False, f"{', '.join(alive & OUTPUT_CMDS)} already running — send stop first"
        if name in INPUT_CMDS and alive & INPUT_CMDS:
            return False, f"{', '.join(alive & INPUT_CMDS)} already running — send stop first"
        return True, None

    def _spawn(name, target, *args):
        # args = (pub_q, *rest) — inject per-worker stop event after pub_q
        ev = threading.Event()
        t  = threading.Thread(target=target, args=(args[0], ev) + args[1:], daemon=True)
        t.start()
        workers[name] = {"thread": t, "stop": ev}

    def handle(cmd):
        nonlocal cfg
        name = cmd.get("cmd", "")

        if name == "status":
            _cleanup_workers()
            alive = list(workers.keys())
            addr  = state["bind_addr"]
            return {"ok": True, "busy": bool(alive),
                    "running_cmd": alive[0] if alive else None, "src_mtime": _SRC_MTIME,
                    "listen_mode": "public" if addr == "*" else "local",
                    "server_enabled": cfg.get("server_enabled", False)}

        if name == "quit":
            should_quit[0] = True
            return {"ok": True}

        if name == "stop":
            target = cmd.get("name")
            if target and target in workers:
                workers[target]["stop"].set()
                workers[target]["thread"].join(timeout=5.0)
                del workers[target]
            else:
                for w in workers.values():
                    w["stop"].set()
                cal_q.put(None)   # unblock calibration worker if it's waiting
                for w in workers.values():
                    w["thread"].join(timeout=5.0)
                workers.clear()
            return {"ok": True}

        if name == "cal_reply":
            cal_q.put(cmd.get("vrms"))
            return {"ok": True}

        if name == "get_calibration":
            out_ch = cmd.get("output_channel", cfg["output_channel"])
            in_ch  = cmd.get("input_channel",  cfg["input_channel"])
            cal    = Calibration.load(output_channel=out_ch, input_channel=in_ch)
            if cal is None:
                return {"ok": True, "found": False}
            return {"ok": True, "found": True,
                    "vrms_at_0dbfs_out": cal.vrms_at_0dbfs_out,
                    "vrms_at_0dbfs_in":  cal.vrms_at_0dbfs_in,
                    "ref_dbfs":          cal.ref_dbfs,
                    "key":               cal.key}

        if name == "list_calibrations":
            cals = Calibration.load_all()
            return {"ok": True, "calibrations": [
                {"key":               c.key,
                 "vrms_at_0dbfs_out": c.vrms_at_0dbfs_out,
                 "vrms_at_0dbfs_in":  c.vrms_at_0dbfs_in,
                 "response_pts":      len(c.response_curve) if c.response_curve else 0,
                 "response_range":    [c.response_curve[0][0], c.response_curve[-1][0]]
                                      if c.response_curve else None,
                 "response_max_dev":  max(abs(d) for f, d in c.response_curve)
                                      if c.response_curve else None}
                for c in cals
            ]}

        if name == "devices":
            try:
                playback, capture = find_ports()
                return {"ok": True,
                        "playback":       playback,
                        "capture":        capture,
                        "output_channel": cfg["output_channel"],
                        "input_channel":  cfg["input_channel"],
                        "output_port":    cfg.get("output_port"),
                        "input_port":     cfg.get("input_port")}
            except Exception as e:
                return {"ok": False, "error": str(e)}

        if name == "setup":
            update = cmd.get("update", {})
            if update:
                if "output_channel" in update or "input_channel" in update:
                    try:
                        playback, capture = find_ports()
                        if "output_channel" in update:
                            update["output_port"] = port_name(playback, update["output_channel"])
                        if "input_channel" in update:
                            update["input_port"] = port_name(capture, update["input_channel"])
                    except Exception:
                        pass  # non-fatal: fall back to index-only
                cfg = save_config(update)
            return {"ok": True, "config": dict(cfg)}

        if name == "dmm_read":
            dmm_host = cfg.get("dmm_host")
            if not dmm_host:
                return {"ok": False,
                        "error": "no DMM configured on server — run: ac setup dmm <host>"}
            try:
                from . import dmm as _dmm
                vrms = _dmm.read_ac_vrms(dmm_host, n=3)
                try:
                    idn = _dmm.identify(dmm_host)
                except Exception:
                    idn = None
                return {"ok": True, "vrms": vrms, "idn": idn}
            except Exception as e:
                return {"ok": False, "error": str(e)}

        # --- Audio commands: concurrency check ---
        if name in AUDIO_CMDS:
            ok, err = _can_start(name)
            if not ok:
                return {"ok": False, "error": f"busy: {err}"}

        # Resolve ports before spawning so we can validate indices immediately
        # and include port names in the ack.
        if name in ("sweep_level", "sweep_frequency"):
            try:
                playback, _ = find_ports()
                out_port = resolve_port(playback, cfg.get("output_port"), cfg["output_channel"])
            except Exception as e:
                return {"ok": False, "error": f"port error: {e}"}
            cmd["_out_port"] = out_port

        if name == "plot":
            try:
                playback, capture = find_ports()
                out_port = resolve_port(playback, cfg.get("output_port"), cfg["output_channel"])
                in_port  = resolve_port(capture,  cfg.get("input_port"),  cfg["input_channel"])
            except Exception as e:
                return {"ok": False, "error": f"port error: {e}"}
            cmd["_out_port"] = out_port
            cmd["_in_port"]  = in_port

        if name in ("monitor_thd", "monitor_spectrum"):
            try:
                _, capture = find_ports()
                in_port = resolve_port(capture, cfg.get("input_port"), cfg["input_channel"])
            except Exception as e:
                return {"ok": False, "error": f"port error: {e}"}
            cmd["_in_port"] = in_port

        if name in ("generate", "generate_pink"):
            channels = cmd.get("channels")
            try:
                playback, _ = find_ports()
                if channels:
                    out_ports = [port_name(playback, ch) for ch in channels]
                else:
                    out_ports = [resolve_port(playback, cfg.get("output_port"), cfg["output_channel"])]
            except Exception as e:
                return {"ok": False, "error": f"port error: {e}"}
            cmd["_out_ports"] = out_ports

        if name == "sweep_level":
            _spawn("sweep_level", _worker_sweep_level_gen, pub_q, cfg, cmd)
            return {"ok": True, "out_port": cmd["_out_port"]}

        if name == "sweep_frequency":
            _spawn("sweep_frequency", _worker_sweep_frequency_gen, pub_q, cfg, cmd)
            return {"ok": True, "out_port": cmd["_out_port"]}

        if name == "plot":
            _spawn("plot", _worker_plot, pub_q, cfg, cmd)
            return {"ok": True,
                    "out_port": cmd["_out_port"], "in_port": cmd["_in_port"]}

        if name == "monitor_thd":
            _spawn("monitor_thd", _worker_monitor_thd, pub_q, cfg, cmd)
            return {"ok": True, "in_port": cmd["_in_port"]}

        if name == "monitor_spectrum":
            _spawn("monitor_spectrum", _worker_monitor_spectrum, pub_q, cfg, cmd)
            return {"ok": True, "in_port": cmd["_in_port"]}

        if name == "generate":
            _spawn("generate", _worker_generate, pub_q, cfg, cmd)
            return {"ok": True, "out_ports": cmd["_out_ports"]}

        if name == "generate_pink":
            _spawn("generate_pink", _worker_generate_pink, pub_q, cfg, cmd)
            return {"ok": True, "out_ports": cmd["_out_ports"]}

        if name == "calibrate":
            # Drain stale cal_q entries from previous stop commands
            while not cal_q.empty():
                try:
                    cal_q.get_nowait()
                except queue.Empty:
                    break
            _spawn("calibrate", _worker_calibrate, pub_q, cal_q, cfg, cmd)
            return {"ok": True}

        if name == "server_enable":
            save_config({"server_enabled": True})
            cfg = load_config()
            _rebind("*")
            addr = state["bind_addr"]
            return {"ok": True, "bind_addr": addr,
                    "listen_mode": "public" if addr == "*" else "local"}

        if name == "server_disable":
            save_config({"server_enabled": False})
            cfg = load_config()
            _rebind("127.0.0.1")
            addr = state["bind_addr"]
            return {"ok": True, "bind_addr": addr,
                    "listen_mode": "public" if addr == "*" else "local"}

        if name == "server_connections":
            _cleanup_workers()
            addr = state["bind_addr"]
            return {"ok": True,
                    "listen_mode":   "public" if addr == "*" else "local",
                    "ctrl_endpoint": state["ctrl_ep"],
                    "data_endpoint": state["data_ep"],
                    "clients":       list(connections.keys()),
                    "workers":       list(workers.keys())}

        if name == "gpio_status":
            handler = state.get("gpio_handler")
            if handler is None:
                return {"ok": True, "active": False}
            return {"ok": True, "active": True, **handler.status}

        if name == "gpio_setup":
            port = cmd.get("port")
            if state["gpio_handler"] is not None:
                state["gpio_handler"].stop()
                state["gpio_handler"] = None
            if port:
                try:
                    from ..gpio.gpio import GpioHandler
                    handler = GpioHandler(
                        port,
                        log_fn=lambda msg: pub_q.put(b"gpio " + json.dumps({"msg": msg}).encode()),
                    )
                    handler.start()
                    state["gpio_handler"] = handler
                except Exception as e:
                    return {"ok": False, "error": str(e)}
            return {"ok": True}

        return {"ok": False, "error": f"unknown command: {name!r}"}

    try:
        while not should_quit[0]:
            _drain_pub()
            events = dict(poller.poll(50))
            _drain_pub()

            # Process monitor events (connection tracking)
            if mon in events:
                while True:
                    try:
                        msg = recv_monitor_message(mon, flags=zmq.NOBLOCK)
                        event    = msg["event"]
                        endpoint = msg.get("endpoint", b"")
                        if isinstance(endpoint, bytes):
                            endpoint = endpoint.decode("utf-8", errors="replace")
                        if event == zmq.EVENT_ACCEPTED:
                            connections[endpoint] = time.time()
                        elif event == zmq.EVENT_DISCONNECTED:
                            connections.pop(endpoint, None)
                    except zmq.Again:
                        break

            if sock_ctrl not in events:
                continue

            raw = sock_ctrl.recv()
            try:
                cmd = json.loads(raw)
            except Exception:
                sock_ctrl.send(json.dumps(
                    {"ok": False, "error": "invalid JSON"}).encode())
                continue

            reply = handle(cmd)
            sock_ctrl.send(json.dumps(reply).encode())

    except KeyboardInterrupt:
        for w in workers.values():
            w["stop"].set()
        cal_q.put(None)
        for w in workers.values():
            w["thread"].join(timeout=5.0)
    finally:
        if state["gpio_handler"] is not None:
            state["gpio_handler"].stop()
        sock_ctrl.disable_monitor()
        mon.close()
        sock_ctrl.close()
        sock_data.close()
        ctx.term()
