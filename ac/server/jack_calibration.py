# jack_calibration.py  -- calibration + Calibration class (JACK backend)
import json
import math
import os
import numpy as np
from .audio      import get_engine_class, get_port_helpers
JackEngine = get_engine_class()
find_ports, port_name, resolve_port = get_port_helpers()
from ..conversions import fmt_vrms, vrms_to_dbu, fmt_vpp, dbfs_to_vrms

DEFAULT_CAL_PATH = os.path.expanduser("~/.config/ac/cal.json")


# ---------------------------------------------------------------------------
# Calibration data class
# ---------------------------------------------------------------------------

def _cal_key(output_channel, input_channel):
    return f"out{output_channel}_in{input_channel}"


class Calibration:
    def __init__(self, output_channel=0, input_channel=0):
        self.output_channel    = output_channel
        self.input_channel     = input_channel
        self.ref_freq          = 1000.0   # freq used for the DMM/loopback measurement
        self.vrms_at_0dbfs_out = None
        self.vrms_at_0dbfs_in  = None
        self.ref_dbfs          = -10.0
        self.response_curve    = None     # list of (freq_hz, delta_db) or None

    @property
    def key(self):
        return _cal_key(self.output_channel, self.input_channel)

    @property
    def output_ok(self):
        return self.vrms_at_0dbfs_out is not None

    @property
    def input_ok(self):
        return self.vrms_at_0dbfs_in is not None

    def response_db(self, freq_hz):
        """Interpolate response curve at freq_hz. Returns delta_db (0.0 if no curve)."""
        if not self.response_curve:
            return 0.0
        freqs  = [f for f, d in self.response_curve]
        deltas = [d for f, d in self.response_curve]
        log_freq  = math.log10(max(freq_hz, 1.0))
        log_freqs = [math.log10(max(f, 1.0)) for f in freqs]
        if log_freq <= log_freqs[0]:
            return deltas[0]
        if log_freq >= log_freqs[-1]:
            return deltas[-1]
        for i in range(len(log_freqs) - 1):
            if log_freqs[i] <= log_freq <= log_freqs[i + 1]:
                t = (log_freq - log_freqs[i]) / (log_freqs[i + 1] - log_freqs[i])
                return deltas[i] + t * (deltas[i + 1] - deltas[i])
        return deltas[-1]

    def out_vrms(self, dbfs, freq_hz=None):
        if not self.output_ok:
            return None
        if freq_hz is not None:
            return dbfs_to_vrms(dbfs - self.response_db(freq_hz), self.vrms_at_0dbfs_out)
        return dbfs_to_vrms(dbfs, self.vrms_at_0dbfs_out)

    def in_vrms(self, linear_rms, freq_hz=None):
        if not self.input_ok:
            return None
        if freq_hz is not None:
            delta = self.response_db(freq_hz)
            return linear_rms * self.vrms_at_0dbfs_in / (10.0 ** (delta / 20.0))
        return linear_rms * self.vrms_at_0dbfs_in

    def save(self, path=None):
        path = path or DEFAULT_CAL_PATH
        os.makedirs(os.path.dirname(path), exist_ok=True)
        try:
            with open(path) as f:
                all_cals = json.load(f)
            stale = [k for k, v in all_cals.items() if not isinstance(v, dict)]
            for k in stale:
                del all_cals[k]
        except (FileNotFoundError, json.JSONDecodeError):
            all_cals = {}
        all_cals[self.key] = {
            "output_channel":    self.output_channel,
            "input_channel":     self.input_channel,
            "ref_freq":          self.ref_freq,
            "vrms_at_0dbfs_out": self.vrms_at_0dbfs_out,
            "vrms_at_0dbfs_in":  self.vrms_at_0dbfs_in,
            "ref_dbfs":          self.ref_dbfs,
            "response_curve":    self.response_curve,
        }
        with open(path, "w") as f:
            json.dump(all_cals, f, indent=2)
        print(f"  Calibration saved -> {path}  (key: {self.key})")

    @classmethod
    def load(cls, output_channel=0, input_channel=0, path=None):
        path = path or DEFAULT_CAL_PATH
        if not os.path.exists(path):
            return None
        try:
            with open(path) as f:
                all_cals = json.load(f)
        except (json.JSONDecodeError, ValueError):
            return None
        key = _cal_key(output_channel, input_channel)
        if key not in all_cals:
            return None
        data = all_cals[key]
        cal  = cls(output_channel=output_channel, input_channel=input_channel)
        cal.ref_freq          = data.get("ref_freq", 1000.0)
        cal.vrms_at_0dbfs_out = data.get("vrms_at_0dbfs_out")
        cal.vrms_at_0dbfs_in  = data.get("vrms_at_0dbfs_in")
        cal.ref_dbfs          = data.get("ref_dbfs", -10.0)
        rc = data.get("response_curve")
        if rc:
            cal.response_curve = [(float(f), float(d)) for f, d in rc]
        return cal

    @classmethod
    def load_output_only(cls, output_channel, path=None):
        """Load the first calibration that matches output_channel, any input channel."""
        path = path or DEFAULT_CAL_PATH
        if not os.path.exists(path):
            return None
        try:
            with open(path) as f:
                all_cals = json.load(f)
        except (json.JSONDecodeError, ValueError):
            return None
        prefix = f"out{output_channel}_in"
        for key, data in all_cals.items():
            if isinstance(data, dict) and key.startswith(prefix):
                in_ch = data.get("input_channel", 0)
                cal   = cls(output_channel=output_channel, input_channel=in_ch)
                cal.ref_freq          = data.get("ref_freq", 1000.0)
                cal.vrms_at_0dbfs_out = data.get("vrms_at_0dbfs_out")
                cal.vrms_at_0dbfs_in  = data.get("vrms_at_0dbfs_in")
                cal.ref_dbfs          = data.get("ref_dbfs", -10.0)
                rc = data.get("response_curve")
                if rc:
                    cal.response_curve = [(float(f), float(d)) for f, d in rc]
                return cal
        return None

    @classmethod
    def load_all(cls, path=None):
        """Return list of all stored Calibration objects."""
        path = path or DEFAULT_CAL_PATH
        if not os.path.exists(path):
            return []
        try:
            with open(path) as f:
                all_cals = json.load(f)
        except (json.JSONDecodeError, ValueError):
            return []
        result = []
        for key, data in all_cals.items():
            if not isinstance(data, dict):
                continue
            cal = cls(output_channel=data.get("output_channel", 0),
                      input_channel=data.get("input_channel",  0))
            cal.ref_freq          = data.get("ref_freq", 1000.0)
            cal.vrms_at_0dbfs_out = data.get("vrms_at_0dbfs_out")
            cal.vrms_at_0dbfs_in  = data.get("vrms_at_0dbfs_in")
            cal.ref_dbfs          = data.get("ref_dbfs", -10.0)
            rc = data.get("response_curve")
            if rc:
                cal.response_curve = [(float(f), float(d)) for f, d in rc]
            result.append(cal)
        return result

    def summary(self):
        print(f"\n  -- Calibration  [{self.key}] ----------------------------------")
        if self.output_ok:
            v = self.vrms_at_0dbfs_out
            print(f"  Output: 0 dBFS = {fmt_vrms(v)}"
                  f"  =  {vrms_to_dbu(v):+.2f} dBu"
                  f"  =  {fmt_vpp(v)}")
        else:
            print("  Output: not calibrated")
        if self.input_ok:
            v = self.vrms_at_0dbfs_in
            print(f"  Input:  0 dBFS = {fmt_vrms(v)}"
                  f"  =  {vrms_to_dbu(v):+.2f} dBu"
                  f"  =  {fmt_vpp(v)}")
        else:
            print("  Input:  not calibrated")
        if self.response_curve:
            deltas = [d for f, d in self.response_curve]
            print(f"  Response: {len(self.response_curve)} pts, "
                  f"{self.response_curve[0][0]:.0f}–{self.response_curve[-1][0]:.0f} Hz, "
                  f"±{max(abs(d) for d in deltas):.2f} dB")
        print("  --------------------------------------------------------------\n")


