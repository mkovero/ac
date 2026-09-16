"""Regression test for port_order.py's analog-block check. No rig needed:
synthetic silent 24-bit multichannel WAVs (a low-level non-zero floor on the
live channels, exact digital zero elsewhere), piped through the real script.

    python3 scripts/rig/lib/port_order_test.py

Replaces the rig item "exercise the port-order gate on a genuinely ADAT-first
boot" (PR #441): what that boot changes is which channels read exact zero,
and that is what these files set.
"""

import os
import random
import subprocess
import sys
import tempfile
import wave

HERE = os.path.dirname(os.path.abspath(__file__))
SECONDS = 1.0  # rig_port_order records 1 s


def run(nch, fr, live, first=1, count=8, stray=None):
    """Write an nch-channel capture where the 1-based channels in `live`
    carry a floor of a few LSB and every other channel is exact zero, except
    `stray` (a 1-based channel) which gets one 1-LSB sample near the end.
    Returns (exit status, stdout) of port_order.py <file> <first> <count>."""
    rng = random.Random(nch * 1000 + fr)
    n = int(fr * SECONDS)
    zero = b"\x00\x00\x00"
    frames = bytearray()
    for i in range(n):
        for c in range(1, nch + 1):
            if c in live:
                # A −110 dBFS floor is ~26 LSB RMS at 24-bit; a few LSB is
                # lower still, and some samples are exactly zero.
                frames += rng.randint(-4, 4).to_bytes(3, "little", signed=True)
            elif c == stray and i == n - 10:
                frames += (1).to_bytes(3, "little", signed=True)
            else:
                frames += zero
    with tempfile.NamedTemporaryFile(suffix=".wav") as f:
        w = wave.open(f.name, "wb")
        w.setnchannels(nch)
        w.setsampwidth(3)
        w.setframerate(fr)
        w.writeframes(bytes(frames))
        w.close()
        p = subprocess.run(
            [sys.executable, os.path.join(HERE, "port_order.py"), f.name, str(first), str(count)],
            capture_output=True,
            text=True,
        )
    return p.returncode, p.stdout


def span(a, b):
    return set(range(a, b + 1))


# (a) 14 captures at 96 kHz, analog-first: live 1-8, 9-14 exact zero.
rc, out = run(14, 96000, span(1, 8))
assert rc == 0, f"(a) analog-first 14 ch: want exit 0, got {rc}\n{out}"

# (b) ADAT-first: live 7-14, 1-6 exact zero. The message must say where the
# live block actually is.
rc, out = run(14, 96000, span(7, 14))
assert rc == 1, f"(b) ADAT-first 14 ch: want exit 1, got {rc}\n{out}"
assert "analog block is at capture 7-14" in out, f"(b) message does not name the live block 7-14:\n{out}"

# (c) 18 captures at 48 kHz, analog-first.
rc, out = run(18, 48000, span(1, 8))
assert rc == 0, f"(c) analog-first 18 ch: want exit 0, got {rc}\n{out}"

# (d) one expected analog channel exact zero (a dead input).
rc, out = run(18, 48000, span(1, 8) - {3})
assert rc == 1, f"(d) dead analog input: want exit 1, got {rc}\n{out}"
assert "exact zero: 3 " in out, f"(d) message does not name capture 3:\n{out}"

# (e) every expected analog channel live, and one ADAT channel not exact zero
# by a single LSB. (a)-(d) cannot tell whether port_order.py still requires
# the non-analog channels to be exact zero — (b) and (d) fail on a missing
# analog channel either way — so this case is the one that does.
rc, out = run(18, 48000, span(1, 8), stray=12)
assert rc == 1, f"(e) live ADAT channel: want exit 1, got {rc}\n{out}"
assert "outside the expected block: 12 " in out, f"(e) message does not name capture 12:\n{out}"

print("port_order.py: analog-first 14/18 ch pass; ADAT-first, dead input and 1-LSB ADAT channel refused")
