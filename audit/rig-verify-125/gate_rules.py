"""Offline scoring of candidate delay-gate rules against the rig-verify-125
captures (handover-desk-work.md item 2).

The deliverable is a RANKED COMPARISON of rules, not a constant. One mic
position, and the speaker configuration during the captures is unrecorded —
see the confounds section of the report.

Every capture is replayed from its own `delay_evidence`, which #237 made
self-reproducing. Nothing here re-runs the estimator; it re-scores what the
estimator recorded.

    python3 gate_rules.py            # summary tables
    python3 gate_rules.py --csv      # per-session rows, machine-readable
"""

import gzip
import pickle
import statistics as st
import sys

SR = 96000

# The rule in the shipped estimator, for reference rows only.
NOISE_FLOOR_PROMINENCE = 12.0
DIRECT_PEAK_FRACTION = 0.5
MIN_PROMINENCE = NOISE_FLOOR_PROMINENCE / DIRECT_PEAK_FRACTION  # 24.0


def load(name):
    return pickle.load(gzip.open(f"{name}-evidence.pkl.gz", "rb"))


def attempts(frames):
    """Frames -> the distinct estimator attempts behind them.

    A refusing pair retries at 1 Hz while frames publish at ~10 Hz, and the
    evidence is republished verbatim on every frame in between (deliberately —
    a late subscriber must still see it). Counting frames would therefore
    count each attempt about ten times and make any "N successive estimates
    agree" rule look ten times stronger than it is.
    """
    out = []
    prev = None
    for f in frames:
        ev = f.get("delay_evidence") or {}
        if not ev:
            continue
        key = (
            ev.get("peak_lag"),
            ev.get("peak_value"),
            ev.get("median_value"),
            ev.get("negative_lag_median"),
        )
        if key != prev:
            out.append((ev, bool(f.get("delay_locked")), f.get("delay_samples"), f["_t"]))
            prev = key
    return out


class Session:
    """One capture with a label from the rig setup, not from the data."""

    def __init__(self, name, label, frames, note=""):
        self.name = name
        self.label = label  # "pos" = a path existed; "neg" = none existed
        self.note = note
        self.frames = frames
        self.att = attempts(frames)
        self.att_times = [a[3] for a in self.att]
        self.locked_frames = sum(1 for f in frames if f.get("delay_locked"))

    # -- scalars ---------------------------------------------------------
    def prom_all(self):
        """Published prominence: peak / median over ALL lags."""
        return [a[0].get("prominence") or 0.0 for a in self.att]

    def prom_neg(self):
        """Prominence recomputed against the negative-lag floor.

        A causal path puts no signal at negative lags, so that median is a
        noise floor uncontaminated by the reverberation the all-lag median
        includes. `None` where the capture predates the field or the floor
        is zero.
        """
        out = []
        for ev, _, _, _ in self.att:
            pv = ev.get("peak_value")
            nm = ev.get("negative_lag_median")
            out.append(pv / nm if pv is not None and nm else None)
        return out

    def peak_lags(self):
        return [a[0].get("peak_lag") for a in self.att]


def sessions():
    out = []
    for i, fs in enumerate(load("run1"), 1):
        out.append(
            Session(
                f"run1-s{i}",
                "pos",
                fs,
                "3 m on-axis, pink into speaker leg + ref loopback",
            )
        )
    r4 = load("run4")
    out.append(
        Session(
            "run4-unrelated",
            "neg",
            r4["unrelated"],
            "pink into the ref loopback only; mic hears room noise",
        )
    )
    out.append(
        Session("run4-healthy", "pos", r4["healthy"], "both legs driven, Run 1 config")
    )
    for tag in ("baseline_before", "baseline_after"):
        out.append(
            Session(tag, "neg", load(tag), "ref leg at -95 dBFS: nothing to correlate")
        )
    for i, run in enumerate(load("onset"), 1):
        out.append(
            Session(f"onset{i}-pre", "neg", run["pre"], "session open, stimulus not yet on")
        )
        out.append(Session(f"onset{i}-post", "pos", run["post"], "stimulus running"))
    return out


# ---------------------------------------------------------------------------
# Rule A — repeatability. Accept when N successive independent estimates
# agree within k samples.
# ---------------------------------------------------------------------------
def rule_repeat(sess, n, k):
    lags = [x for x in sess.peak_lags() if x is not None]
    if len(lags) < n:
        return False, None
    for i in range(len(lags) - n + 1):
        w = lags[i : i + n]
        if max(w) - min(w) <= k:
            return True, w[0]
    return False, None


