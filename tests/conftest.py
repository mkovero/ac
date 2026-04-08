"""Shared fixtures: session-scoped Rust ac-daemon (--fake-audio) + AcClient."""
import os
import socket
import subprocess
import sys
import time

import pytest

from ac.client.ac import AcClient


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
    import shutil
    daemon = shutil.which("ac-daemon")
    if daemon:
        return daemon
    here = os.path.dirname(os.path.abspath(__file__))
    dev  = os.path.normpath(os.path.join(here, "..", "ac-rs", "target", "debug", "ac-daemon"))
    if os.path.isfile(dev) and os.access(dev, os.X_OK):
        return dev
    return None


# ---------------------------------------------------------------------------
# Session-scoped server + client fixture
# ---------------------------------------------------------------------------

@pytest.fixture(scope="session")
def server_client():
    """Start ac-daemon --fake-audio on free ports and return a connected AcClient.
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

    # Poll until daemon is ready (up to 5 s)
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
