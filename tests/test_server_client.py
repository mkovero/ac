"""ZMQ integration tests: real server thread + real AcClient.

All tests share one session-scoped server (FakeJackEngine, no JACK daemon).
Each test that starts a worker must drain to a done/error frame before returning
so the server is idle for the next test.
"""
import time
import pytest


# ---------------------------------------------------------------------------
# Helper
# ---------------------------------------------------------------------------

def recv_until(client, done_topics=("done", "error"), max_frames=200, timeout_ms=5000):
    """Receive DATA frames until a terminal topic arrives or max_frames exceeded."""
    frames = []
    for _ in range(max_frames):
        try:
            topic, frame = client.recv_data(timeout_ms=timeout_ms)
        except TimeoutError:
            break
        frames.append((topic, frame))
        if topic in done_topics:
            break
    return frames


def _drain(client, max_frames=200, timeout_ms=1000):
    """Drain residual frames without caring about content."""
    recv_until(client, done_topics=("done", "error"), max_frames=max_frames,
               timeout_ms=timeout_ms)


def _stop_and_drain(client):
    """Send stop and drain until done."""
    client.send_cmd({"cmd": "stop"}, timeout_ms=5000)
    _drain(client, max_frames=500, timeout_ms=3000)


# ---------------------------------------------------------------------------
# Non-audio / status commands
# ---------------------------------------------------------------------------

def test_status(server_client):
    ack = server_client.send_cmd({"cmd": "status"})
    assert ack is not None
    assert ack["ok"] is True
    assert ack["busy"] is False


def test_devices(server_client):
    ack = server_client.send_cmd({"cmd": "devices"})
    assert ack is not None
    assert ack["ok"] is True
    assert "playback" in ack
    assert "capture"  in ack
    assert isinstance(ack["playback"], list)
    assert isinstance(ack["capture"],  list)


def test_setup_update_and_restore(server_client):
    client = server_client
    # Save original
    orig_ack = client.send_cmd({"cmd": "setup", "update": {}})
    orig_ch  = orig_ack["config"]["output_channel"]

    # Update to a sentinel value
    ack = client.send_cmd({"cmd": "setup", "update": {"output_channel": 1}})
    assert ack["ok"] is True
    assert ack["config"]["output_channel"] == 1

    # Restore
    r = client.send_cmd({"cmd": "setup", "update": {"output_channel": orig_ch}})
    assert r["config"]["output_channel"] == orig_ch


def test_get_calibration_not_found(server_client):
    # Use an out-of-range channel combo that will never have a calibration file
    ack = server_client.send_cmd({
        "cmd":            "get_calibration",
        "output_channel": 17,
        "input_channel":  18,
    })
    assert ack is not None
    assert ack["ok"]    is True
    assert ack["found"] is False


def test_list_calibrations_returns_list(server_client):
    ack = server_client.send_cmd({"cmd": "list_calibrations"})
    assert ack is not None
    assert ack["ok"] is True
    assert isinstance(ack["calibrations"], list)


def test_unknown_command(server_client):
    ack = server_client.send_cmd({"cmd": "bogus_cmd_xyz"})
    assert ack is not None
    assert ack["ok"] is False
    assert "unknown" in ack.get("error", "").lower()


# ---------------------------------------------------------------------------
# Sweep level
# ---------------------------------------------------------------------------

def test_sweep_level_frames(server_client):
    """sweep_level is output-only: ack has out_port (no in_port), sends done when finished."""
    client = server_client
    ack = client.send_cmd({
        "cmd":        "sweep_level",
        "freq_hz":    1000.0,
        "start_dbfs": -20.0,
        "stop_dbfs":  -16.0,
        "duration":    0.1,   # short ramp for the test
    })
    assert ack["ok"] is True
    assert "out_port" in ack and ack["out_port"]
    assert "in_port"  not in ack   # output-only: no input port

    frames = recv_until(client, done_topics=("done", "error"), timeout_ms=5000)
    topics = [t for t, _ in frames]
    assert "done" in topics or "error" in topics

    # No sweep_point data frames — sweep is non-blocking output-only
    data_frames = [(t, f) for t, f in frames if t == "data"]
    assert len(data_frames) == 0


