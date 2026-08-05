"""Captures that retain full `delay_evidence.candidates`.

Everything in session 3 scored the *admission* half of the gate, because
admission needs only `peak_value` and `median_value`, which every frame
carries. The *selection* half — which lag the earliest-peak rule takes — needs
the candidate list, and the session's bulk runs were captured before those
were being kept.

Three captures, 20 s each:

  3m       A at 3.000 m on axis — where the shipped gate refuses 8/8 and the
           strongest peak is the direct arrival to 3 cm. The position the
           change exists to rescue.
  1m       A at 1.000 m — where the shipped gate already works, so the
           proposed selection rule must not make it worse.
  silence  no emission at all. Every candidate is noise by construction, so
           the distribution of (candidate value / median) over the *candidate
           set* is the empirical ceiling a selection floor must clear.

The last one is the point of the exercise. `NOISE_FLOOR_PROMINENCE` = 12 is a
multiple-comparison correction for scanning ~96000 lags; selection compares a
handful of already-identified peaks inside a 6 dB window, where the correction
is roughly half as large. Measuring the ceiling at the real candidate count
beats deriving it from a simulated one.

Usage:  python3 capture_candidates.py <tag> <channels|-> [seconds]
"""

import gzip
import pickle
import sys
import time

import rig
import s3

LEVEL = -30.0
SETTLE_S = 2.0


def keep(f):
    return {
        k: f.get(k)
        for k in (
            "meas_channel",
            "ref_channel",
            "delay_samples",
            "delay_ms",
            "delay_locked",
            "delay_attempts",
            "delay_evidence",
            "meas_peak_dbfs",
            "ref_peak_dbfs",
            "sr",
            "_t",
        )
    }


def capture(tag, channels, seconds=20.0, notes=None):
    rig.stop()
    time.sleep(0.5)
    try:
        if channels:
            r = rig.pink_on(LEVEL, channels)
            assert r.get("ok"), r
            time.sleep(SETTLE_S)
        s = rig.sub()
        r = rig.req("transfer_stream", pairs=s3.PAIRS)
        assert r.get("ok"), r
        fs = rig.frames(s, seconds)
        s.close()
    finally:
        rig.stop()  # never leave the rig emitting

    rows = [keep(f) for f in fs]
    out = {"tag": tag, "channels": channels, "seconds": seconds,
           "level_dbfs": LEVEL if channels else None, "notes": notes or {},
           "frames": rows}
    with gzip.open(f"{tag}.pkl.gz", "wb") as fh:
        pickle.dump(out, fh)

    acou = [r for r in rows if r["meas_channel"] == 0 and r["delay_evidence"]]
    attempts = {r["delay_attempts"]: r for r in acou}
    ncand = [len(r["delay_evidence"].get("candidates") or []) for r in acou]
    locked = sum(1 for r in acou if r["delay_locked"])
    print(
        f"  {tag}: {len(acou)} frames, {len(attempts)} attempts, "
        f"{locked} locked, candidates/frame "
        f"{min(ncand) if ncand else 0}-{max(ncand) if ncand else 0}",
        flush=True,
    )
    return out


if __name__ == "__main__":
    tag = sys.argv[1]
    chans = [int(c) for c in sys.argv[2].split(",")] if sys.argv[2] != "-" else []
    secs = float(sys.argv[3]) if len(sys.argv) > 3 else 20.0
    capture(tag, chans, secs)