# ---------------------------------------------------------------------------
# DMM input helper
# ---------------------------------------------------------------------------

def _try_dmm_read(dmm_host):
    """Take 3 averaged AC Vrms readings from the DMM. Returns float or None."""
    try:
        from . import dmm as _dmm
        vrms = _dmm.read_ac_vrms(dmm_host, n=3)
        return vrms
    except Exception as e:
        print(f"  DMM read failed: {e}")
        return None


def _parse_dmm(prompt, dmm_host=None):
    suggestion = None
    if dmm_host:
        print(f"  Reading DMM ({dmm_host})...", end=" ", flush=True)
        suggestion = _try_dmm_read(dmm_host)
        if suggestion is not None:
            print(f"{fmt_vrms(suggestion)}  =  {vrms_to_dbu(suggestion):+.2f} dBu")
        else:
            print("(failed)")

    while True:
        if suggestion is not None:
            hint = f"{suggestion*1000:.4f} mVrms"
            raw  = input(f"  Enter to accept DMM reading ({hint}), or type override: ").strip().lower().replace(" ", "")
            if not raw:
                return suggestion
        else:
            raw = input(prompt).strip().lower().replace(" ", "")
            if not raw:
                return None
        try:
            if raw.endswith("mv") or raw.endswith("m"):
                return float(raw.rstrip("mv").rstrip("m")) / 1000.0
            elif raw.endswith("v"):
                return float(raw.rstrip("v"))
            else:
                return float(raw)
        except ValueError:
            print("  Try:  0.245  or  245mV  or  245m  -- press Enter to skip/accept")