def rule_repeat_lags(lags, n, k):
    lags = [x for x in lags if x is not None]
    if len(lags) < n:
        return False
    return any(
        max(lags[i : i + n]) - min(lags[i : i + n]) <= k
        for i in range(len(lags) - n + 1)
    )


# The daemon's H1 window: nperseg + step*(n_averages-1) = 2.5 s at any rate.
RING_S = 2.5


def independent_lags(sess):
    """Peak lags from attempts at least one ring apart.

    Successive retries reuse most of the same samples, so agreement between
    them is partly agreement with themselves. This subsamples to attempts
    whose correlation windows do not overlap at all.
    """
    keep, last = [], float("-inf")
    for (ev, _, _, _), t in zip(sess.att, sess.att_times):
        if t - last >= RING_S:
            keep.append(ev.get("peak_lag"))
            last = t
    return keep


def repeat_stats(sess):
    """Descriptive, threshold-free: how concentrated are the estimates."""
    lags = [x for x in sess.peak_lags() if x is not None]
    if not lags:
        return 0, 0.0, 0
    mode = st.mode(lags)
    return mode, sum(1 for x in lags if x == mode) / len(lags), len(set(lags))


# ---------------------------------------------------------------------------
# Rule B — prominence against the negative-lag floor.
# ---------------------------------------------------------------------------
def rule_prom(values, t):
    vals = [v for v in values if v is not None]
    return bool(vals) and st.median(vals) >= t


def rule_prom_any(values, t):
    """The daemon's own decision procedure, not the session median.

    `transfer_stream` accepts on the FIRST attempt that clears the gate and
    caches it for the session, so a session-median score answers a different
    question than the shipped code does — and the difference is not academic:
    onset4-post's median prominence is 18.0 and it locked, because one attempt
    reached 31.
    """
    vals = [v for v in values if v is not None]
    return any(v >= t for v in vals)


def separation(ss, accept):
    """(#pos accepted, #pos, #neg accepted, #neg) for a predicate."""
    pos = [s for s in ss if s.label == "pos"]
    neg = [s for s in ss if s.label == "neg"]
    return (
        sum(1 for s in pos if accept(s)),
        len(pos),
        sum(1 for s in neg if accept(s)),
        len(neg),
    )


def med(xs):
    xs = [x for x in xs if x is not None]
    return st.median(xs) if xs else float("nan")