def test_plot_fields(server_client):
    """plot (blocking measurement) emits sweep_point frames with required fields."""
    client = server_client
    ack = client.send_cmd({
        "cmd":        "plot",
        "start_hz":   20.0,
        "stop_hz":    200.0,
        "level_dbfs": -20.0,
        "ppd":         2,
    })
    assert ack["ok"] is True
    assert "out_port" in ack and ack["out_port"]
    assert "in_port"  in ack and ack["in_port"]

    frames = recv_until(client, done_topics=("done", "error"), timeout_ms=30000)
    data_frames = [f for t, f in frames if t == "data"]
    assert data_frames, "expected at least one sweep_point from plot"

    for f in data_frames:
        assert f.get("type") == "sweep_point"
        assert "thd_pct"    in f
        assert "thdn_pct"   in f
        assert "drive_db"   in f
        assert "spectrum"   in f
        assert "freqs"      in f


# ---------------------------------------------------------------------------
# Sweep frequency
# ---------------------------------------------------------------------------

def test_sweep_frequency_frames(server_client):
    """sweep_frequency is output-only chirp: ack has out_port (no in_port), sends done."""
    client = server_client
    ack = client.send_cmd({
        "cmd":        "sweep_frequency",
        "start_hz":    20.0,
        "stop_hz":    200.0,
        "level_dbfs": -20.0,
        "duration":    0.1,   # short chirp for the test
    })
    assert ack["ok"] is True
    assert "out_port" in ack and ack["out_port"]
    assert "in_port"  not in ack   # output-only: no input port

    frames = recv_until(client, done_topics=("done", "error"), timeout_ms=5000)
    topics = [t for t, _ in frames]
    assert "done" in topics or "error" in topics

    # No measurement data frames — sweep is non-blocking output-only
    data_frames = [f for t, f in frames if t == "data"]
    assert len(data_frames) == 0


# ---------------------------------------------------------------------------
# Busy guard
# ---------------------------------------------------------------------------

def test_monitor_thd_port_info(server_client):
    """monitor_thd ack must include in_port (input-only — no out_port)."""
    client = server_client
    ack = client.send_cmd({
        "cmd":        "monitor_thd",
        "freq_hz":    1000.0,
        "level_dbfs": -20.0,
        "interval":   0.05,
    })
    assert ack["ok"] is True
    assert "in_port"  in ack and ack["in_port"]
    assert "out_port" not in ack   # monitor is input-only
    _stop_and_drain(client)


def test_bad_channel_returns_error(server_client):
    """A channel index out of range must return ok=False immediately (no crash)."""
    client = server_client
    # Temporarily set an out-of-range output channel
    client.send_cmd({"cmd": "setup", "update": {"output_channel": 99}})
    ack = client.send_cmd({
        "cmd":        "sweep_level",
        "freq_hz":    1000.0,
        "start_dbfs": -20.0,
        "stop_dbfs":  -18.0,
        "duration":    1.0,
    })
    # Restore
    client.send_cmd({"cmd": "setup", "update": {"output_channel": 0}})
    assert ack is not None
    assert ack["ok"] is False
    assert "port" in ack.get("error", "").lower() or "range" in ack.get("error", "").lower()


def test_busy_guard(server_client):
    """Starting a second exclusive command while server is busy must return ok=False."""
    client = server_client

    # Start an infinite monitor
    ack1 = client.send_cmd({
        "cmd":        "monitor_thd",
        "freq_hz":    1000.0,
        "level_dbfs": -20.0,
        "interval":   0.05,
    })
    assert ack1["ok"] is True

    # Immediately try to start a plot (exclusive) while monitor is busy
    ack2 = client.send_cmd({
        "cmd":        "plot",
        "start_hz":   20.0,
        "stop_hz":    200.0,
        "level_dbfs": -20.0,
        "ppd":         2,
    })
    assert ack2 is not None
    assert ack2["ok"] is False
    assert "busy" in ack2.get("error", "").lower()

    _stop_and_drain(client)


