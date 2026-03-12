# server.py -- ZMQ REP+PUB server
# CTRL REP port 5556: receives commands, replies with ack
# DATA PUB port 5557: streams measurement results
import json
import queue
import threading
import numpy as np
from .audio            import find_ports, port_name, JackEngine
from .analysis         import analyze
from .conversions      import vrms_to_dbu
from .constants        import WARMUP_REPS
from .jack_calibration import Calibration
from .config           import load as load_config, save as save_config

CTRL_PORT = 5556
DATA_PORT  = 5557


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
    out_vrms  = cal.out_vrms(level_dbfs) if cal else None
    in_vrms   = cal.in_vrms(r["linear_rms"]) if cal else None
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

def _worker_sweep_level(pub_q, stop_ev, cfg, cmd):
    freq      = cmd["freq_hz"]
    start_db  = cmd["start_dbfs"]
    stop_db   = cmd["stop_dbfs"]
    step_db   = cmd["step_db"]
    duration  = cmd.get("duration", 1.0)
    cal       = Calibration.load(output_channel=cfg["output_channel"],
                                 input_channel=cfg["input_channel"],
                                 freq=freq)

    levels_db = np.arange(start_db, stop_db + step_db * 0.5, step_db)
    playback, capture = find_ports()
    out_port = port_name(playback, cfg["output_channel"])
    in_port  = port_name(capture,  cfg["input_channel"])
    xruns = 0
    n = 0
    engine = JackEngine()
    try:
        engine.start(output_ports=out_port, input_port=in_port)
        for level_db in levels_db:
            if stop_ev.is_set():
                break
            engine.set_tone(freq, 10.0 ** (level_db / 20.0))
            _warmup(engine)
            data = engine.capture_block(duration)
            rec  = data.reshape(-1, 1)
            r    = analyze(rec, sr=engine.samplerate, fundamental=freq)
            if "error" in r:
                continue
            r["drive_db"] = level_db
            frame = _sweep_point_frame(r, cal, n, "sweep_level", level_db)
            _pub(pub_q, "data", frame)
            n += 1
        xruns = engine.xruns
    except Exception as e:
        _pub(pub_q, "error", {"cmd": "sweep_level", "message": str(e)})
        return
    finally:
        engine.set_silence()
        engine.stop()
    _pub(pub_q, "done", {"cmd": "sweep_level", "n_points": n, "xruns": xruns})


def _worker_sweep_frequency(pub_q, stop_ev, cfg, cmd):
    start_hz   = cmd["start_hz"]
    stop_hz    = cmd["stop_hz"]
    level_dbfs = cmd["level_dbfs"]
    ppd        = cmd.get("ppd", 10)
    duration   = cmd.get("duration", 1.0)
    cal        = Calibration.load(output_channel=cfg["output_channel"],
                                  input_channel=cfg["input_channel"],
                                  freq=1000)

    n_decades = np.log10(stop_hz / start_hz)
    n_points  = max(2, int(round(n_decades * ppd)))
    freqs     = np.unique(np.round(np.geomspace(start_hz, stop_hz, n_points)).astype(int))
    amplitude = 10.0 ** (level_dbfs / 20.0)

    playback, capture = find_ports()
    out_port = port_name(playback, cfg["output_channel"])
    in_port  = port_name(capture,  cfg["input_channel"])
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
            frame = _sweep_point_frame(r, cal, n, "sweep_frequency", level_dbfs,
                                       freq_hz=float(freq))
            _pub(pub_q, "data", frame)
            n += 1
        xruns = engine.xruns
    except Exception as e:
        _pub(pub_q, "error", {"cmd": "sweep_frequency", "message": str(e)})
        return
    finally:
        engine.set_silence()
        engine.stop()
    _pub(pub_q, "done", {"cmd": "sweep_frequency", "n_points": n, "xruns": xruns})