def main():
    ss = sessions()

    print("== captures ==")
    print(
        f"{'session':<16} {'label':<4} {'frames':>6} {'attempts':>8} "
        f"{'locked_fr':>9} {'prom_all':>9} {'prom_neg':>9} {'mode_lag':>8} "
        f"{'mode_frac':>9} {'uniq':>5}"
    )
    for s in ss:
        mode, frac, uniq = repeat_stats(s)
        print(
            f"{s.name:<16} {s.label:<4} {len(s.frames):>6} {len(s.att):>8} "
            f"{s.locked_frames:>9} {med(s.prom_all()):>9.2f} "
            f"{med(s.prom_neg()):>9.2f} {mode:>8} {frac:>9.2f} {uniq:>5}"
        )

    print("\n== rule 0: the shipped gate (all-lag prominence >= 24) ==")
    a, na, b, nb = separation(ss, lambda s: rule_prom(s.prom_all(), MIN_PROMINENCE))
    print(f"session median : accepts {a}/{na} positives, {b}/{nb} negatives")
    a, na, b, nb = separation(ss, lambda s: rule_prom_any(s.prom_all(), MIN_PROMINENCE))
    print(f"any attempt    : accepts {a}/{na} positives, {b}/{nb} negatives  "
          f"(this is what the daemon does — first accept is cached)")
    for s in ss:
        hit = [v for v in s.prom_all() if v >= MIN_PROMINENCE]
        if hit:
            print(f"  {s.name} ({s.label}) clears 24 on {len(hit)}/{len(s.att)} "
                  f"attempts, max {max(s.prom_all()):.2f}")

    print("\n== rule A: N successive estimates agree within k samples ==")
    print(f"{'N':>3} {'k':>4}  {'pos accepted':>13}  {'neg accepted':>13}")
    for n in (2, 3, 4, 5, 8):
        for k in (0, 1, 2, 4, 8, 16):
            a, na, b, nb = separation(ss, lambda s, n=n, k=k: rule_repeat(s, n, k)[0])
            print(f"{n:>3} {k:>4}  {a:>6}/{na:<6}  {b:>6}/{nb:<6}")

    print("\n== rule B: median prominence against the negative-lag floor >= t ==")
    print(f"{'t':>6}  {'pos accepted':>13}  {'neg accepted':>13}")
    for t in (4, 6, 8, 10, 12, 16, 20, 24, 30):
        a, na, b, nb = separation(ss, lambda s, t=t: rule_prom(s.prom_neg(), t))
        print(f"{t:>6}  {a:>6}/{na:<6}  {b:>6}/{nb:<6}")

    print("\n== rule B, any-attempt form (the daemon's cache-on-first-accept) ==")
    print(f"{'t':>6}  {'pos accepted':>13}  {'neg accepted':>13}")
    for t in (4, 6, 8, 10, 12, 16, 20, 24):
        a, na, b, nb = separation(ss, lambda s, t=t: rule_prom_any(s.prom_neg(), t))
        print(f"{t:>6}  {a:>6}/{na:<6}  {b:>6}/{nb:<6}")

    print("\n== the two floors, side by side ==")
    print(f"{'session':<16} {'label':<4} {'median_value':>12} {'neg_lag_med':>12} "
          f"{'ratio':>7} {'peak_value':>11}")
    for s in ss:
        ev = [a[0] for a in s.att]
        mv = st.median([e["median_value"] for e in ev])
        nm = st.median([e["negative_lag_median"] for e in ev])
        pv = st.median([e["peak_value"] for e in ev])
        print(f"{s.name:<16} {s.label:<4} {mv:>12.4f} {nm:>12.4f} "
              f"{mv / nm:>7.3f} {pv:>11.4f}")

    print("\n== ranking: does the floor change the order? ==")
    print(f"{'session':<16} {'label':<4} {'rank_all':>9} {'rank_neg':>9} {'move':>5}")
    by_all = sorted(ss, key=lambda s: -med(s.prom_all()))
    by_neg = sorted(ss, key=lambda s: -med(s.prom_neg()))
    ra = {s.name: i + 1 for i, s in enumerate(by_all)}
    rn = {s.name: i + 1 for i, s in enumerate(by_neg)}
    for s in by_all:
        print(
            f"{s.name:<16} {s.label:<4} {ra[s.name]:>9} {rn[s.name]:>9} "
            f"{rn[s.name] - ra[s.name]:>+5}"
        )

    print("\n== rule A+B: both must hold (N=3, k=2; floor threshold swept) ==")
    print(f"{'t':>6}  {'pos accepted':>13}  {'neg accepted':>13}")
    for t in (4, 6, 8, 10, 12, 16):
        a, na, b, nb = separation(
            ss,
            lambda s, t=t: rule_repeat(s, 3, 2)[0] and rule_prom(s.prom_neg(), t),
        )
        print(f"{t:>6}  {a:>6}/{na:<6}  {b:>6}/{nb:<6}")

    print("\n== rule A on INDEPENDENT attempts only (>= one ring apart) ==")
    print("the ring is 2.5 s and retries run at 1 Hz, so successive attempts")
    print("share ~60% of their samples — 'N successive estimates agree' is not")
    print("N independent estimates unless the attempts are spaced out first.")
    print(f"{'N':>3} {'k':>5}  {'pos accepted':>13}  {'neg accepted':>13}")
    for n in (2, 3, 4):
        for k in (0, 2, 16, 120):
            a, na, b, nb = separation(
                ss, lambda s, n=n, k=k: rule_repeat_lags(independent_lags(s), n, k)
            )
            print(f"{n:>3} {k:>5}  {a:>6}/{na:<6}  {b:>6}/{nb:<6}")

    if "--csv" in sys.argv:
        print("\nname,label,frames,attempts,locked_frames,prom_all,prom_neg,mode_lag,mode_frac,uniq_lags")
        for s in ss:
            mode, frac, uniq = repeat_stats(s)
            print(
                f"{s.name},{s.label},{len(s.frames)},{len(s.att)},{s.locked_frames},"
                f"{med(s.prom_all()):.4f},{med(s.prom_neg()):.4f},{mode},{frac:.4f},{uniq}"
            )


if __name__ == "__main__":
    main()
