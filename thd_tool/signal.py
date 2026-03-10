# signal.py
import numpy as np
from .constants import SAMPLERATE, DURATION, FUNDAMENTAL_HZ

def make_sine(amplitude, freq=FUNDAMENTAL_HZ, duration=DURATION, sr=SAMPLERATE):
    t    = np.linspace(0, duration, int(sr * duration), endpoint=False)
    mono = (np.sin(2.0 * np.pi * freq * t) * amplitude).astype(np.float32)
    return mono[:, None]