# ---------------------------------------------------------------------------
# Calibration procedure
# ---------------------------------------------------------------------------

def run_calibration_jack(output_channel=0, input_channel=0,
                         ref_dbfs=-10.0, dmm_host=None,
                         range_start_hz=20.0, range_stop_hz=20000.0,
                         output_port=None, input_port=None):
    from .signal import make_sine
    from ..constants import SAMPLERATE

    freq         = 1000.0
    cal          = Calibration(output_channel=output_channel,
                               input_channel=input_channel)
    cal.ref_freq = freq
    cal.ref_dbfs = ref_dbfs
    amplitude    = 10.0 ** (ref_dbfs / 20.0)

    playback, capture = find_ports()
    out_port = resolve_port(playback, output_port, output_channel)
    in_port  = resolve_port(capture,  input_port,  input_channel)

    print(f"\n{'='*64}")
    print(f"  CALIBRATION  --  {freq:.0f} Hz tone at {ref_dbfs:.0f} dBFS")
    print(f"  Key: {cal.key}")
    print(f"{'='*64}")
    print(f"\n  STEP 1 -- Output voltage  (loaded)")
    print(f"  Connect output -> input with your loopback cable first.")
    print(f"  Probe the OUTPUT jack with DMM and enter the reading below.\n")

    engine = JackEngine()
    engine.set_tone(freq, amplitude)
    engine.start(output_ports=out_port, input_port=in_port)

    import time
    time.sleep(0.5)

    try:
        try:
            vrms_out = _parse_dmm("  DMM reading at output (e.g. 245mV or 0.245): ", dmm_host=dmm_host)
        finally:
            engine.set_silence()

        if vrms_out is None:
            print("  Skipped -- output uncalibrated, levels shown as dBFS only.")
            cal.summary()
            return cal

        cal.vrms_at_0dbfs_out = vrms_out / (10.0 ** (ref_dbfs / 20.0))
        print(f"\n  OK  {fmt_vrms(vrms_out)} at {ref_dbfs:.0f} dBFS"
              f"  =  {vrms_to_dbu(vrms_out):+.2f} dBu"
              f"  =  {fmt_vpp(vrms_out)}")
        print(f"      0 dBFS reference -> {fmt_vrms(cal.vrms_at_0dbfs_out)}"
              f"  =  {vrms_to_dbu(cal.vrms_at_0dbfs_out):+.2f} dBu")

        print(f"\n  STEP 2 -- Loopback capture  (auto)")
        print(f"  Capturing loopback to derive input scaling...\n")

        engine.set_tone(freq, amplitude)
        duration = max(1.0, 10.0 / freq)

        try:
            data = engine.capture_block(duration)
        except Exception as e:
            engine.set_silence()
            print(f"\n  !! Loopback capture failed: {e}")
            print("  Input calibration skipped.")
            cal.summary()
            cal.save()
            return cal

        engine.set_silence()

        mono           = data.astype(np.float64)
        trim           = int(len(mono) * 0.05)
        rec_linear_rms = float(np.sqrt(np.mean(mono[trim:-trim] ** 2)))
        rec_dbfs       = 20.0 * np.log10(max(rec_linear_rms, 1e-12))
        drop_db        = rec_dbfs - ref_dbfs

        print(f"  Loopback: {rec_dbfs:.2f} dBFS  ({drop_db:+.2f} dB vs played)")

        if drop_db < -40.0:
            print(f"\n  !! Signal too low ({drop_db:.1f} dB drop) -- input channel probably wrong")
            print(f"  !! Check:  ac setup  and verify input channel matches your cable.")
            print(f"  Input calibration skipped.\n")
        else:
            ratio                = 10.0 ** (drop_db / 20.0)
            cal.vrms_at_0dbfs_in = cal.vrms_at_0dbfs_out / ratio
            vrms_seen            = dbfs_to_vrms(rec_dbfs, cal.vrms_at_0dbfs_in)
            print(f"  Input jack: {fmt_vrms(vrms_seen)}"
                  f"  =  {vrms_to_dbu(vrms_seen):+.2f} dBu")
            print(f"  0 dBFS reference -> {fmt_vrms(cal.vrms_at_0dbfs_in)}"
                  f"  =  {vrms_to_dbu(cal.vrms_at_0dbfs_in):+.2f} dBu")

            # Step 3: automated response sweep
            print(f"\n  STEP 3 -- Response curve  (auto)")
            print(f"  Sweeping {range_start_hz:.0f}–{range_stop_hz:.0f} Hz...\n")
            n_pts        = 30
            sweep_freqs  = np.geomspace(range_start_hz, range_stop_hz, n_pts)
            response_curve = []
            for f_sw in sweep_freqs:
                dur_sw = max(0.3, 5.0 / f_sw)
                engine.set_tone(float(f_sw), amplitude)
                try:
                    engine.capture_block(min(0.2, dur_sw))   # warmup
                    data_sw = engine.capture_block(dur_sw)
                except Exception:
                    continue
                mono_sw = data_sw.astype(np.float64)
                trim_sw = int(len(mono_sw) * 0.05)
                if trim_sw * 2 >= len(mono_sw):
                    trim_sw = 0
                end_sw = len(mono_sw) - trim_sw if trim_sw else len(mono_sw)
                lin_sw  = float(np.sqrt(np.mean(mono_sw[trim_sw:end_sw] ** 2)))
                dbfs_sw = 20.0 * np.log10(max(lin_sw, 1e-12))
                response_curve.append((float(f_sw), float(dbfs_sw - rec_dbfs)))
                print(f"  {f_sw:>7.1f} Hz  {dbfs_sw - rec_dbfs:+.2f} dB", flush=True)

            engine.set_silence()
            cal.response_curve = response_curve
            print(f"\n  Response curve: {len(response_curve)} pts")

    finally:
        engine.set_silence()
        engine.stop()

    cal.summary()
    cal.save()
    return cal


