"""ZMQ integration tests: real server thread + real AcClient.

All tests share one session-scoped server (FakeJackEngine, no JACK daemon).
Each test that starts a worker must drain to a done/error frame before returning
so the server is idle for the next test.
"""
import time
import numpy as np
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


# Frame `type` values emitted by plot/plot_level per frequency point.
# Transition-era: daemon emits both the legacy `sweep_point` and the
# tier-prefixed `measurement/frequency_response/point` for the same
# payload. Tests assert membership in this set.
SWEEP_POINT_TYPES = ("sweep_point", "measurement/frequency_response/point")
SPECTRUM_TYPES    = ("spectrum",    "visualize/spectrum")


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

    sp_frames = [f for f in data_frames if f.get("type") in SWEEP_POINT_TYPES]
    assert sp_frames, "expected at least one sweep_point frame from plot"
    for f in sp_frames:
        assert "thd_pct"    in f
        assert "thdn_pct"   in f
        assert "drive_db"   in f
        assert "spectrum"   in f
        assert "freqs"      in f


def test_plot_emits_measurement_report(server_client):
    """plot emits a tier-1 MeasurementReport frame at the end."""
    client = server_client
    ack = client.send_cmd({
        "cmd":        "plot",
        "start_hz":   20.0,
        "stop_hz":    200.0,
        "level_dbfs": -20.0,
        "ppd":         2,
    })
    assert ack["ok"] is True

    frames = recv_until(client, done_topics=("done", "error"), timeout_ms=30000)
    reports = [f for t, f in frames
               if t == "data" and f.get("type") == "measurement/report"]
    assert len(reports) == 1, "expected exactly one measurement/report frame"

    r = reports[0]["report"]
    assert r["schema_version"] == 1
    assert "ac_version" in r
    assert r["method"]["kind"] == "stepped_sine"
    assert r["data"]["kind"]   == "frequency_response"
    assert len(r["data"]["points"]) == r["stimulus"]["n_points"]


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


