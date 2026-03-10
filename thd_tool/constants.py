# constants.py
SAMPLERATE      = 48000
DURATION        = 1.0
LIVE_DURATION   = 1.0
FUNDAMENTAL_HZ  = 1000
NUM_HARMONICS   = 10
WARMUP_REPS     = 2
FFT_WINDOW      = "hann"
DBU_REF_VRMS    = 0.7746

# Fireface 400 JACK device indices (from --list-devices)
DEFAULT_INPUT   = 2   # FFADO Source
DEFAULT_OUTPUT  = 3   # FFADO Sink
