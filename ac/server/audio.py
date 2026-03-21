# audio.py  -- JACK-based audio engine
import threading
import numpy as np
import jack

RINGBUFFER_SECONDS = 4


class JackEngine:
    def __init__(self, client_name="ac"):
        self._client      = jack.Client(client_name)
        self._sr          = self._client.samplerate
        self._blocksize   = self._client.blocksize

        # Output ports -- one per hardware channel we drive.
        # Populated lazily in start(); _out_ports is a list so the
        # process callback can write the same tone to all of them.
        self._out_ports   = []
        self._in_port     = self._client.inports.register("in_0")

        # Output tone -- one cycle-aligned buffer, looped on all outputs
        self._tone        = np.zeros(self._blocksize, dtype=np.float32)
        self._tone_pos    = 0
        self._tone_lock   = threading.Lock()

        # Input ringbuffer
        rb_frames         = int(self._sr * RINGBUFFER_SECONDS)
        self._ringbuf     = jack.RingBuffer(rb_frames * 4)
        self._capture_on  = False

        self.xruns        = 0

        self._client.set_process_callback(self._process)
        self._client.set_xrun_callback(self._xrun)
        self._client.set_shutdown_callback(self._shutdown)
        self._shutdown_event = threading.Event()

    # ------------------------------------------------------------------
    # Properties
    # ------------------------------------------------------------------

    @property
    def samplerate(self):
        return self._sr

    @property
    def blocksize(self):
        return self._blocksize

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    def start(self, output_ports=None, input_port=None):
        """Activate and connect to hardware ports.

        output_ports: str (single port name) or list of str.
        input_port:   str or None.
        """
        # Normalise to list
        if output_ports is None:
            out_list = []
        elif isinstance(output_ports, str):
            out_list = [output_ports]
        else:
            out_list = list(output_ports)

        # Register exactly as many JACK output ports as we need
        for i in range(len(self._out_ports), len(out_list)):
            self._out_ports.append(
                self._client.outports.register(f"out_{i}")
            )

        self._client.activate()

        for jack_port, hw_port in zip(self._out_ports, out_list):
            self._client.connect(jack_port, hw_port)
        if input_port:
            self._client.connect(input_port, self._in_port)

    # Keep backward-compatible single-port kwarg
    def start_mono(self, output_port=None, input_port=None):
        self.start(output_ports=output_port, input_port=input_port)

    def stop(self):
        try:
            self._client.deactivate()
            self._client.close()
        except Exception:
            pass

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.stop()

    # ------------------------------------------------------------------
    # Tone control
    # ------------------------------------------------------------------

    def set_tone(self, freq, amplitude, duration_seconds=None):
        n    = self._sr
        t    = np.arange(n) / self._sr
        tone = (amplitude * np.sin(2 * np.pi * freq * t)).astype(np.float32)
        with self._tone_lock:
            self._tone     = tone
            self._tone_pos = 0

    def set_pink_noise(self, amplitude):
        n     = 4 * self._sr   # 4-second buffer for stability; sub-20 Hz bins are zeroed below
        white = np.random.randn(n).astype(np.float32)
        fft   = np.fft.rfft(white)
        freqs = np.fft.rfftfreq(n, 1.0 / self._sr)
        freqs[0] = 1.0   # avoid division by zero at DC
        fft  /= np.sqrt(freqs)
        fft[freqs < 20.0] = 0.0  # band-limit to ≥20 Hz; matches DMM >20 Hz mode, RMS norm consistent with what DMM measures
        pink  = np.fft.irfft(fft, n=n).astype(np.float32)
        rms = float(np.sqrt(np.mean(pink ** 2)))
        if rms > 0:
            pink *= amplitude / (rms * np.sqrt(2))  # amplitude is peak-referenced (sine peak = amplitude); pink RMS = amplitude/√2
        with self._tone_lock:
            self._tone     = pink
            self._tone_pos = 0

    def set_silence(self):
        with self._tone_lock:
            self._tone     = np.zeros(self._blocksize, dtype=np.float32)
            self._tone_pos = 0

    # ------------------------------------------------------------------
    # Input capture
    # ------------------------------------------------------------------

    def start_capture(self):
        self._ringbuf.read(self._ringbuf.read_space)
        self._capture_on = True

    def stop_capture(self):
        self._capture_on = False

    def read_capture(self, n_frames):
        n_bytes = n_frames * 4
        while self._ringbuf.read_space < n_bytes:
            threading.Event().wait(0.005)
        raw = self._ringbuf.read(n_bytes)
        return np.frombuffer(raw, dtype=np.float32).copy()

    def capture_block(self, duration_seconds):
        n = int(duration_seconds * self._sr)
        self.start_capture()
        data = self.read_capture(n)
        self.stop_capture()
        return data

    # ------------------------------------------------------------------
    # JACK process callback
    # ------------------------------------------------------------------

    def _process(self, frames):
        # Output -- write same tone to every registered output port
        with self._tone_lock:
            tone = self._tone
            pos  = self._tone_pos
        tlen    = len(tone)
        written = 0
        # build one block first, then copy to all ports
        block = np.empty(frames, dtype=np.float32)
        while written < frames:
            chunk = min(frames - written, tlen - pos)
            block[written:written + chunk] = tone[pos:pos + chunk]
            written += chunk
            pos = (pos + chunk) % tlen
        with self._tone_lock:
            self._tone_pos = pos
        for port in self._out_ports:
            port.get_array()[:] = block

        # Input
        if self._capture_on:
            data    = self._in_port.get_array().astype(np.float32)
            n_bytes = data.nbytes
            if self._ringbuf.write_space >= n_bytes:
                self._ringbuf.write(data.tobytes())

    def _xrun(self, delay):
        self.xruns += 1

    def _shutdown(self, status, reason):
        self._shutdown_event.set()


