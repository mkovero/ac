# dmm.py  -- SCPI socket client for Keysight 34461A (and compatible DMMs)
import socket

DEFAULT_PORT = 5025


def _query(sock, cmd, timeout=8.0):
    sock.settimeout(timeout)
    sock.sendall((cmd.strip() + "\n").encode())
    buf = b""
    while True:
        chunk = sock.recv(4096)
        if not chunk:
            break
        buf += chunk
        if buf.endswith(b"\n"):
            break
    return buf.decode().strip()


def identify(host, port=DEFAULT_PORT, timeout=3.0):
    with socket.create_connection((host, port), timeout=timeout) as sock:
        return _query(sock, "*IDN?")


def read_ac_vrms(host, port=DEFAULT_PORT, timeout=10.0):
    """
    Configure the DMM for AC voltage and return one reading in Vrms.
    Uses default resolution (5.5 digits) which is fast (~100 ms) and
    more than sufficient for calibration (< 0.01% error).
    """
    with socket.create_connection((host, port), timeout=timeout) as sock:
        result = _query(sock, "MEAS:VOLT:AC?", timeout=timeout)
    return float(result)
