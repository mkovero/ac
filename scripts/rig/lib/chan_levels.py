"""Per-channel RMS, peak and single-tone level of a multichannel PCM WAV.

    python3 chan_levels.py <file.wav> [tone-Hz]   (default 1000)

Pure standard library — rigs have no numpy. The tone level is a Goertzel
amplitude at exactly `tone-Hz`, reported as a sine amplitude in dBFS; far
more processing gain than a broadband RMS, so a -60 dBFS probe tone reads
cleanly against a room-noise floor. The first 0.2 s is skipped.
"""

import math
import sys
import wave

w = wave.open(sys.argv[1])
f0 = float(sys.argv[2]) if len(sys.argv) > 2 else 1000.0
nch, sw, fr = w.getnchannels(), w.getsampwidth(), w.getframerate()
raw = w.readframes(w.getnframes())
frames = len(raw) // (nch * sw)
skip = int(0.2 * fr)
full = float(1 << (8 * sw - 1))
k = 2 * math.cos(2 * math.pi * f0 / fr)


def db(v):
    return 20 * math.log10(v) if v > 0 else float("-inf")


print(f"{nch} ch  {sw * 8} bit  {fr} Hz  {frames} frames  tone {f0:g} Hz")
print("| channel | rms dBFS | peak dBFS | tone dBFS |")
print("|---|---|---|---|")
for c in range(nch):
    s2 = 0.0
    pk = 0
    q1 = q2 = 0.0
    m = 0
    for i in range(skip, frames):
        o = (i * nch + c) * sw
        v = int.from_bytes(raw[o:o + sw], "little", signed=True)
        s2 += v * v
        pk = max(pk, abs(v))
        q0 = k * q1 - q2 + v
        q2, q1 = q1, q0
        m += 1
    rms = math.sqrt(s2 / m) / full
    amp = 2 * math.sqrt(max(q1 * q1 + q2 * q2 - k * q1 * q2, 0.0)) / m / full
    print(f"| {c + 1} | {db(rms):.1f} | {db(pk / full):.1f} | {db(amp):.1f} |")
