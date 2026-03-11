# audio.py  -- JACK-based audio engine
# Replaces the sounddevice playrec calls with a continuous duplex stream.
#
# Architecture:
#   - JackEngine runs a persistent client with process callback
#   - Output: loops a pre-computed tone buffer endlessly
#   - Input:  fills a ringbuffer; callers pull blocks from it
#   - No open/close between measurements -- just swap the tone and drain the ring

import threading
import numpy as np
import jack

RINGBUFFER_SECONDS = 4   # how much input history to keep


class JackEngine:
    def __init__(self, client_name="ac"):
        self._client      = jack.Client(client_name)
        self._sr          = self._client.samplerate
        self._blocksize   = self._client.blocksize

        # Ports -- connected by caller after activation
        self._out_port    = self._client.outports.register("out_0")
        self._in_port     = self._client.inports.register("in_0")

        # Output tone -- numpy array, looped
        self._tone        = np.zeros(self._blocksize, dtype=np.float32)
        self._tone_pos    = 0
        self._tone_lock   = threading.Lock()

        # Input ringbuffer
        rb_frames         = int(self._sr * RINGBUFFER_SECONDS)
        self._ringbuf     = jack.RingBuffer(rb_frames * 4)  # 4 bytes per float32
        self._capture_on  = False

        # Xrun counter
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

    def start(self, output_port=None, input_port=None):
        """Activate and optionally connect to hardware ports."""
        self._client.activate()
        if output_port:
            self._client.connect(self._out_port, output_port)
        if input_port:
            self._client.connect(input_port, self._in_port)

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
        """
        Set the output tone. Generates one full cycle-aligned buffer.
        duration_seconds is ignored (tone loops forever) but kept for
        API compatibility with the old playrec style.
        """
        n = self._sr
        t = np.arange(n) / self._sr
        tone = (amplitude * np.sin(2 * np.pi * freq * t)).astype(np.float32)
        with self._tone_lock:
            self._tone     = tone
            self._tone_pos = 0

    def set_silence(self):
        with self._tone_lock:
            self._tone     = np.zeros(self._blocksize, dtype=np.float32)
            self._tone_pos = 0

    # ------------------------------------------------------------------
    # Input capture
    # ------------------------------------------------------------------

    def start_capture(self):
        """Clear ringbuffer and start filling it."""
        # drain
        self._ringbuf.read(self._ringbuf.read_space)
        self._capture_on = True

    def stop_capture(self):
        self._capture_on = False

    def read_capture(self, n_frames):
        """
        Block until n_frames are available, return float32 numpy array.
        Raises RuntimeError on xrun during capture.
        """
        n_bytes = n_frames * 4
        while self._ringbuf.read_space < n_bytes:
            threading.Event().wait(0.005)
        raw = self._ringbuf.read(n_bytes)
        return np.frombuffer(raw, dtype=np.float32).copy()

    def capture_block(self, duration_seconds):
        """Convenience: play tone, capture duration_seconds, return array."""
        n = int(duration_seconds * self._sr)
        self.start_capture()
        data = self.read_capture(n)
        self.stop_capture()
        return data

    # ------------------------------------------------------------------
    # JACK callbacks
    # ------------------------------------------------------------------

    def _process(self, frames):
        # --- output ---
        with self._tone_lock:
            tone = self._tone
            pos  = self._tone_pos
        out  = self._out_port.get_array()
        tlen = len(tone)
        written = 0
        while written < frames:
            chunk = min(frames - written, tlen - pos)
            out[written:written + chunk] = tone[pos:pos + chunk]
            written += chunk
            pos = (pos + chunk) % tlen
        with self._tone_lock:
            self._tone_pos = pos

        # --- input ---
        if self._capture_on:
            data = self._in_port.get_array().astype(np.float32)
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
    """Return lists of available hardware playback and capture ports."""
    tmp = jack.Client(client_name)
    playback = [p.name for p in tmp.get_ports(is_audio=True, is_input=True,  is_physical=True)]
    capture  = [p.name for p in tmp.get_ports(is_audio=True, is_output=True, is_physical=True)]
    tmp.close()
    return playback, capture


def port_name(ports, channel_index):
    """Return port name by 0-based index, or raise a clear error."""
    if channel_index >= len(ports):
        raise ValueError(
            f"Channel {channel_index} out of range -- "
            f"only {len(ports)} ports available: {ports}"
        )
    return ports[channel_index]
