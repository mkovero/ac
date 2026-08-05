"""Offline scoring of the absolute-floor rule against session-3 captures.

The rule under test: **admit any causal peak whose value exceeds
`NOISE_FLOOR_PROMINENCE` (12) x the median, and take the earliest such peak** —
dropping `MIN_PROMINENCE` (24, = 12 / DIRECT_PEAK_FRACTION) as an admission
gate entirely.

Narrow question asked of it: does that accept **A at 3.000 m** and **B at
3.2 m** — the two positions where the estimator resolves the arrival to a few
centimetres and refuses it — while still refusing **silence**? The near-wall
data is deliberately excluded here; it is scored separately because at that
position no candidate corresponds to the direct arrival at all.

Two halves, and only one of them is fully scoreable from these captures:

- **Admission** — "does the peak clear 12 x median" needs only `peak_value`
  and `median_value`, which every frame carries. Scored on every run.
- **Selection** — "which lag does earliest-above-floor pick" needs the
  candidate list, which only the three evidence captures carry. And even
  there the list is capped at `MAX_CANDIDATES` by rank, so an earlier
  qualifying ripple may exist that the capture never reported. **The replayed
  selection is therefore a bound, not the daemon's answer**: it can only be
  later than or equal to what the daemon would pick.
"""

import gzip
import json
import pickle
import statistics as st
import sys

FLOOR = 12.0  # NOISE_FLOOR_PROMINENCE
SHIPPED_GATE = 24.0  # MIN_PROMINENCE = 12 / 0.5
C_M_S = 346.0
CONST_MS = 1.1931  # measured converter constant, see rig-session-3-results.md


def implied_m(lag, sr=96000):
    return (lag / sr * 1e3 - CONST_MS) * 1e-3 * C_M_S


def rows(tag):
    d = json.load(gzip.open(f"{tag}.json.gz", "rt"))
    return [r for s in d["sessions"] for r in s if r["pair"] == "0-3"], d


def attempts(tag):
    """One record per estimator attempt (frames repeat the same attempt)."""
    d = json.load(gzip.open(f"{tag}.json.gz", "rt"))
    out = []
    for s in d["sessions"]:
        first = {}
        for r in s:
            if r["pair"] != "0-3":
                continue
            first.setdefault(r["delay_attempts"] or 0, r)
        out.append([first[a] for a in sorted(first) if a > 0])
    return out


def admission(tag, truth_lag=None):
    """Per-session: would the floor rule admit, and would the shipped gate?"""
    sess = attempts(tag)
    n_floor = n_gate = 0
    proms = []
    for s in sess:
        p = [r["prominence"] or 0.0 for r in s]
        proms += p
        if any(x >= FLOOR for x in p):
            n_floor += 1
        if any(x >= SHIPPED_GATE for x in p):
            n_gate += 1
    lags = {r["peak_lag"] for s in sess for r in s if r["peak_lag"] is not None}
    line = (f"{tag:16s} sessions {len(sess):2d}  "
            f"floor(12) admits {n_floor}/{len(sess)}  "
            f"gate(24) admits {n_gate}/{len(sess)}  "
            f"prominence {min(proms):5.2f}-{max(proms):5.2f}")
    if truth_lag is not None:
        ok = sum(1 for lg in lags if abs(lg - truth_lag) <= 15)
        line += f"  peak_lag {sorted(lags)[:4]} (truth ~{truth_lag})"
    print(line)
    return n_floor, n_gate, len(sess)


def selection(tag, truth_lag):
    """Replay 'earliest causal candidate above FLOOR x median' on a capture
    that kept its candidate lists."""
    fs = pickle.load(gzip.open(f"{tag}.pkl.gz", "rb"))
    acou = [f for f in fs if f.get("meas_channel") == 0 and f.get("delay_evidence")]
    picks, shipped = [], []
    for f in acou:
        ev = f["delay_evidence"]
        med = ev.get("median_value")
        cands = ev.get("candidates") or []
        causal = [c for c in cands if c["lag"] >= 0 and c["value"] >= FLOOR * med]
        if causal:
            picks.append(min(c["lag"] for c in causal))
        if f.get("delay_locked"):
            shipped.append(f["delay_samples"])
    if not picks:
        print(f"{tag:16s} no candidate cleared the floor")
        return
    print(f"{tag:16s} earliest-above-floor picks "
          f"{sorted(set(picks))[:5]} (median {st.median(picks):.0f} -> "
          f"{implied_m(st.median(picks)):.3f} m)  "
          f"shipped picked {sorted(set(shipped)) if shipped else 'refused'}  "
          f"truth ~{truth_lag} ({implied_m(truth_lag):.3f} m)")


if __name__ == "__main__":
    print("=== admission: does the peak clear 12 x median? ===")
    print("-- the two positions the rule is meant to rescue --")
    admission("runE-3m-A", truth_lag=938)
    admission("run2-B-alone", truth_lag=988)
    print("-- silence: must stay refused --")
    for t in ("baseline-before", "baseline-after", "baseline-final"):
        admission(t)
    print("-- reference: positions the shipped gate already accepts --")
    for t in ("run1-1m-spkA", "runC-2m-A", "runC-2m-B", "runD-A", "runD-B", "runD-AB"):
        admission(t)

    print("\n=== selection: which lag does 'earliest above floor' take? ===")
    print("(bounded — the candidate list is capped at MAX_CANDIDATES by rank)")
    selection("runD-A-evidence", truth_lag=628)
    selection("runD-AB-evidence", truth_lag=628)