# ---------------------------------------------------------------------------
# ZMQ calibration procedure (used by server worker)
# ---------------------------------------------------------------------------

def run_calibration_jack_zmq(pub_q, cal_q,
                              output_channel=0, input_channel=0,
                              ref_dbfs=-10.0, dmm_host=None,
                              range_start_hz=20.0, range_stop_hz=20000.0,
                              output_port=None, input_port=None):
    """Calibration for the ZMQ server: publishes cal_prompt/cal_done instead of
    using input().  pub_q is a queue.Queue; cal_q receives vrms from cal_reply."""
    import json
    import time

    def _pub(topic, frame):
        pub_q.put(topic.encode() + b" " + json.dumps(frame).encode())

    freq         = 1000.0
    cal          = Calibration(output_channel=output_channel,
                               input_channel=input_channel)
    cal.ref_freq = freq
    cal.ref_dbfs = ref_dbfs
    amplitude    = 10.0 ** (ref_dbfs / 20.0)

    playback, capture = find_ports()
    out_port = resolve_port(playback, output_port, output_channel)
    in_port  = resolve_port(capture,  input_port,  input_channel)

    engine = JackEngine()
    engine.set_tone(freq, amplitude)
    engine.start(output_ports=out_port, input_port=in_port)
    time.sleep(0.5)   # let analog output settle

    try:
        # Step 1: output voltage (optional — Enter skips if no DMM)
        dmm_vrms = _try_dmm_read(dmm_host) if dmm_host else None
        _pub("cal_prompt", {
            "step": 1,
            "text": (f"STEP 1 — Output voltage (loaded)\n"
                     f"  Connect output -> loopback cable.\n"
                     f"  Probe the OUTPUT jack with DMM and enter reading below."
                     + ("" if dmm_host else "\n  (no DMM configured — press Enter to skip)")),
            "dmm_vrms": dmm_vrms,
        })
        try:
            vrms_out = cal_q.get(timeout=120)
        except Exception:
            vrms_out = None
        finally:
            engine.set_silence()

        if vrms_out is not None:
            cal.vrms_at_0dbfs_out = vrms_out / (10.0 ** (ref_dbfs / 20.0))

        # Step 2: loopback capture at reference freq (also used as response curve reference)
        engine.set_tone(freq, amplitude)
        duration = max(1.0, 10.0 / freq)
        try:
            data = engine.capture_block(duration)
        except Exception as e:
            engine.set_silence()
            _pub("cal_done", {"key": cal.key,
                              "vrms_at_0dbfs_out": cal.vrms_at_0dbfs_out,
                              "error": f"loopback capture failed: {e}"})
            return cal

        engine.set_silence()

        mono           = data.astype(np.float64)
        trim           = int(len(mono) * 0.05)
        rec_linear_rms = float(np.sqrt(np.mean(mono[trim:-trim] ** 2)))
        rec_dbfs       = 20.0 * np.log10(max(rec_linear_rms, 1e-12))
        drop_db        = rec_dbfs - ref_dbfs

        if drop_db >= -40.0:
            if cal.vrms_at_0dbfs_out is not None:
                ratio                = 10.0 ** (drop_db / 20.0)
                cal.vrms_at_0dbfs_in = cal.vrms_at_0dbfs_out / ratio

            # Step 3: automated response sweep
            n_pts       = 30
            sweep_freqs = np.geomspace(range_start_hz, range_stop_hz, n_pts)
            response_curve = []
            for i, f_sw in enumerate(sweep_freqs):
                _pub("cal_progress", {
                    "step": 3, "n": i + 1, "total": n_pts, "freq_hz": float(f_sw),
                })
                dur_sw = max(0.3, 5.0 / f_sw)
                engine.set_tone(float(f_sw), amplitude)
                try:
                    engine.capture_block(min(0.2, dur_sw))   # warmup
                    data_sw = engine.capture_block(dur_sw)
                except Exception:
                    continue
                mono_sw = data_sw.astype(np.float64)
                trim_sw = int(len(mono_sw) * 0.05)
                if trim_sw * 2 >= len(mono_sw):
                    trim_sw = 0
                end_sw = len(mono_sw) - trim_sw if trim_sw else len(mono_sw)
                lin_sw  = float(np.sqrt(np.mean(mono_sw[trim_sw:end_sw] ** 2)))
                dbfs_sw = 20.0 * np.log10(max(lin_sw, 1e-12))
                response_curve.append((float(f_sw), float(dbfs_sw - rec_dbfs)))

            engine.set_silence()
            cal.response_curve = response_curve

        cal.save()
        _pub("cal_done", {
            "key":               cal.key,
            "vrms_at_0dbfs_out": cal.vrms_at_0dbfs_out,
            "vrms_at_0dbfs_in":  cal.vrms_at_0dbfs_in,
            "response_pts":      len(cal.response_curve) if cal.response_curve else 0,
        })
        return cal

    finally:
        engine.set_silence()
        engine.stop()