def test_busy_guard(server_client):
    """Starting a second exclusive command while server is busy must return ok=False."""
    client = server_client

    # Start an infinite monitor
    ack1 = client.send_cmd({
        "cmd":        "monitor_spectrum",
        "freq_hz":    1000.0,
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
# Generate
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



def test_server_enable_disable(server_client):
    """server_enable must reply successfully (not timeout due to rebind) and
    server_disable must revert to local mode."""
    client = server_client

    ack = client.send_cmd({"cmd": "server_enable"}, timeout_ms=3000)
    assert ack is not None, "server_enable timed out (reply lost before rebind?)"
    assert ack["ok"] is True
    assert ack.get("listen_mode") == "public"

    # Give the server a moment to rebind and ZMQ to reconnect, then verify it still responds
    time.sleep(0.4)
    status = client.send_cmd({"cmd": "status"}, timeout_ms=2000)
    assert status is not None
    assert status["listen_mode"] == "public"

    # Restore to local
    ack2 = client.send_cmd({"cmd": "server_disable"}, timeout_ms=3000)
    assert ack2 is not None, "server_disable timed out"
    assert ack2["ok"] is True
    assert ack2.get("listen_mode") == "local"

    time.sleep(0.4)
    status2 = client.send_cmd({"cmd": "status"}, timeout_ms=2000)
    assert status2 is not None
    assert status2["listen_mode"] == "local"


# ---------------------------------------------------------------------------
# Plot level
# ---------------------------------------------------------------------------

def test_plot_level_fields(server_client):
    """plot_level emits sweep_point frames with level-sweep fields."""
    client = server_client
    ack = client.send_cmd({
        "cmd":        "plot_level",
        "freq_hz":    1000.0,
        "start_dbfs": -20.0,
        "stop_dbfs":  -16.0,
        "steps":       3,
    })
    assert ack["ok"] is True
    assert "out_port" in ack and ack["out_port"]
    assert "in_port"  in ack and ack["in_port"]

    frames = recv_until(client, done_topics=("done", "error"), timeout_ms=30000)
    data_frames = [f for t, f in frames if t == "data"]
    assert data_frames, "expected at least one sweep_point from plot_level"

    sp_frames = [f for f in data_frames if f.get("type") in SWEEP_POINT_TYPES]
    assert sp_frames, "expected at least one sweep_point frame from plot_level"
    for f in sp_frames:
        assert f.get("cmd")  == "plot_level"
        assert "thd_pct"    in f
        assert "thdn_pct"   in f
        assert "drive_db"   in f
        assert "spectrum"   in f
        assert "freqs"      in f
        assert "freq_hz"    in f

    # Done frame should have the right cmd
    done_frames = [f for t, f in frames if t == "done"]
    assert done_frames
    assert done_frames[0]["cmd"] == "plot_level"


def test_plot_level_step_count(server_client):
    """plot_level should produce exactly the requested number of points."""
    client = server_client
    ack = client.send_cmd({
        "cmd":        "plot_level",
        "freq_hz":    1000.0,
        "start_dbfs": -20.0,
        "stop_dbfs":  -18.0,
        "steps":       3,
    })
    assert ack["ok"] is True

    frames = recv_until(client, done_topics=("done", "error"), timeout_ms=30000)
    data_frames = [f for t, f in frames if t == "data"]
    assert len(data_frames) == 3

    done_frames = [f for t, f in frames if t == "done"]
    assert done_frames[0]["n_points"] == 3


# ---------------------------------------------------------------------------
# Sweep point frame: None fields should not break numpy float conversion
# ---------------------------------------------------------------------------

def test_sweep_point_none_fields_are_numeric_safe(server_client):
    """Without calibration, gain_db/out_dbu/in_dbu are None in sweep_point frames.
    UI code must handle these safely with np.nan, not None in float arrays."""
    import numpy as np
    client = server_client
    ack = client.send_cmd({
        "cmd":        "plot",
        "start_hz":   1000.0,
        "stop_hz":    2000.0,
        "level_dbfs": -20.0,
        "ppd":         1,
    })
    assert ack["ok"] is True

    frames = recv_until(client, done_topics=("done", "error"), timeout_ms=30000)
    data_frames = [f for t, f in frames
                   if t == "data" and f.get("type") in SWEEP_POINT_TYPES]
    assert data_frames

    # The test server has no calibration, so these fields should be None
    for f in data_frames:
        assert f["gain_db"] is None, "expected None gain_db without calibration"
        assert f["out_dbu"] is None, "expected None out_dbu without calibration"
        assert f["in_dbu"]  is None, "expected None in_dbu without calibration"

    # Verify the pattern used in sweep.py handles None → NaN correctly
    pts = data_frames
    gain = np.array([p["gain_db"] if p.get("gain_db") is not None
                     else np.nan for p in pts], dtype=float)
    assert gain.dtype == np.float64
    assert np.all(np.isnan(gain))

    # Verify the WRONG pattern would produce an object array (the bug we fixed)
    gain_broken = np.array([p.get("gain_db", np.nan) for p in pts])
    # p.get("gain_db", np.nan) returns None because the key exists with value None
    assert gain_broken.dtype == object, \
        "get() with default should return None when key exists — confirming the bug pattern"


def test_plot_thd_numerical_accuracy(server_client):
    """FakeJackEngine generates 1% 2nd harmonic — verify the server reports ≈1% THD."""
    client = server_client
    ack = client.send_cmd({
        "cmd":        "plot",
        "start_hz":   1000.0,
        "stop_hz":    1000.0,
        "level_dbfs": -20.0,
        "ppd":         1,
    })
    assert ack["ok"] is True

    frames = recv_until(client, done_topics=("done", "error"), timeout_ms=30000)
    data_frames = [f for t, f in frames
                   if t == "data" and f.get("type") in SWEEP_POINT_TYPES]
    assert data_frames, "expected at least one sweep_point"

    f = data_frames[0]
    # FakeJackEngine: amp=0.1, 2nd harmonic = 0.01*0.1 = 0.001 → THD = 1.0%
    assert 0.8 < f["thd_pct"] < 1.3, \
        f"THD should be ≈1.0% from FakeJackEngine, got {f['thd_pct']:.4f}%"
    # THD+N should be close to THD (no real noise in synthetic signal)
    assert f["thdn_pct"] >= f["thd_pct"], \
        f"THD+N ({f['thdn_pct']:.4f}%) < THD ({f['thd_pct']:.4f}%) — impossible"
    # fundamental_dbfs should be ≈ -20 dBFS (amplitude 0.1)
    assert -22 < f["fundamental_dbfs"] < -18, \
        f"fundamental_dbfs should be ≈-20, got {f['fundamental_dbfs']:.2f}"


def test_plot_level_thd_numerical_accuracy(server_client):
    """plot_level at fixed 1kHz should also produce ≈1% THD from FakeJackEngine."""
    client = server_client
    ack = client.send_cmd({
        "cmd":        "plot_level",
        "freq_hz":    1000.0,
        "start_dbfs": -20.0,
        "stop_dbfs":  -20.0,
        "steps":       1,
    })
    assert ack["ok"] is True

    frames = recv_until(client, done_topics=("done", "error"), timeout_ms=30000)
    data_frames = [f for t, f in frames if t == "data"]
    assert data_frames

    f = data_frames[0]
    assert 0.8 < f["thd_pct"] < 1.3, \
        f"THD should be ≈1.0%, got {f['thd_pct']:.4f}%"
    assert f["freq_hz"] == pytest.approx(1000.0)
    assert f["drive_db"] == pytest.approx(-20.0)


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
        if topic == "data" and frame.get("type") in SPECTRUM_TYPES:
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


# ---------------------------------------------------------------------------
# probe
# ---------------------------------------------------------------------------

def test_probe_ack(server_client):
    """probe returns ok=True with port counts immediately; emits data frames then done."""
    client = server_client
    ack = client.send_cmd({"cmd": "probe"})
    assert ack is not None
    assert ack["ok"] is True
    assert "n_playback" in ack
    assert "n_capture"  in ack
    assert isinstance(ack["n_playback"], int)
    assert isinstance(ack["n_capture"],  int)
    assert ack["n_playback"] >= 0
    assert ack["n_capture"]  >= 0

    # Collect a few frames to verify the worker is running, then stop it.
    # (Full scan over 20×20 fake ports takes ~40 s — not suitable for CI.)
    initial_frames = []
    for _ in range(10):
        try:
            topic, frame = client.recv_data(timeout_ms=3000)
        except TimeoutError:
            break
        initial_frames.append((topic, frame))
        if topic in ("done", "error"):
            break

    assert initial_frames, "probe emitted no frames"
    # Stop and drain
    _stop_and_drain(client)


# ---------------------------------------------------------------------------
# transfer
# ---------------------------------------------------------------------------

def _ensure_reference_channel(client, channel=1):
    """Set reference_channel so transfer/test_hardware/test_dut commands don't reject."""
    ack = client.send_cmd({"cmd": "setup", "update": {"reference_channel": channel}})
    assert ack is not None and ack["ok"] is True


# ---------------------------------------------------------------------------
# test_hardware
# ---------------------------------------------------------------------------

@pytest.mark.slow
def test_test_hardware_frames(server_client):
    """test_hardware emits test_result frames with required fields then done."""
    client = server_client
    _ensure_reference_channel(client, channel=1)

    ack = client.send_cmd({"cmd": "test_hardware"})
    assert ack is not None
    assert ack["ok"] is True, f"test_hardware rejected: {ack.get('error')}"
    assert "out_port" in ack
    assert "in_port"  in ack

    # test_hardware is slow in fake mode (~25 s); set a generous timeout
    frames = recv_until(client, done_topics=("done", "error"), timeout_ms=90000)

    data_frames = [f for t, f in frames if t == "data"]
    assert data_frames, "expected test_result frames from test_hardware"

    for f in data_frames:
        assert f.get("type")      == "test_result"
        assert f.get("cmd")       == "test_hardware"
        assert "name"             in f
        assert "pass"             in f
        assert "detail"           in f
        assert isinstance(f["pass"], bool)

    done_frames = [f for t, f in frames if t == "done"]
    assert done_frames, "no done frame from test_hardware"
    done = done_frames[0]
    assert done["cmd"]        == "test_hardware"
    assert "tests_run"        in done
    assert done["tests_run"]  >= 1


# ---------------------------------------------------------------------------
# test_dut
# ---------------------------------------------------------------------------

@pytest.mark.slow
def test_test_dut_frames(server_client):
    """test_dut emits test_result frames with required fields then done."""
    client = server_client
    _ensure_reference_channel(client, channel=1)

    ack = client.send_cmd({"cmd": "test_dut", "level_dbfs": -20.0})
    assert ack is not None
    assert ack["ok"] is True, f"test_dut rejected: {ack.get('error')}"
    assert "out_port" in ack
    assert "in_port"  in ack
    assert "ref_port" in ack

    # test_dut is slow in fake mode (~20 s); set a generous timeout
    frames = recv_until(client, done_topics=("done", "error"), timeout_ms=60000)

    data_frames = [f for t, f in frames if t == "data"]
    assert data_frames, "expected test_result frames from test_dut"

    for f in data_frames:
        assert f.get("type")  == "test_result"
        assert f.get("cmd")   == "test_dut"
        assert "name"         in f
        assert "pass"         in f
        assert isinstance(f["pass"], bool)

    done_frames = [f for t, f in frames if t == "done"]
    assert done_frames, "no done frame from test_dut"
    done = done_frames[0]
    assert done["cmd"]       == "test_dut"
    assert "tests_run"       in done
    assert done["tests_run"] >= 1
