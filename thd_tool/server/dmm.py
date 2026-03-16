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


def read_ac_vrms(host, port=DEFAULT_PORT, timeout=10.0, n=3):
    """
    Configure the DMM for AC voltage, take n readings, return the average.
    Keeping the socket open across readings avoids reconnect overhead and
    gives the DMM multiple independent aperture windows for better accuracy.
    """
    with socket.create_connection((host, port), timeout=timeout) as sock:
        readings = [float(_query(sock, "MEAS:VOLT:AC?", timeout=timeout))
                    for _ in range(n)]
    return sum(readings) / len(readings)
