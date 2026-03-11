# jack_calibration.py  -- calibration + Calibration class (JACK backend)
import json
import os
import numpy as np
from .audio      import JackEngine, find_ports, port_name
from .conversions import fmt_vrms, vrms_to_dbu, fmt_vpp, dbfs_to_vrms

DEFAULT_CAL_PATH = os.path.expanduser("~/.config/thd_tool/cal.json")


# ---------------------------------------------------------------------------
# Calibration data class
# ---------------------------------------------------------------------------

def _cal_key(output_channel, input_channel, freq):
    return f"out{output_channel}_in{input_channel}_{freq:.0f}hz"


class Calibration:
    def __init__(self, output_channel=0, input_channel=0, freq=1000):
        self.output_channel    = output_channel
        self.input_channel     = input_channel
        self.freq              = freq
        self.vrms_at_0dbfs_out = None
        self.vrms_at_0dbfs_in  = None
        self.ref_dbfs          = -10.0
        self.dmm_ratio         = 1.0

    @property
    def key(self):
        return _cal_key(self.output_channel, self.input_channel, self.freq)

    @property
    def output_ok(self):
        return self.vrms_at_0dbfs_out is not None

    @property
    def input_ok(self):
        return self.vrms_at_0dbfs_in is not None

    def out_vrms(self, dbfs):
        return dbfs_to_vrms(dbfs, self.vrms_at_0dbfs_out) if self.output_ok else None

    def in_vrms(self, linear_rms):
        if not self.input_ok:
            return None
        return linear_rms * self.vrms_at_0dbfs_in

    @property
    def gain_correction(self):
        return self.dmm_ratio

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
            "freq":              self.freq,
            "vrms_at_0dbfs_out": self.vrms_at_0dbfs_out,
            "vrms_at_0dbfs_in":  self.vrms_at_0dbfs_in,
            "ref_dbfs":          self.ref_dbfs,
            "dmm_ratio":         self.dmm_ratio,
        }
        with open(path, "w") as f:
            json.dump(all_cals, f, indent=2)
        print(f"  Calibration saved -> {path}  (key: {self.key})")

    @classmethod
    def load(cls, output_channel=0, input_channel=0, freq=1000, path=None):
        path = path or DEFAULT_CAL_PATH
        if not os.path.exists(path):
            return None
        with open(path) as f:
            all_cals = json.load(f)
        key = _cal_key(output_channel, input_channel, freq)
        if key not in all_cals:
            return None
        data = all_cals[key]
        cal  = cls(output_channel=output_channel,
                   input_channel=input_channel,
                   freq=freq)
        cal.vrms_at_0dbfs_out = data.get("vrms_at_0dbfs_out")
        cal.vrms_at_0dbfs_in  = data.get("vrms_at_0dbfs_in")
        cal.ref_dbfs          = data.get("ref_dbfs", -10.0)
        cal.dmm_ratio         = data.get("dmm_ratio", 1.0)
        return cal

    def summary(self):
        import math
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
        if self.output_ok and self.input_ok:
            gc = self.gain_correction
            print(f"  Gain correction: {gc:.4f}x"
                  f"  ({20*math.log10(gc):+.2f} dB applied to input)")
        print("  --------------------------------------------------------------\n")


# ---------------------------------------------------------------------------
# DMM input helper
# ---------------------------------------------------------------------------

def _parse_dmm(prompt):
    while True:
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
            print("  Try:  0.245  or  245mV  or  245m  -- press Enter to skip")


# ---------------------------------------------------------------------------
# Calibration procedure
# ---------------------------------------------------------------------------

def run_calibration_jack(output_channel=0, input_channel=0,
                         ref_dbfs=-10.0, freq=1000):
    from .signal import make_sine
    from .constants import SAMPLERATE

    cal          = Calibration(output_channel=output_channel,
                               input_channel=input_channel,
                               freq=freq)
    cal.ref_dbfs = ref_dbfs
    amplitude    = 10.0 ** (ref_dbfs / 20.0)

    playback, capture = find_ports()
    out_port = port_name(playback, output_channel)
    in_port  = port_name(capture,  input_channel)

    print(f"\n{'='*64}")
    print(f"  CALIBRATION  --  {freq:.0f} Hz tone at {ref_dbfs:.0f} dBFS")
    print(f"  Key: {cal.key}")
    print(f"{'='*64}")
    print(f"\n  STEP 1 -- Output voltage  (loaded)")
    print(f"  Connect output -> input with your loopback cable first.")
    print(f"  Probe the OUTPUT jack with DMM and enter the reading below.\n")

    engine = JackEngine()
    engine.set_tone(freq, amplitude)
    engine.start(output_port=out_port, input_port=in_port)

    try:
        vrms_out = _parse_dmm("  DMM reading at output (e.g. 245mV or 0.245): ")
    finally:
        engine.set_silence()

    if vrms_out is None:
        print("  Skipped -- output uncalibrated, levels shown as dBFS only.")
    else:
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
        print(f"\n  !! Loopback capture failed: {e}")
        print("  Input calibration skipped.")
        engine.set_silence()
        engine.stop()
        cal.summary()
        return cal

    engine.set_silence()
    engine.stop()

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
    elif cal.output_ok:
        import math
        ratio                = 10.0 ** (drop_db / 20.0)
        cal.vrms_at_0dbfs_in = cal.vrms_at_0dbfs_out / ratio
        vrms_seen            = dbfs_to_vrms(rec_dbfs, cal.vrms_at_0dbfs_in)
        print(f"  Input jack: {fmt_vrms(vrms_seen)}"
              f"  =  {vrms_to_dbu(vrms_seen):+.2f} dBu")
        print(f"  0 dBFS reference -> {fmt_vrms(cal.vrms_at_0dbfs_in)}"
              f"  =  {vrms_to_dbu(cal.vrms_at_0dbfs_in):+.2f} dBu")
    else:
        print("  Output uncalibrated -- cannot derive input scaling.")

    cal.summary()
    cal.save()
    return cal
