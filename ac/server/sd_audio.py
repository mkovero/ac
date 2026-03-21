# sd_audio.py  -- sounddevice (PortAudio) audio engine
# Drop-in replacement for JackEngine when JACK is unavailable.
import collections
import threading
import numpy as np
import sounddevice as sd

RINGBUFFER_SECONDS = 4
SAMPLERATE = 48000
BLOCKSIZE = 1024


class SoundDeviceEngine:
    def __init__(self, client_name="ac"):
        self._sr = SAMPLERATE
        self._blocksize = BLOCKSIZE
        self._stream = None

        # Output tone -- one cycle-aligned buffer, looped on all outputs
        self._tone = np.zeros(self._blocksize, dtype=np.float32)
        self._tone_pos = 0
        self._tone_lock = threading.Lock()

        # Input ringbuffer (deque of float32 arrays)
        self._capture_buf = collections.deque()
        self._capture_on = False
        self._capture_lock = threading.Lock()

        self.xruns = 0

        # Resolved in start()
        self._out_device = None
        self._in_device = None
        self._out_channels = 1
        self._in_channels = 1
        self._out_channel_map = []   # list of 0-based channel indices
        self._in_channel_idx = 0

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
        """Open and start the sounddevice stream.

        output_ports: str or list of "<device_name>:<channel>" strings, or None.
        input_port:   str "<device_name>:<channel>" or None.
        """
        if output_ports is None:
            out_list = []
        elif isinstance(output_ports, str):
            out_list = [output_ports]
        else:
            out_list = list(output_ports)

        out_dev = None
        in_dev = None
        out_max_ch = 0
        in_ch_idx = 0

        if out_list:
            parsed = [_parse_port(p) for p in out_list]
            out_dev = parsed[0][0]
            self._out_channel_map = [p[1] for p in parsed]
            out_max_ch = max(self._out_channel_map) + 1
        if input_port:
            in_dev, in_ch_idx = _parse_port(input_port)
            self._in_channel_idx = in_ch_idx

        self._out_device = out_dev
        self._in_device = in_dev

        has_output = bool(out_list)
        has_input = input_port is not None

        if has_output:
            self._out_channels = out_max_ch
        if has_input:
            self._in_channels = in_ch_idx + 1

        # Validate samplerate support
        device_pair = (
            out_dev if has_output else None,
            in_dev if has_input else None,
        )

        try:
            self._stream = sd.Stream(
                samplerate=self._sr,
                blocksize=self._blocksize,
                device=device_pair if (has_output and has_input) else (out_dev if has_output else in_dev),
                channels=(self._out_channels if has_output else None,
                          self._in_channels if has_input else None)
                         if (has_output and has_input) else None,
                dtype='float32',
                callback=self._callback,
            )
        except sd.PortAudioError as e:
            msg = str(e)
            if "Invalid sample rate" in msg or "sample rate" in msg.lower():
                raise RuntimeError(
                    f"Device does not support {self._sr} Hz sample rate. "
                    f"Check your audio device settings."
                ) from e
            raise

        self._stream.start()

    def stop(self):
        try:
            if self._stream is not None:
                self._stream.stop()
                self._stream.close()
                self._stream = None
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
        n = self._sr
        t = np.arange(n) / self._sr
        tone = (amplitude * np.sin(2 * np.pi * freq * t)).astype(np.float32)
        with self._tone_lock:
            self._tone = tone
            self._tone_pos = 0

    def set_pink_noise(self, amplitude):
        n = 4 * self._sr
        white = np.random.randn(n).astype(np.float32)
        fft = np.fft.rfft(white)
        freqs = np.fft.rfftfreq(n, 1.0 / self._sr)
        freqs[0] = 1.0
        fft /= np.sqrt(freqs)
        fft[freqs < 20.0] = 0.0
        pink = np.fft.irfft(fft, n=n).astype(np.float32)
        rms = float(np.sqrt(np.mean(pink ** 2)))
        if rms > 0:
            pink *= amplitude / (rms * np.sqrt(2))
        with self._tone_lock:
            self._tone = pink
            self._tone_pos = 0

    def set_silence(self):
        with self._tone_lock:
            self._tone = np.zeros(self._blocksize, dtype=np.float32)
            self._tone_pos = 0

    # ------------------------------------------------------------------
    # Input capture
    # ------------------------------------------------------------------

    def start_capture(self):
        with self._capture_lock:
            self._capture_buf.clear()
        self._capture_on = True

    def stop_capture(self):
        self._capture_on = False

    def read_capture(self, n_frames):
        collected = 0
        chunks = []
        while collected < n_frames:
            with self._capture_lock:
                if self._capture_buf:
                    chunk = self._capture_buf.popleft()
                    chunks.append(chunk)
                    collected += len(chunk)
                    continue
            threading.Event().wait(0.005)
        result = np.concatenate(chunks)
        return result[:n_frames]

    def capture_block(self, duration_seconds):
        n = int(duration_seconds * self._sr)
        self.start_capture()
        data = self.read_capture(n)
        self.stop_capture()
        return data

    # ------------------------------------------------------------------
    # Stream callback
    # ------------------------------------------------------------------

    def _callback(self, indata, outdata, frames, time_info, status):
        if status.output_underflow or status.input_overflow:
            self.xruns += 1

        # Output
        if outdata is not None:
            with self._tone_lock:
                tone = self._tone
                pos = self._tone_pos
            tlen = len(tone)
            written = 0
            block = np.empty(frames, dtype=np.float32)
            while written < frames:
                chunk = min(frames - written, tlen - pos)
                block[written:written + chunk] = tone[pos:pos + chunk]
                written += chunk
                pos = (pos + chunk) % tlen
            with self._tone_lock:
                self._tone_pos = pos
            outdata[:] = 0.0
            for ch in self._out_channel_map:
                if ch < outdata.shape[1]:
                    outdata[:, ch] = block

        # Input
        if indata is not None and self._capture_on:
            mono = indata[:, self._in_channel_idx].copy()
            with self._capture_lock:
                self._capture_buf.append(mono)


# ------------------------------------------------------------------
# Port discovery helpers
# ------------------------------------------------------------------

def _parse_port(port_str):
    """Parse '<device_name>:<channel>' -> (device_index_or_name, channel_int).

    Also accepts plain integer strings as device index with channel 0.
    """
    if ":" in port_str:
        dev_part, ch_part = port_str.rsplit(":", 1)
        try:
            ch = int(ch_part)
        except ValueError:
            raise ValueError(f"Invalid channel in port string: {port_str!r}")
        # Try to resolve device name to index
        dev_idx = _resolve_device_name(dev_part)
        return dev_idx, ch
    # Plain index
    try:
        return int(port_str), 0
    except ValueError:
        raise ValueError(f"Cannot parse port string: {port_str!r}")


def _resolve_device_name(name):
    """Resolve a device name (or numeric string) to a device index."""
    try:
        return int(name)
    except ValueError:
        pass
    devices = sd.query_devices()
    for i, d in enumerate(devices):
        if d['name'] == name:
            return i
    # Substring match
    for i, d in enumerate(devices):
        if name in d['name']:
            return i
    raise ValueError(f"No sounddevice matching {name!r}")


def find_ports(client_name="ac-probe"):
    """Return (playback, capture) lists of port strings.

    Each port string is '<device_name>:<channel_index>'.
    """
    devices = sd.query_devices()
    playback = []
    capture = []
    for i, d in enumerate(devices):
        name = d['name']
        for ch in range(d['max_output_channels']):
            playback.append(f"{name}:{ch}")
        for ch in range(d['max_input_channels']):
            capture.append(f"{name}:{ch}")
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
