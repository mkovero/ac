"""Locate the analog capture block in a silent multichannel capture.

    python3 port_order.py <file.wav> <expected-first> <analog-count>

The FF400's JACK channel order under snd_fireface is not stable: the ADAT
(and S/PDIF) block sometimes precedes the analog block and sometimes follows
it. With nothing connected to ADAT/S/PDIF, those captures read exact digital
zero, while an analog input never does (even a −110 dBFS floor is tens of
LSB at 24-bit). So the live channels show where the analog block sits.

Exit 0 when channels <expected-first> .. +<analog-count>-1 (1-based, JACK
`system:capture_N` numbering) are live and every other channel is exact zero.
Exit 1 otherwise, saying where the live channels actually are. Emits nothing
itself — the capture it reads must have been silent.
"""

import sys
import wave

path, first, count = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
w = wave.open(path)
nch, sw = w.getnchannels(), w.getsampwidth()
raw = w.readframes(w.getnframes())
frames = len(raw) // (nch * sw)
live = []
for c in range(nch):
    nonzero = any(
        raw[(i * nch + c) * sw:(i * nch + c) * sw + sw] != b"\x00" * sw for i in range(frames)
    )
    if nonzero:
        live.append(c + 1)

expected = list(range(first, first + count))


def spans(chs):
    out, start = [], None
    for i, c in enumerate(chs):
        if start is None:
            start = c
        if i + 1 == len(chs) or chs[i + 1] != c + 1:
            out.append(f"{start}" if start == c else f"{start}-{c}")
            start = None
    return ",".join(out) or "none"


print(f"captures: {nch}  live (non-zero): {spans(live)}  expected analog block: {spans(expected)}")
if live == expected:
    print("port order: matches the profile")
    sys.exit(0)
missing = sorted(set(expected) - set(live))
extra = sorted(set(live) - set(expected))
if missing and extra and len(live) == count:
    print(f"port order: MOVED — the analog block is at capture {spans(live)}, not {spans(expected)}; "
          "the profile's port names and ac indices point at ADAT/S/PDIF")
else:
    if missing:
        print(f"port order: expected-analog captures reading exact zero: {spans(missing)} "
              "(block moved, or an input is dead)")
    if extra:
        print(f"port order: live captures outside the expected block: {spans(extra)} "
              "(block moved, or something is connected to ADAT/S/PDIF)")
sys.exit(1)
