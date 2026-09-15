"""Regression test for chan_levels.py's Goertzel tone level. No rig needed:
a synthetic 24-bit WAV at a known dBFS, piped through the real script.

    python3 scripts/rig/lib/chan_levels_test.py
"""

import math
import os
import subprocess
import sys
import tempfile
import wave

HERE = os.path.dirname(os.path.abspath(__file__))

fr, dur, f0, level_dbfs = 48000, 2.0, 1000.0, -20.0
amp = 10 ** (level_dbfs / 20) * (1 << 23)
n = int(fr * dur)

with tempfile.NamedTemporaryFile(suffix=".wav") as f:
    w = wave.open(f.name, "wb")
    w.setnchannels(1)
    w.setsampwidth(3)
    w.setframerate(fr)
    w.writeframes(
        b"".join(
            int(amp * math.sin(2 * math.pi * f0 * i / fr)).to_bytes(3, "little", signed=True)
            for i in range(n)
        )
    )
    w.close()
    out = subprocess.run(
        [sys.executable, os.path.join(HERE, "chan_levels.py"), f.name, str(f0)],
        capture_output=True,
        text=True,
        check=True,
    ).stdout

reported = float(out.strip().splitlines()[-1].split("|")[4])
assert abs(reported - level_dbfs) < 0.05, f"got {reported}, want {level_dbfs}"
print(f"chan_levels.py: tone level {reported:.2f} dBFS within 0.05 dB of the synthetic {level_dbfs:g} dBFS input")
