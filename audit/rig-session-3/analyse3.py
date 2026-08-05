"""Scoring for session 3.

Two things the earlier sessions could not do:

1. **Geometry, not history.** Every session here carries the electrical
   loopback pair (3,3) alongside the acoustic pair (0,3), so the zero-flight
   constant is measured in the same session as the arrival it is subtracted
   from. `flight()` converts a locked acoustic lag into an implied distance
   at c = 346 m/s (24-26 degC; see handoff-rig-session-3.md), given the
   converter constant supplied on the command line.

2. **Independent windows.** `independent()` keeps only attempts at least
   RING_S apart inside one session, so "successive estimates agree" stops
   partly measuring buffer overlap. Across sessions every estimate is already
   independent — s3.run leaves GAP_S of dead air and each session refills its
   rings from scratch.
"""

import gzip
import json
import os
import pickle
import statistics as st
import sys

C_M_S = 346.0  # 25 degC, the room's constant tonight. NOT 343 (that is 20 degC).
RING_S = 2.5  # correlation ring length; attempts closer than this share samples
ELEC, ACOU = "3-3", "0-3"


def load(tag):
    """Records are stored as gzipped JSON — portable, and small enough to
    keep in the repo. `.pkl` is written too while a session is running; the
    JSON is what survives."""
    if os.path.exists(f"{tag}.json.gz"):
        with gzip.open(f"{tag}.json.gz", "rt") as fh:
            return json.load(fh)
    with open(f"{tag}.pkl", "rb") as fh:
        return pickle.load(fh)


def pair(session, key):
    return [r for r in session if r["pair"] == key]


def locked(rows):
    return [r for r in rows if r["delay_locked"]]


def med(xs):
    return st.median(xs) if xs else None


def flight(delay_ms, const_ms):
    """Implied one-way distance for an acoustic lag, after subtracting the
    electrical/converter constant."""
    return (delay_ms - const_ms) * 1e-3 * C_M_S


def independent(rows, ring_s=RING_S):
    """Thin a session's frames down to estimates whose correlation windows do
    not overlap: keep the first frame of each new attempt, then drop any kept
    frame closer than ring_s to the previous one."""
    seen = {}
    for r in rows:
        a = r["delay_attempts"] or 0
        if a not in seen:
            seen[a] = r
    firsts = [seen[a] for a in sorted(seen)]
    out = []
    for r in firsts:
        if not out or (r["t"] - out[-1]["t"]) >= ring_s:
            out.append(r)
    return out


def summarise(tag, const_ms=0.0, verbose=True):
    d = load(tag)
    print(f"\n== {tag} ==  channels={d['channels']} n={len(d['sessions'])} "
          f"cap={d['capture_s']}s  {d['notes']}")

    elec_lags, acou, proms, atts, mics, refs = [], [], [], [], [], []
    for i, s in enumerate(d["sessions"], 1):
        e, a = pair(s, ELEC), pair(s, ACOU)
        el = locked(e)
        if el:
            elec_lags += [r["delay_samples"] for r in el]
        al = locked(a)
        p = [r["prominence"] or 0.0 for r in a]
        proms += p
        atts.append(max((r["delay_attempts"] or 0) for r in a) if a else 0)
        mics += [r["meas_peak_dbfs"] for r in a if r["meas_peak_dbfs"] is not None]
        refs += [r["ref_peak_dbfs"] for r in a if r["ref_peak_dbfs"] is not None]
        if al:
            ms = med([r["delay_ms"] for r in al])
            lag = med([r["delay_samples"] for r in al])
            acou.append((i, ms, lag, med(p)))
            if verbose:
                print(f"  s{i}: LOCK  {ms:.4f} ms  lag {lag:.0f}  "
                      f"prom {med(p):.2f}  implied {flight(ms, const_ms):.4f} m")
        elif verbose:
            print(f"  s{i}: refuse      prom med {med(p):.2f} "
                  f"max {max(p):.2f}  attempts {atts[-1]}")

    print(f"  electrical pair: {len(elec_lags)} locked frames, "
          f"lag {set(elec_lags) if len(set(elec_lags)) < 5 else 'varies'}")
    print(f"  mic {med(mics):.1f} dBFS   ref {med(refs):.1f} dBFS   "
          f"prominence min/med/max {min(proms):.2f}/{med(proms):.2f}/{max(proms):.2f}")
    print(f"  locked {len(acou)}/{len(d['sessions'])} sessions   "
          f"attempts/session median {med(atts):.0f}")
    if acou:
        ms = [a[1] for a in acou]
        print(f"  locked delay median {med(ms):.4f} ms  "
              f"spread {max(ms) - min(ms):.4f} ms  "
              f"implied distance {flight(med(ms), const_ms):.4f} m")
    return d, acou, proms


def repeatability(tag, ring_s=RING_S):
    """Session 3's Run 3: spread of estimates scored on windows that do not
    share samples, against the same spread scored on every attempt."""
    d = load(tag)
    all_dev, ind_dev = [], []
    for s in d["sessions"]:
        rows = pair(s, ACOU)
        if not rows:
            continue
        firsts = {}
        for r in rows:
            firsts.setdefault(r["delay_attempts"] or 0, r)
        seq = [firsts[a] for a in sorted(firsts)]
        lags = [r["peak_lag"] for r in seq if r["peak_lag"] is not None]
        all_dev += [abs(b - a) for a, b in zip(lags, lags[1:])]
        ind = independent(rows, ring_s)
        ilags = [r["peak_lag"] for r in ind if r["peak_lag"] is not None]
        ind_dev += [abs(b - a) for a, b in zip(ilags, ilags[1:])]

    def q(xs, f):
        if not xs:
            return float("nan")
        xs = sorted(xs)
        return xs[min(len(xs) - 1, int(f * len(xs)))]

    print(f"\n== repeatability {tag} ==  ring {ring_s}s")
    for name, xs in (("every attempt (overlapping)", all_dev),
                     ("independent windows", ind_dev)):
        if not xs:
            print(f"  {name}: no pairs")
            continue
        print(f"  {name}: n={len(xs)}  |dlag| median {med(xs):.0f}  "
              f"p90 {q(xs, 0.9):.0f}  max {max(xs)}  "
              f"(p90 = {q(xs, 0.9) / 96.0:.2f} ms, "
              f"{q(xs, 0.9) / 96000.0 * C_M_S:.3f} m)")
    return all_dev, ind_dev


if __name__ == "__main__":
    const = float(sys.argv[2]) if len(sys.argv) > 2 else 0.0
    summarise(sys.argv[1], const)
    repeatability(sys.argv[1])
