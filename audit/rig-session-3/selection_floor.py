"""Scores the *selection* half of the gate against captures that kept their
candidate lists.

Four rules, all sharing the same admission decision except where noted:

  shipped   admit prominence >= 24; select earliest causal candidate
            >= max(0.5*peak, 12*median)
  floor     admit prominence >= 12; same selection rule as shipped. This is
            "drop the derived gate" — and below prominence 24 the median term
            binds, so the window collapses toward the global maximum.
  fixed     admit prominence >= 12; select earliest causal candidate
            >= 0.5*peak, with no noise term in selection at all.
  measured  admit prominence >= 12; select earliest causal candidate
            >= max(0.5*peak, S*median), where S is the *selection* floor
            measured from the silence capture rather than inherited from the
            full-scan admission floor.

The last one is the point. 12 is a multiple-comparison correction for scanning
~96000 lags. Selection compares the ~30 peaks already in the candidate list,
where the correction is much smaller — so reusing 12 there is over-strict and
is what collapses the window at low prominence.
"""

import gzip
import math
import pickle
import statistics as st
import sys

ADMIT_FLOOR = 12.0
SHIPPED_GATE = 24.0
FRACTION = 0.5
CONST_MS = 1.1931
C_M_S = 346.0


def load(tag):
    with gzip.open(f"{tag}.pkl.gz", "rb") as fh:
        return pickle.load(fh)


def attempts(tag):
    """One record per estimator attempt, acoustic pair only."""
    d = load(tag)
    first = {}
    for r in d["frames"]:
        if r["meas_channel"] != 0 or not r["delay_evidence"]:
            continue
        first.setdefault(r["delay_attempts"], r)
    return [first[a] for a in sorted(first) if a > 0], d


def implied_m(lag, sr=96000):
    return (lag / sr * 1e3 - CONST_MS) * 1e-3 * C_M_S


def select(ev, floor_mult, fraction=FRACTION, use_noise_term=True):
    """Earliest causal candidate clearing the rule's floor."""
    med, peak = ev["median_value"], ev["peak_value"]
    cut = fraction * peak
    if use_noise_term:
        cut = max(cut, floor_mult * med)
    causal = [c for c in ev.get("candidates") or [] if c["lag"] >= 0 and c["value"] >= cut]
    return min((c["lag"] for c in causal), default=None)


def ripple_stats(tag):
    """Silence: every candidate is noise, so `value / median` over the
    candidate set is the ceiling a selection floor has to clear."""
    at, _ = attempts(tag)
    worst_all, worst_win, n_win, n_cand = [], [], [], []
    for r in at:
        ev = r["delay_evidence"]
        med, peak = ev["median_value"], ev["peak_value"]
        cands = ev.get("candidates") or []
        n_cand.append(len(cands))
        others = [c for c in cands if c["value"] < peak]
        if others:
            worst_all.append(max(c["value"] for c in others) / med)
        win = [c for c in others if c["value"] >= FRACTION * peak]
        n_win.append(len(win))
        if win:
            worst_win.append(max(c["value"] for c in win) / med)

    def q(xs, f):
        xs = sorted(xs)
        return xs[min(len(xs) - 1, int(f * len(xs)))] if xs else float("nan")

    print(f"\n=== silence ripple statistics ({tag}, {len(at)} independent attempts) ===")
    print(f"  candidates per attempt: {min(n_cand)}-{max(n_cand)} "
          f"(the real comparison count for selection, not 96000)")
    print(f"  peak/median (prominence): "
          f"{min(r['delay_evidence']['prominence'] for r in at):.2f}-"
          f"{max(r['delay_evidence']['prominence'] for r in at):.2f}  "
          f"— admission floor 12 refuses all of these")
    print(f"  strongest non-peak candidate, value/median: "
          f"median {st.median(worst_all):.2f}  p90 {q(worst_all, 0.9):.2f}  "
          f"max {max(worst_all):.2f}")
    print(f"  candidates inside the 6 dB window (excl. peak): "
          f"{min(n_win)}-{max(n_win)}, median {st.median(n_win):.0f}")
    if worst_win:
        print(f"  strongest of those, value/median: median {st.median(worst_win):.2f}  "
              f"p90 {q(worst_win, 0.9):.2f}  **max {max(worst_win):.2f}**")
        print(f"  -> a selection floor of {math.ceil(max(worst_win) * 10) / 10:.1f}x median "
              f"clears every ripple seen here; 12 is "
              f"{12 / max(worst_win):.1f}x stricter than the data requires")
    return worst_win


def score(tag, truth, sel_floor):
    at, d = attempts(tag)
    print(f"\n=== {tag} — {d['notes'].get('position')} (truth {truth} = "
          f"{implied_m(truth):.3f} m) ===")
    rules = (
        ("shipped   ", SHIPPED_GATE, ADMIT_FLOOR, True),
        ("floor     ", ADMIT_FLOOR, ADMIT_FLOOR, True),
        ("fixed     ", ADMIT_FLOOR, 0.0, False),
        (f"measured({sel_floor:.1f})", ADMIT_FLOOR, sel_floor, True),
    )
    for name, gate, mult, noise in rules:
        picks, admitted = [], 0
        for r in at:
            ev = r["delay_evidence"]
            if ev["prominence"] < gate:
                continue
            admitted += 1
            lag = select(ev, mult, use_noise_term=noise)
            if lag is not None:
                picks.append(lag)
        if not admitted:
            print(f"  {name}: admits 0/{len(at)} attempts — no lock")
            continue
        ok = sum(1 for p in picks if abs(p - truth) <= 20)
        early = sum(1 for p in picks if p < truth - 20)
        late = sum(1 for p in picks if p > truth + 20)
        print(f"  {name}: admits {admitted:2d}/{len(at)}  correct {ok:2d}  "
              f"early(ripple) {early:2d}  late(reflection) {late:2d}  "
              f"lags {sorted(set(picks))[:6]}")


if __name__ == "__main__":
    worst = ripple_stats("cand-silence")
    sel_floor = math.ceil(max(worst) * 10) / 10 if worst else 6.0
    if len(sys.argv) > 1:
        sel_floor = float(sys.argv[1])
    score("cand-1m", 392, sel_floor)
    score("cand-3m", 947, sel_floor)
