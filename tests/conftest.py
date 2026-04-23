"""Shared fixtures: session-scoped Rust ac-daemon (--fake-audio) + minimal
pyzmq client.

The client is defined inline here on purpose — `tests/` is a black-box
protocol harness against the Rust daemon and deliberately has no Python
runtime to depend on. See ac-rs/ZMQ.md for the authoritative wire format.
"""
import json
import os
import shutil
import socket
import subprocess
import time

import pytest
import zmq


# ---------------------------------------------------------------------------
# Marker: slow tests (`test_hardware`, `test_dut` drive the full battery
# against the fake engine and dominate wall-clock — ~2 min each). Skipped
# by default; opt in with `pytest --runslow` or `pytest -m slow`.
# ---------------------------------------------------------------------------

def pytest_addoption(parser):
    parser.addoption(
        "--runslow", action="store_true", default=False,
        help="run tests marked `slow` (extended suite: multi-minute)",
    )


def pytest_configure(config):
    config.addinivalue_line(
        "markers", "slow: long-running test; skipped unless --runslow or -m slow"
    )


def pytest_collection_modifyitems(config, items):
    if config.getoption("--runslow"):
        return
    # If the user explicitly selected slow tests via `-m`, respect that.
    if "slow" in (config.getoption("-m") or ""):
        return
    skip_slow = pytest.mark.skip(reason="slow (pass --runslow to include)")
    for item in items:
        if "slow" in item.keywords:
            item.add_marker(skip_slow)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _free_port():
    """Return an unused TCP port on localhost."""
    s = socket.socket()
    s.bind(("", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def _find_daemon():
    """Locate the ac-daemon binary: $PATH first, then the local dev build."""
    daemon = shutil.which("ac-daemon")
    if daemon:
        return daemon
    here = os.path.dirname(os.path.abspath(__file__))
    dev = os.path.normpath(os.path.join(here, "..", "ac-rs", "target", "debug", "ac-daemon"))
    if os.path.isfile(dev) and os.access(dev, os.X_OK):
        return dev
    return None


# ---------------------------------------------------------------------------
# Minimal ZMQ client (JSON REQ on CTRL, JSON SUB with `<topic> <json>\n` on DATA)
# ---------------------------------------------------------------------------

class AcClient:
    def __init__(self, host, ctrl_port, data_port):
        self._ctx = zmq.Context.instance()
        self._host = host
        self._ctrl_port = ctrl_port
        self._ctrl = None
        self._connect_ctrl()

        self._data = self._ctx.socket(zmq.SUB)
        self._data.setsockopt(zmq.LINGER, 0)
        self._data.setsockopt(zmq.SUBSCRIBE, b"")
        self._data.connect(f"tcp://{host}:{data_port}")
        time.sleep(0.05)  # PUB/SUB slow-join

    def _connect_ctrl(self):
        if self._ctrl is not None:
            self._ctrl.close(0)
        self._ctrl = self._ctx.socket(zmq.REQ)
        self._ctrl.setsockopt(zmq.LINGER, 0)
        self._ctrl.connect(f"tcp://{self._host}:{self._ctrl_port}")

    def send_cmd(self, cmd, timeout_ms=5000):
        self._ctrl.setsockopt(zmq.RCVTIMEO, int(timeout_ms))
        self._ctrl.send_string(json.dumps(cmd))
        try:
            return json.loads(self._ctrl.recv_string())
        except zmq.Again:
            # REQ is stuck in RECV state; reset the socket for the next call.
            self._connect_ctrl()
            return None

    def recv_data(self, timeout_ms=30000):
        if self._data.poll(int(timeout_ms)) == 0:
            raise TimeoutError
        raw = self._data.recv_string()
        sp = raw.find(" ")
        topic = raw[:sp]
        frame = json.loads(raw[sp + 1:])
        return topic, frame

    def close(self):
        self._ctrl.close(0)
        self._data.close(0)


# ---------------------------------------------------------------------------
# Session-scoped server + client fixture
# ---------------------------------------------------------------------------

@pytest.fixture(scope="session")
def server_client():
    """Start ac-daemon --fake-audio on free ports and return a connected client.
    Shared across the entire test session."""
    daemon = _find_daemon()
    if daemon is None:
        pytest.skip("ac-daemon binary not found — run `cd ac-rs && cargo build -p ac-daemon`")

    ctrl_port = _free_port()
    data_port = _free_port()

    proc = subprocess.Popen(
        [daemon, "--local", "--fake-audio",
         "--ctrl-port", str(ctrl_port),
         "--data-port", str(data_port)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    client = AcClient("localhost", ctrl_port, data_port)

    for _ in range(50):
        ack = client.send_cmd({"cmd": "status"}, timeout_ms=200)
        if ack is not None:
            break
        time.sleep(0.1)
    else:
        proc.terminate()
        pytest.fail("ac-daemon did not start within 5 s")

    yield client

    client.close()
    proc.terminate()
    try:
        proc.wait(timeout=3)
    except subprocess.TimeoutExpired:
        proc.kill()
