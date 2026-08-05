"""The rate that should set admission: how often is a ripple both admitted
*and* earlier than the true arrival?

A noise ripple only causes harm under a conjunction — it must clear the
selection cut **and** sit earlier than the direct arrival, because the rule
takes the earliest qualifying candidate. Either alone is harmless: a late
ripple loses to the arrival, and an early one below the cut is not a
candidate.

That conjunction is far rarer than the marginal noise ceiling implies, and it
is measurable from captures rather than derived from a maximum that grows with
every dataset. This scores it directly.

Scope, stated because it bounds the answer: candidate lists exist only for the
captures below. Attempt counts are what they are — the ~500 attempts scored
elsewhere in this session carry `peak_value`/`median_value` but not
candidates, so they can answer admission questions and not this one.
"""

import gzip
import math
import pickle

import selection_floor as sf

CONST_MS = 1.1931
C_M_S = 346.0


def lag_for(metres, sr=96000):
    return (CONST_MS + metres / C_M_S * 1e3) * sr / 1e3


def attempts_of(tag):
    if tag.endswith("-evidence"):
        with gzip.open(f"{tag}.pkl.gz", "rb") as fh:
            fs = pickle.load(fh)
        first = {}
        for r in fs:
            if r.get("meas_channel") != 0 or not r.get("delay_evidence"):
                continue
            first.setdefault(r.get("delay_attempts"), r)
        return [first[a] for a in sorted(first) if a]
    at, _ = sf.attempts(tag)
    return at


def score(tag, truth_lag, label, admissions=(12, 16)):
    """Count attempts holding a candidate earlier than the arrival and above
    the cut — under the rule as it would run (0.5*peak) and under the cut each
    admission constant implies ((adm/2)*median)."""
    at = attempts_of(tag)
    rows = {}
    for name in ("0.5*peak",) + tuple(f"adm{a}" for a in admissions):
        rows[name] = 0
    n = 0
    worst = None
    for r in at:
        ev = r["delay_evidence"]
        med, peak = ev["median_value"], ev["peak_value"]
        cands = [c for c in (ev.get("candidates") or []) if 0 <= c["lag"] < truth_lag - 20]
        if not cands:
            n += 1
            continue
        n += 1
        cuts = {"0.5*peak": 0.5 * peak}
        for a in admissions:
            cuts[f"adm{a}"] = (a / 2.0) * med
        for name, cut in cuts.items():
            hits = [c for c in cands if c["value"] >= cut]
            if hits:
                rows[name] += 1
                if name == "0.5*peak":
                    e = min(hits, key=lambda c: c["lag"])
                    d = 20 * math.log10(e["value"] / peak)
                    if worst is None or e["lag"] < worst[0]:
                        worst = (e["lag"], d)
    cells = "  ".join(f"{k} {v}/{n}" for k, v in rows.items())
    extra = f"   earliest offender lag {worst[0]} at {worst[1]:.1f} dB" if worst else ""
    print(f"  {label:34s} {cells}{extra}")
    return rows, n


if __name__ == "__main__":
    print("attempts holding a candidate EARLIER than the arrival and above the cut")
    print("(the conjunction that actually causes a wrong-early lock)\n")
    score("cand-3m", 947, "A 3.000 m (truth 947)")
    score("cand-1m", 419, "A ~1.10 m (truth 419)")
    score("runD-A-evidence", 628, "A 1.8 m (truth 628)")
    score("runD-AB-evidence", 628, "A+B 1.8/2.5 m (truth 628)")
    print()
    print("near-boundary position, where no candidate is the direct arrival at all:")
    score("runF-wall-evidence", 780, "A 2.4 m, 28 cm off wall (truth 780)")
