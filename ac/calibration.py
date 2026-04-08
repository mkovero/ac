# calibration.py — Calibration data class and persistence.
# Extracted from server/jack_calibration.py; no audio engine dependency.
import json
import os

from .conversions import fmt_vrms, vrms_to_dbu, fmt_vpp, dbfs_to_vrms

DEFAULT_CAL_PATH = os.path.expanduser("~/.config/ac/cal.json")


def _cal_key(output_channel, input_channel):
    return f"out{output_channel}_in{input_channel}"


class Calibration:
    def __init__(self, output_channel=0, input_channel=0):
        self.output_channel    = output_channel
        self.input_channel     = input_channel
        self.ref_freq          = 1000.0
        self.vrms_at_0dbfs_out = None
        self.vrms_at_0dbfs_in  = None
        self.ref_dbfs          = -10.0

    @property
    def key(self):
        return _cal_key(self.output_channel, self.input_channel)

    @property
    def output_ok(self):
        return self.vrms_at_0dbfs_out is not None

    @property
    def input_ok(self):
        return self.vrms_at_0dbfs_in is not None

    def out_vrms(self, dbfs):
        if not self.output_ok:
            return None
        return dbfs_to_vrms(dbfs, self.vrms_at_0dbfs_out)

    def in_vrms(self, linear_rms):
        if not self.input_ok:
            return None
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
        return cal

    @classmethod
    def load_output_only(cls, output_channel, path=None):
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
                return cal
        return None

    @classmethod
    def load_all(cls, path=None):
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
        print("  --------------------------------------------------------------\n")