def _worker_monitor_thd(pub_q, stop_ev, cfg, cmd):
    freq       = cmd["freq_hz"]
    level_dbfs = cmd["level_dbfs"]
    interval   = cmd.get("interval", 1.0)
    cal        = Calibration.load(output_channel=cfg["output_channel"],
                                  input_channel=cfg["input_channel"],
                                  freq=freq)
    amplitude = 10.0 ** (level_dbfs / 20.0)
    duration  = max(0.1, interval)

    playback, capture = find_ports()
    out_port = port_name(playback, cfg["output_channel"])
    in_port  = port_name(capture,  cfg["input_channel"])
    engine = JackEngine()
    try:
        engine.start(output_ports=out_port, input_port=in_port)
        engine.set_tone(freq, amplitude)
        while not stop_ev.is_set():
            data = engine.capture_block(duration)
            rec  = data.reshape(-1, 1)
            r    = analyze(rec, sr=engine.samplerate, fundamental=freq)
            if "error" in r:
                continue
            in_vrms  = cal.in_vrms(r["linear_rms"])    if (cal and cal.input_ok)  else None
            in_dbu   = vrms_to_dbu(in_vrms)            if in_vrms is not None     else None
            out_vrms = cal.out_vrms(level_dbfs)        if (cal and cal.output_ok) else None
            out_dbu  = vrms_to_dbu(out_vrms)           if out_vrms is not None    else None
            gain_db  = (in_dbu - out_dbu
                        if in_dbu is not None and out_dbu is not None else None)
            _pub(pub_q, "data", {
                "type":              "thd_point",
                "cmd":               "monitor_thd",
                "freq_hz":           freq,
                "thd_pct":           float(r["thd_pct"]),
                "thdn_pct":          float(r["thdn_pct"]),
                "fundamental_dbfs":  float(r["fundamental_dbfs"]),
                "in_dbu":            in_dbu,
                "out_dbu":           out_dbu,
                "gain_db":           gain_db,
                "clipping":          bool(r.get("clipping", False)),
                "xruns":             engine.xruns,
            })
    except Exception as e:
        _pub(pub_q, "error", {"cmd": "monitor_thd", "message": str(e)})
        return
    finally:
        engine.set_silence()
        engine.stop()
    _pub(pub_q, "done", {"cmd": "monitor_thd"})


