# config.py
# Persistent hardware configuration. Stored alongside calibration in
# ~/.config/thd_tool/config.json
import json
import os

DEFAULT_CONFIG_PATH = os.path.expanduser("~/.config/thd_tool/config.json")

# Exact mathematical 0 dBu = sqrt(0.001 * 600) = 0.77459667 Vrms
# Convention value used on most datasheets: 0.775 Vrms
# Some broadcast gear uses 0.7752 Vrms
DBU_REF_EXACT = 0.77459667

DEFAULTS = {
    "device":         0,
    "output_channel": 0,
    "input_channel":  0,
    "dbu_ref_vrms":   DBU_REF_EXACT,
    "dmm_host":       None,
    "range_start_hz": 20.0,
    "range_stop_hz":  20000.0,
}


def load(path=None):
    path = path or DEFAULT_CONFIG_PATH
    try:
        with open(path) as f:
            data = json.load(f)
        # merge with defaults so new keys always exist
        return {**DEFAULTS, **data}
    except (FileNotFoundError, json.JSONDecodeError):
        return dict(DEFAULTS)


def save(cfg, path=None):
    path = path or DEFAULT_CONFIG_PATH
    os.makedirs(os.path.dirname(path), exist_ok=True)
    # merge with existing so partial updates work
    existing = load(path)
    existing.update(cfg)
    with open(path, "w") as f:
        json.dump(existing, f, indent=2)
    return existing


def show(cfg):
    ref = cfg.get("dbu_ref_vrms", DBU_REF_EXACT)
    print(f"\n  -- Hardware config --")
    print(f"  Device:         {cfg['device']}")
    print(f"  Output channel: {cfg['output_channel']}")
    print(f"  Input channel:  {cfg['input_channel']}")
    print(f"  dBu reference: {ref*1000:.4f} mVrms  ({ref:.8f} V)")
    dmm = cfg.get("dmm_host")
    print(f"  DMM host:      {dmm if dmm else '(not configured)'}")
    print(f"  Range:         {cfg.get('range_start_hz', 20):.0f} – {cfg.get('range_stop_hz', 20000):.0f} Hz")
    print()
