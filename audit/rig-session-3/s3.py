"""Session-3 runner — one fresh transfer session per measurement point.

Every run here carries two pairs in one session:

    (3, 3)  capture_4 against itself — the electrical loopback (playback_2 ->
            capture_4) correlated with itself. Zero flight by construction,
            so whatever this reads is the estimator + buffering constant that
            must be subtracted from the acoustic pair. Handoff Run 1 step 1.
    (0, 3)  capture_1 (mic) against capture_4 — the acoustic path.

Both pairs live in the same session, so the constant is contemporaneous with
the measurement rather than inferred from an earlier evening.

The stimulus is a standalone generate_pink worker started BEFORE the session
(issue #226): the per-pair delay is estimated once when the rings first fill
and cached for the life of the session, so a session opened against silence
stays wrong forever. Emission is stopped between sessions.

Channels are JACK playback indices: 1 = playback_2 (AN2, reference loopback),
4 = playback_5 (right 1083, ADAT), 5 = playback_6 (left 1083, ADAT).
"""

import json
import pickle
import statistics as st
import sys
import time

import rig

LEVEL = -30.0
PAIRS = [[3, 3], [0, 3]]
ELEC, ACOU = "3-3", "0-3"
SETTLE_S = 2.0  # stimulus flowing before the session opens its rings
GAP_S = 3.0  # dead time between capture windows; ring is 2.5 s, so
# successive sessions share no samples (handoff Run 3)


def pair_key(f):
    return f"{f.get('meas_channel')}-{f.get('ref_channel')}"


def ev(f):
    return f.get("delay_evidence") or {}


def row(f):
    e = ev(f)
    return {
        "pair": pair_key(f),
        "delay_ms": f.get("delay_ms"),
        "delay_samples": f.get("delay_samples"),
        "delay_locked": f.get("delay_locked"),
        "delay_attempts": f.get("delay_attempts"),
        "prominence": e.get("prominence"),
        "peak_lag": e.get("peak_lag"),
        "peak_value": e.get("peak_value"),
        "noncausal_peak_lag": e.get("noncausal_peak_lag"),
        "noncausal_peak_value": e.get("noncausal_peak_value"),
        "median_value": e.get("median_value"),
        "negative_lag_median": e.get("negative_lag_median"),
        "n_candidates": len(e.get("candidates") or []),
        "meas_peak_dbfs": f.get("meas_peak_dbfs"),
        "ref_peak_dbfs": f.get("ref_peak_dbfs"),
        "sr": f.get("sr"),
        "t": f.get("_t"),
    }


def report(tag, i, fs):
    """One line per pair per session."""
    for key in (ELEC, ACOU):
        pf = [f for f in fs if pair_key(f) == key]
        if not pf:
            print(f"  {tag} s{i} {key}: no frames", flush=True)
            continue
        locked = [f for f in pf if f.get("delay_locked")]
        proms = [ev(f).get("prominence") or 0.0 for f in pf]
        mic = [f["meas_peak_dbfs"] for f in pf if f.get("meas_peak_dbfs") is not None]
        att = max((f.get("delay_attempts") or 0) for f in pf)
        head = f"  {tag} s{i} {key}: {len(locked)}/{len(pf)} locked"
        if locked:
            d = [f["delay_ms"] for f in locked]
            lag = [f["delay_samples"] for f in locked]
            print(
                f"{head}  delay={st.median(d):.4f} ms "
                f"({st.median(lag):.0f} smp, spread {max(lag) - min(lag)})  "
                f"prom={st.median(proms):.2f}  att={att}  "
                f"meas={st.median(mic):.1f} dBFS",
                flush=True,
            )
        else:
            print(
                f"{head}  REFUSED  prom med={st.median(proms):.2f} "
                f"max={max(proms):.2f}  att={att}  "
                f"meas={st.median(mic):.1f} dBFS  "
                f"peak_lag={ev(pf[-1]).get('peak_lag')}",
                flush=True,
            )


def run(tag, channels, n, capture_s=15.0, notes=None):
    """n fresh sessions. `channels` are the pink worker's playback indices;
    pass an empty list for a silent baseline (no emission at all)."""
    out = {
        "tag": tag,
        "channels": channels,
        "n": n,
        "capture_s": capture_s,
        "level_dbfs": LEVEL if channels else None,
        "notes": notes or {},
        "started": time.time(),
        "sessions": [],
    }
    try:
        for i in range(1, n + 1):
            rig.stop()
            time.sleep(0.5)
            if channels:
                r = rig.pink_on(LEVEL, channels)
                if not r.get("ok"):
                    print(f"  {tag} s{i}: pink failed: {r}", flush=True)
                    break
                time.sleep(SETTLE_S)

            s = rig.sub()
            r = rig.req("transfer_stream", pairs=PAIRS)
            if not r.get("ok"):
                print(f"  {tag} s{i}: transfer failed: {r}", flush=True)
                rig.stop()
                s.close()
                break

            fs = rig.frames(s, capture_s)
            s.close()
            rig.stop()  # emission off between sessions

            out["sessions"].append([row(f) for f in fs])
            report(tag, i, fs)
            time.sleep(GAP_S)
    finally:
        rig.stop()  # never leave the rig emitting
        out["ended"] = time.time()
        with open(f"{tag}.pkl", "wb") as fh:
            pickle.dump(out, fh)
        with open(f"{tag}.json", "w") as fh:
            json.dump(out, fh)
        print(f"  saved {len(out['sessions'])} sessions to {tag}.pkl", flush=True)
    return out


if __name__ == "__main__":
    tag = sys.argv[1]
    chans = [int(c) for c in sys.argv[2].split(",")] if sys.argv[2] != "-" else []
    n = int(sys.argv[3]) if len(sys.argv) > 3 else 8
    cap = float(sys.argv[4]) if len(sys.argv) > 4 else 15.0
    run(tag, chans, n, cap)