def _worker_monitor_spectrum(pub_q, stop_ev, cfg, cmd):
    freq       = cmd["freq_hz"]
    level_dbfs = cmd["level_dbfs"]
    interval   = cmd.get("interval", 0.2)
    cal        = Calibration.load(output_channel=cfg["output_channel"],
                                  input_channel=cfg["input_channel"],
                                  freq=freq)
    amplitude = 10.0 ** (level_dbfs / 20.0)
    duration  = max(0.05, interval)

    playback, capture = find_ports()
    out_port = port_name(playback, cfg["output_channel"])
    in_port  = port_name(capture,  cfg["input_channel"])
    engine = JackEngine()
    try:
        engine.start(output_ports=out_port, input_port=in_port)
        engine.set_tone(freq, amplitude)
        while not stop_ev.is_set():
            data = engine.capture_block(duration)
            rec  = data.reshape(-1, 1)
            r    = analyze(rec, sr=engine.samplerate, fundamental=freq)
            if "error" in r:
                continue
            spec_ds, freqs_ds = _downsample_spectrum(r["spectrum"][1:], r["freqs"][1:])
            in_vrms = cal.in_vrms(r["linear_rms"]) if (cal and cal.input_ok) else None
            in_dbu  = vrms_to_dbu(in_vrms)         if in_vrms is not None    else None
            _pub(pub_q, "data", {
                "type":     "spectrum",
                "cmd":      "monitor_spectrum",
                "freq_hz":  freq,
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
        engine.set_silence()
        engine.stop()
    _pub(pub_q, "done", {"cmd": "monitor_spectrum"})


def _worker_generate(pub_q, stop_ev, cfg, cmd):
    freq       = cmd["freq_hz"]
    level_dbfs = cmd["level_dbfs"]
    channels   = cmd.get("channels")
    amplitude  = 10.0 ** (level_dbfs / 20.0)

    playback, _ = find_ports()
    if channels:
        out_ports = [port_name(playback, ch) for ch in channels]
    else:
        out_ports = port_name(playback, cfg["output_channel"])

    engine = JackEngine()
    try:
        engine.set_tone(freq, amplitude)
        engine.start(output_ports=out_ports)
        stop_ev.wait()
    except Exception as e:
        _pub(pub_q, "error", {"cmd": "generate", "message": str(e)})
        return
    finally:
        engine.set_silence()
        engine.stop()
    _pub(pub_q, "done", {"cmd": "generate"})


def _worker_calibrate(pub_q, stop_ev, cal_q, cfg, cmd):
    from .jack_calibration import run_calibration_jack_zmq
    freq           = cmd.get("freq_hz", 1000.0)
    ref_dbfs       = cmd.get("ref_dbfs", -10.0)
    output_channel = cmd.get("output_channel", cfg["output_channel"])
    input_channel  = cmd.get("input_channel",  cfg["input_channel"])
    run_calibration_jack_zmq(
        pub_q=pub_q,
        cal_q=cal_q,
        output_channel=output_channel,
        input_channel=input_channel,
        ref_dbfs=ref_dbfs,
        freq=freq,
    )


# ---------------------------------------------------------------------------
# Server main loop
# ---------------------------------------------------------------------------

def run_server(ctrl_port=CTRL_PORT, data_port=DATA_PORT):
    try:
        import zmq
    except ImportError:
        print("  error: pyzmq not installed — run: pip install pyzmq")
        return

    # Check whether a server is already running on this port
    _probe = zmq.Context()
    _req   = _probe.socket(zmq.REQ)
    _req.setsockopt(zmq.LINGER,   0)
    _req.setsockopt(zmq.RCVTIMEO, 500)
    _req.connect(f"tcp://localhost:{ctrl_port}")
    try:
        _req.send_json({"cmd": "status"})
        ack = _req.recv_json()
        _req.close(); _probe.term()
        print(f"  Server already running on port {ctrl_port} "
              f"(busy={ack.get('busy', '?')}, cmd={ack.get('running_cmd')})")
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
        sock_ctrl.bind(f"tcp://*:{ctrl_port}")
    except zmq.ZMQError as e:
        sock_ctrl.close(); ctx.term()
        print(f"  error: cannot bind CTRL port {ctrl_port}: {e}")
        return
    sock_data = ctx.socket(zmq.PUB)
    try:
        sock_data.bind(f"tcp://*:{data_port}")
    except zmq.ZMQError as e:
        sock_ctrl.close(); sock_data.close(); ctx.term()
        print(f"  error: cannot bind DATA port {data_port}: {e}")
        return

    poller = zmq.Poller()
    poller.register(sock_ctrl, zmq.POLLIN)

    pub_q       = queue.Queue()
    cal_q       = queue.Queue()
    stop_ev     = threading.Event()
    worker      = [None]        # [Thread or None]
    running_cmd = [None]        # [str or None]
    cfg         = load_config()

    def _drain_pub():
        while True:
            try:
                sock_data.send(pub_q.get_nowait())
            except queue.Empty:
                break

    def _is_busy():
        return worker[0] is not None and worker[0].is_alive()

    def _spawn(target, *args):
        stop_ev.clear()
        t = threading.Thread(target=target, args=args, daemon=True)
        t.start()
        worker[0] = t

    def handle(cmd):
        nonlocal cfg
        name = cmd.get("cmd", "")

        if name == "status":
            return {"ok": True, "busy": _is_busy(), "running_cmd": running_cmd[0]}

        if name == "stop":
            stop_ev.set()
            if worker[0]:
                worker[0].join(timeout=5.0)
            running_cmd[0] = None
            return {"ok": True}

        if name == "cal_reply":
            cal_q.put(cmd.get("vrms"))
            return {"ok": True}

        if name == "get_calibration":
            out_ch = cmd.get("output_channel", cfg["output_channel"])
            in_ch  = cmd.get("input_channel",  cfg["input_channel"])
            freq   = cmd.get("freq_hz", 1000.0)
            cal    = Calibration.load(output_channel=out_ch, input_channel=in_ch, freq=freq)
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
                {"key": c.key,
                 "vrms_at_0dbfs_out": c.vrms_at_0dbfs_out,
                 "vrms_at_0dbfs_in":  c.vrms_at_0dbfs_in}
                for c in cals
            ]}

        if name == "devices":
            try:
                playback, capture = find_ports()
                return {"ok": True,
                        "playback":       playback,
                        "capture":        capture,
                        "output_channel": cfg["output_channel"],
                        "input_channel":  cfg["input_channel"]}
            except Exception as e:
                return {"ok": False, "error": str(e)}

        if name == "setup":
            update = cmd.get("update", {})
            if update:
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

        # --- Audio commands require JACK ---
        if _is_busy():
            return {"ok": False,
                    "error": f"busy: {running_cmd[0]} running — send stop first"}

        if name == "sweep_level":
            running_cmd[0] = "sweep_level"
            _spawn(_worker_sweep_level, pub_q, stop_ev, cfg, cmd)
            return {"ok": True}

        if name == "sweep_frequency":
            running_cmd[0] = "sweep_frequency"
            _spawn(_worker_sweep_frequency, pub_q, stop_ev, cfg, cmd)
            return {"ok": True}

        if name == "monitor_thd":
            running_cmd[0] = "monitor_thd"
            _spawn(_worker_monitor_thd, pub_q, stop_ev, cfg, cmd)
            return {"ok": True}

        if name == "monitor_spectrum":
            running_cmd[0] = "monitor_spectrum"
            _spawn(_worker_monitor_spectrum, pub_q, stop_ev, cfg, cmd)
            return {"ok": True}

        if name == "generate":
            running_cmd[0] = "generate"
            _spawn(_worker_generate, pub_q, stop_ev, cfg, cmd)
            return {"ok": True}

        if name == "calibrate":
            running_cmd[0] = "calibrate"
            _spawn(_worker_calibrate, pub_q, stop_ev, cal_q, cfg, cmd)
            return {"ok": True}

        return {"ok": False, "error": f"unknown command: {name!r}"}

    print(f"\n  ZMQ server  CTRL tcp://*:{ctrl_port}  DATA tcp://*:{data_port}")
    print(f"  Ctrl+C to stop\n")

    try:
        while True:
            _drain_pub()
            events = dict(poller.poll(50))
            _drain_pub()

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
        print("\n\n  Stopping server...")
        stop_ev.set()
        if worker[0]:
            worker[0].join(timeout=5.0)
    finally:
        sock_ctrl.close()
        sock_data.close()
        ctx.term()
        print("  Server stopped.")
