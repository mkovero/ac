"""Broadband RMS and band levels of one channel of a PCM WAV.

    python3 bands.py <file.wav> <channel-index>

Pure standard library — rigs have no numpy. RBJ band-pass per band:
Q 1.41 (about an octave) at the standard centres, Q 4 at 50/100/150 Hz so
mains hum stands out against its neighbours if present.
"""

import math
import sys
import wave

path, ch = sys.argv[1], int(sys.argv[2])
w = wave.open(path)
nch, sw, fr = w.getnchannels(), w.getsampwidth(), w.getframerate()
raw = w.readframes(w.getnframes())
n = len(raw) // (nch * sw)
full = float(1 << (8 * sw - 1))
skip = int(0.2 * fr)
x = [
    int.from_bytes(raw[(i * nch + ch) * sw:(i * nch + ch) * sw + sw], "little", signed=True) / full
    for i in range(skip, n)
]


def db(v):
    return 20 * math.log10(v) if v > 0 else float("-inf")


print(f"channel {ch}  {fr} Hz  {len(x) / fr:.1f} s analysed")
print(f"broadband rms {db(math.sqrt(sum(v * v for v in x) / len(x))):7.1f} dBFS")
for fc in (20, 31.5, 50, 63, 100, 125, 150, 250, 500, 1000, 2000, 4000, 8000, 16000):
    if fc >= fr / 2:
        continue
    q = 4.0 if fc in (50, 100, 150) else 1.41
    w0 = 2 * math.pi * fc / fr
    alpha = math.sin(w0) / (2 * q)
    a0 = 1 + alpha
    b0, b2 = alpha / a0, -alpha / a0
    a1, a2 = -2 * math.cos(w0) / a0, (1 - alpha) / a0
    x1 = x2 = y1 = y2 = 0.0
    s, m = 0.0, 0
    settle = fr // 10
    for i, v in enumerate(x):
        y = b0 * v + b2 * x2 - a1 * y1 - a2 * y2
        x2, x1 = x1, v
        y2, y1 = y1, y
        if i > settle:
            s += y * y
            m += 1
    print(f"  {fc:>7} Hz  Q{q:<4} {db(math.sqrt(s / m)):7.1f} dBFS")
