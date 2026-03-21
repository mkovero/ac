"""Shared fixtures: FakeJackEngine, free ports, session-scoped ZMQ server + client."""
import socket
import threading
import time

import numpy as np
import pytest
from unittest.mock import patch

from ac.client.ac import AcClient
from ac.server.engine import run_server


# ---------------------------------------------------------------------------
# Fake JACK engine — no JACK daemon required
# ---------------------------------------------------------------------------

class FakeJackEngine:
    samplerate = 48000
    blocksize  = 1024
    xruns      = 0

    def __init__(self, client_name="ac"):
        self._freq = 1000.0
        self._amp  = 0.1

    def set_tone(self, f, a):
        self._freq = float(f)
        self._amp  = float(a)

    def set_silence(self):
        pass

    def start(self, output_ports=None, input_port=None):
        pass

    def stop(self):
        pass

    def capture_block(self, duration):
        # Small sleep to prevent spinning in monitor loops and to allow
        # stop_ev to be checked between iterations.
        time.sleep(0.01)
        n   = int(self.samplerate * duration)
        t   = np.arange(n) / self.samplerate
        sig = self._amp * np.sin(2 * np.pi * self._freq * t)
        # Add ~1 % 2nd harmonic so THD analysis returns a realistic value
        sig += 0.01 * self._amp * np.sin(2 * np.pi * self._freq * 2 * t)
        return sig.astype(np.float32)


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


_FAKE_PORTS = (
    [f"fake_playback_{i}" for i in range(20)],
    [f"fake_capture_{i}"  for i in range(20)],
)

# Config used by the test server — channels 0 and 0, guaranteed in range
_TEST_CFG = {
    "device":         0,
    "output_channel": 0,
    "input_channel":  0,
    "dbu_ref_vrms":   0.77459667,
    "dmm_host":       None,
}


# ---------------------------------------------------------------------------
# Session-scoped server + client fixture
# ---------------------------------------------------------------------------

@pytest.fixture(scope="session")
def server_client():
    """Start a real ZMQ server in a daemon thread (with FakeJackEngine) and
    return a connected AcClient.  Shared across the entire test session."""
    ctrl_port = _free_port()
    data_port = _free_port()

    with patch("ac.server.engine.JackEngine",   FakeJackEngine), \
         patch("ac.server.engine.find_ports",   return_value=_FAKE_PORTS), \
         patch("ac.server.engine.load_config",  return_value=dict(_TEST_CFG)), \
         patch("ac.server.engine.save_config",  side_effect=lambda u: {**_TEST_CFG, **u}):

        t = threading.Thread(
            target=run_server,
            kwargs=dict(ctrl_port=ctrl_port, data_port=data_port),
            daemon=True,
        )
        t.start()

        client = AcClient("localhost", ctrl_port, data_port)

        # Poll until server responds (allow up to 4 s to cover the 500 ms probe)
        for _ in range(40):
            ack = client.send_cmd({"cmd": "status"}, timeout_ms=200)
            if ack is not None:
                break
            time.sleep(0.1)
        else:
            pytest.fail("ZMQ test server did not start within 4 s")

        yield client

        client.close()