# ------------------------------------------------------------------
# Port discovery helpers
# ------------------------------------------------------------------

def find_ports(client_name="ac-probe"):
    tmp      = jack.Client(client_name)
    playback = [p.name for p in tmp.get_ports(is_audio=True, is_input=True,  is_physical=True)]
    capture  = [p.name for p in tmp.get_ports(is_audio=True, is_output=True, is_physical=True)]
    tmp.close()
    return playback, capture


def port_name(ports, channel_index):
    if channel_index >= len(ports):
        raise ValueError(
            f"Channel {channel_index} out of range -- "
            f"only {len(ports)} ports available: {ports}"
        )
    return ports[channel_index]


def resolve_port(ports, sticky_name, fallback_index):
    """Return port name: by sticky name if present and found, else by index."""
    if sticky_name and sticky_name in ports:
        return sticky_name
    return port_name(ports, fallback_index)


# ------------------------------------------------------------------
# Backend detection + factory
# ------------------------------------------------------------------

_cached_backend = None


def _detect_backend():
    """Return 'jack' or 'sounddevice'. Checks config first, then probes."""
    global _cached_backend
    if _cached_backend is not None:
        return _cached_backend
    from ..config import load as load_config
    forced = load_config().get("backend")
    if forced in ("jack", "sounddevice"):
        _cached_backend = forced
        return forced
    try:
        import jack
        c = jack.Client("ac-probe-detect")
        c.close()
        _cached_backend = "jack"
    except Exception:
        _cached_backend = "sounddevice"
    return _cached_backend


def get_engine_class(backend=None):
    """Return the engine class for the given (or auto-detected) backend."""
    if backend is None:
        backend = _detect_backend()
    if backend == "jack":
        return JackEngine
    from .sd_audio import SoundDeviceEngine
    return SoundDeviceEngine


def get_port_helpers(backend=None):
    """Return (find_ports, port_name, resolve_port) for the given backend."""
    if backend is None:
        backend = _detect_backend()
    if backend == "jack":
        return find_ports, port_name, resolve_port
    from . import sd_audio
    return sd_audio.find_ports, sd_audio.port_name, sd_audio.resolve_port