# ---------------------------------------------------------------------------
# Stop
# ---------------------------------------------------------------------------

def test_stop_sweep(server_client):
    """After sending stop the server must eventually publish a done frame."""
    client = server_client

    # Long ramp so it's likely still running when we stop
    ack = client.send_cmd({
        "cmd":        "sweep_level",
        "freq_hz":    1000.0,
        "start_dbfs": -60.0,
        "stop_dbfs":    0.0,
        "duration":    10.0,   # 10-second ramp
    })
    assert ack["ok"] is True

    # Send stop (may arrive before or after the sweep finishes — both are OK)
    client.send_cmd({"cmd": "stop"})

    frames = recv_until(client, done_topics=("done", "error"), max_frames=500,
                        timeout_ms=10000)
    topics = [t for t, _ in frames]
    assert "done" in topics or "error" in topics


# ---------------------------------------------------------------------------
# Monitor THD
# ---------------------------------------------------------------------------

def test_monitor_thd_frames(server_client):
    """Monitor should stream thd_point frames."""
    client = server_client

    ack = client.send_cmd({
        "cmd":        "monitor_thd",
        "freq_hz":    1000.0,
        "level_dbfs": -20.0,
        "interval":   0.05,
    })
    assert ack["ok"] is True

    thd_frames = []
    for _ in range(20):
        try:
            topic, frame = client.recv_data(timeout_ms=3000)
        except TimeoutError:
            break
        if topic == "data" and frame.get("type") == "thd_point":
            thd_frames.append(frame)
            if len(thd_frames) >= 3:
                break

    _stop_and_drain(client)

    assert len(thd_frames) >= 1
    f = thd_frames[0]
    assert "thd_pct"  in f
    assert "thdn_pct" in f
    assert "freq_hz"  in f


# ---------------------------------------------------------------------------
# Monitor spectrum
# ---------------------------------------------------------------------------

def test_generate_port_info(server_client):
    """generate ack must include out_ports (list of resolved JACK port names)."""
    client = server_client
    ack = client.send_cmd({
        "cmd":        "generate",
        "freq_hz":    1000.0,
        "level_dbfs": -20.0,
    })
    assert ack is not None
    assert ack["ok"] is True
    assert "out_ports" in ack
    assert isinstance(ack["out_ports"], list)
    assert len(ack["out_ports"]) >= 1
    _stop_and_drain(client)


def test_generate_bad_channel_returns_error(server_client):
    """An out-of-range channel for generate must return ok=False immediately."""
    client = server_client
    ack = client.send_cmd({
        "cmd":        "generate",
        "freq_hz":    1000.0,
        "level_dbfs": -20.0,
        "channels":   [99],
    })
    assert ack is not None
    assert ack["ok"] is False
    assert "port" in ack.get("error", "").lower() or "range" in ack.get("error", "").lower()


def test_monitor_spectrum_frames(server_client):
    """Spectrum monitor should stream spectrum frames."""
    client = server_client

    ack = client.send_cmd({
        "cmd":        "monitor_spectrum",
        "freq_hz":    1000.0,
        "level_dbfs": -20.0,
        "interval":   0.05,
    })
    assert ack["ok"] is True

    spec_frames = []
    for _ in range(20):
        try:
            topic, frame = client.recv_data(timeout_ms=3000)
        except TimeoutError:
            break
        if topic == "data" and frame.get("type") == "spectrum":
            spec_frames.append(frame)
            if len(spec_frames) >= 2:
                break

    _stop_and_drain(client)

    assert len(spec_frames) >= 1
    f = spec_frames[0]
    assert "freqs"    in f
    assert "spectrum" in f
    assert isinstance(f["freqs"],    list)
    assert isinstance(f["spectrum"], list)
