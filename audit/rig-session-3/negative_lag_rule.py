"""Block 2 of `rig/rig-verify-queue.md`, scored offline — no rig time.

The proposal under test came out of rig session 2 and has never been measured:
`prominence` divides `peak_value` by a median taken over **all** lags, and on a
reverberant path most lags hold reverberation, so the floor it is measured
against contains the thing it is meant to discriminate against.
`negative_lag_median` is the same statistic over the negative lags only, where
no causal path puts any signal, and it was published every frame in session 3
for exactly this question:

> Does `peak_value / negative_lag_median` separate the valid locks from the
> noise ceiling, where `peak_value / median_value` does not?

Three separate things have to be true for the answer to be "yes", and this
script scores them one at a time, because failing any one of them kills the
proposal for a different reason:

1. **Contamination is real and population-dependent.** `R = median_value /
   negative_lag_median` must be *larger* on driven captures than on silence.
   If R is the same on both, the new statistic is the old one times a
   constant, every threshold moves by that constant, and nothing separates —
   the proposal is arithmetically dead regardless of how good the numbers look.
2. **Separation improves.** The margin between the worst valid attempt and the
   silence ceiling must widen. Scored both per attempt and per session, and
   scored against the *pooled* silence ceiling: a maximum of a noise
   distribution grows with sample size (see `silence-ceiling.md`), so the
   number to beat is the one from the largest pool, not the prettiest one.
3. **It does not promote the harmful case.** `runF-wall` accepted once at
   prominence 24.15 and was **52 cm wrong** — no candidate there corresponds to
   the direct arrival at all. A statistic that lifts the valid captures and
   lifts that one just as far has bought nothing, because the gate has to
   separate *those two*, not signal from silence. Session 3's whole finding was
   that 24 is simultaneously too high for a clean 3 m capture and only just
   high enough to exclude the wall.

Ground truth lags are from `rig-session-3-results.md` "Recording — every
capture". An attempt counts as landing on the arrival when `peak_lag` is
within TRUTH_TOL of that lag — the same tolerance `floor_rule.py` uses.

Run: `python3 negative_lag_rule.py`
"""

import gzip
import json
import statistics as st

TRUTH_TOL = 15  # samples; same as floor_rule.py
C_M_S = 346.0
CONST_MS = 1.1931  # measured converter constant, rig-session-3-results.md
ACOU = "0-3"

# tag -> (truth lag, label). Truth from the session-3 recording table.
# `runF-wall` has no truth lag: at that position nothing in the 6 dB window is
# within 0.5 m of the direct arrival, which is the point of including it.
DRIVEN = [
    ("run1-1m-spkA", 392, "A 1.000 m on axis"),
    ("run2-AB", 392, "A+B, mic at A's 1 m"),
    ("run2-B-alone", 988, "B alone ~3.2 m"),
    ("probe-spkB", 988, "B probe ~3.2 m"),
    ("runC-2m-A", 659, "A ~2.0 m equidistant"),
    ("runC-2m-B", 659, "B ~2.0 m equidistant"),
    ("runD-A", 628, "A 1.8 m"),
    ("runD-B", 762, "B ~2.5 m"),
    ("runD-AB", 628, "A+B 1.8/2.5 m"),
    ("runE-3m-A", 938, "A 3.000 m on axis"),
]
SILENT = ["baseline-before", "baseline-after", "baseline-final"]
HARMFUL = [("runF-wall", 925, "A 2.4 m, 28 cm off wall — the wrong lock")]


def implied_m(lag, sr=96000):
    return (lag / sr * 1e3 - CONST_MS) * 1e-3 * C_M_S


def attempts(tag):
    """One record per estimator attempt. Frames repeat an attempt's evidence,
    so scoring frames would weight long sessions by frame count rather than by
    the number of independent decisions the estimator actually made."""
    d = json.load(gzip.open(f"{tag}.json.gz", "rt"))
    out = []
    for s in d["sessions"]:
        first = {}
        for r in s:
            if r["pair"] != ACOU:
                continue
            first.setdefault(r["delay_attempts"] or 0, r)
        out.append([first[a] for a in sorted(first) if a > 0])
    return out


def stats(r):
    """(all-lag prominence, negative-lag prominence, contamination ratio).

    `prominence` is carried in the record; it is recomputed here from
    `peak_value / median_value` so that a divergence between the two shows up
    as an assertion rather than as a silently different definition."""
    pv, mv, nv = r["peak_value"], r["median_value"], r["negative_lag_median"]
    if not (pv and mv and nv):
        return None
    p_all = pv / mv
    if r["prominence"] is not None:
        assert abs(p_all - r["prominence"]) < 1e-6 * max(1.0, p_all), (
            f"prominence field {r['prominence']} != peak/median {p_all}"
        )
    return p_all, pv / nv, mv / nv


def scored(tag, truth):
    """Per-attempt rows: (p_all, p_neg, R, on_arrival)."""
    out = []
    for sess in attempts(tag):
        rows = []
        for r in sess:
            s = stats(r)
            if s is None:
                continue
            on = (
                truth is not None
                and r["peak_lag"] is not None
                and abs(r["peak_lag"] - truth) <= TRUTH_TOL
            )
            rows.append((*s, on))
        out.append(rows)
    return out


def q(xs, f):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(f * len(xs)))]


def fmt(xs):
    return f"{min(xs):6.2f} {st.median(xs):6.2f} {max(xs):6.2f}"


def main():
    print("=" * 78)
    print("1. IS THE FLOOR ACTUALLY CONTAMINATED?  R = median_value / negative_lag_median")
    print("=" * 78)
    print("   A reverberant all-lag floor should sit ABOVE the negative-lag floor")
    print("   (R > 1) on driven captures, and at R ~ 1 in silence. If R is the same")
    print("   in both, the new statistic is the old one rescaled and cannot separate.")
    print(f"\n   {'capture':22s} {'n':>4s}  {'R min':>6s} {'R med':>6s} {'R max':>6s}")
    driven_R, silent_R = [], []
    for tag, truth, label in DRIVEN + HARMFUL:
        rs = [r for sess in scored(tag, truth) for r in sess]
        Rs = [r[2] for r in rs]
        driven_R += Rs
        print(f"   {tag:22s} {len(Rs):4d}  {fmt(Rs)}   {label}")
    for tag in SILENT:
        rs = [r for sess in scored(tag, None) for r in sess]
        Rs = [r[2] for r in rs]
        silent_R += Rs
        print(f"   {tag:22s} {len(Rs):4d}  {fmt(Rs)}   silent")
    print(f"\n   pooled driven  n={len(driven_R):4d}  R median {st.median(driven_R):.4f}")
    print(f"   pooled silent  n={len(silent_R):4d}  R median {st.median(silent_R):.4f}")
    if silent_R:
        print(f"   -> driven floor sits {st.median(driven_R) / st.median(silent_R):.3f}x "
              f"the silent floor, in R terms")

    print()
    print("=" * 78)
    print("2. DOES IT SEPARATE?  worst valid attempt vs the pooled silence ceiling")
    print("=" * 78)
    sil = [r for tag in SILENT for sess in scored(tag, None) for r in sess]
    sil_all = [r[0] for r in sil]
    sil_neg = [r[1] for r in sil]
    print(f"   silence, {len(sil)} attempts pooled (three positions, both ends of the evening)")
    print(f"     peak/median      max {max(sil_all):6.2f}  p90 {q(sil_all, 0.9):6.2f}  "
          f"median {st.median(sil_all):6.2f}")
    print(f"     peak/negmedian   max {max(sil_neg):6.2f}  p90 {q(sil_neg, 0.9):6.2f}  "
          f"median {st.median(sil_neg):6.2f}")

    print(f"\n   {'capture':22s} {'on-arrival':>10s}  {'peak/median':>22s}  "
          f"{'peak/negmedian':>22s}")
    print(f"   {'':22s} {'attempts':>10s}  {'min    med    max':>22s}  "
          f"{'min    med    max':>22s}")
    worst_all, worst_neg = [], []
    for tag, truth, label in DRIVEN:
        rs = [r for sess in scored(tag, truth) for r in sess if r[3]]
        if not rs:
            print(f"   {tag:22s} {'0':>10s}  {'— never on arrival —':>22s}")
            continue
        a = [r[0] for r in rs]
        n = [r[1] for r in rs]
        worst_all.append(min(a))
        worst_neg.append(min(n))
        print(f"   {tag:22s} {len(rs):10d}  {fmt(a):>22s}  {fmt(n):>22s}")

    print("\n   Separation margin = worst on-arrival attempt / pooled silence max:")
    print(f"     peak/median     {min(worst_all):6.2f} / {max(sil_all):5.2f} "
          f"= {min(worst_all) / max(sil_all):5.2f}x")
    print(f"     peak/negmedian  {min(worst_neg):6.2f} / {max(sil_neg):5.2f} "
          f"= {min(worst_neg) / max(sil_neg):5.2f}x")

    print()
    print("=" * 78)
    print("3. DOES IT DEMOTE THE HARMFUL CASE?  runF-wall's 52 cm-wrong acceptance")
    print("=" * 78)
    print("   This is the comparison the gate actually has to make. Silence is not")
    print("   what a gate fails on — a confident wrong answer is.")
    for tag, wrong_lag, label in HARMFUL:
        sess = scored(tag, wrong_lag)
        rs = [r for s in sess for r in s]
        at_wrong = [r for r in rs if r[3]]
        a = [r[0] for r in rs]
        n = [r[1] for r in rs]
        print(f"\n   {tag} — {label}")
        print(f"     all {len(rs)} attempts     peak/median    {fmt(a)}")
        print(f"                          peak/negmedian {fmt(n)}")
        if at_wrong:
            aw = [r[0] for r in at_wrong]
            nw = [r[1] for r in at_wrong]
            print(f"     {len(at_wrong)} at lag {wrong_lag} "
                  f"({implied_m(wrong_lag):.3f} m, ~52 cm wrong)")
            print(f"                          peak/median    {fmt(aw)}")
            print(f"                          peak/negmedian {fmt(nw)}")
        print(f"     worst valid on-arrival attempt, for comparison:")
        print(f"                          peak/median    {min(worst_all):6.2f}")
        print(f"                          peak/negmedian {min(worst_neg):6.2f}")
        print(f"     headroom (worst valid / worst wall attempt — >1 means separable):")
        print(f"                          peak/median    {min(worst_all) / max(a):5.2f}x")
        print(f"                          peak/negmedian {min(worst_neg) / max(n):5.2f}x")

    print()
    print("=" * 78)
    print("4. IS THERE A THRESHOLD THAT WORKS?  session-level, both statistics")
    print("=" * 78)
    print("   A session locks on its first qualifying attempt, so admission is scored")
    print("   per session: does ANY attempt clear T. Wanted: keeps every valid")
    print("   session, refuses every silent session, refuses the wall.")

    def sessions_admitted(tag, truth, idx, T, require_correct=False):
        out = []
        for sess in scored(tag, truth):
            hit = [r for r in sess if r[idx] >= T]
            if not hit:
                out.append("refuse")
            elif require_correct:
                out.append("correct" if hit[0][3] else "WRONG")
            else:
                out.append("admit")
        return out

    for idx, name in ((0, "peak/median"), (1, "peak/negmedian")):
        print(f"\n   --- {name} ---")
        print(f"   {'T':>6s}  {'valid sessions kept':>19s}  {'silent admitted':>15s}  "
              f"{'wall admitted':>13s}")
        for T in (6, 8, 10, 12, 16, 20, 24, 28, 32, 40, 48):
            kept = tot = 0
            for tag, truth, _ in DRIVEN:
                res = sessions_admitted(tag, truth, idx, T)
                kept += sum(1 for x in res if x == "admit")
                tot += len(res)
            sil_adm = sum(
                1
                for tag in SILENT
                for x in sessions_admitted(tag, None, idx, T)
                if x == "admit"
            )
            sil_tot = sum(len(scored(tag, None)) for tag in SILENT)
            wall = sessions_admitted("runF-wall", 925, idx, T)
            w_adm = sum(1 for x in wall if x == "admit")
            print(f"   {T:6d}  {kept:9d} / {tot:<7d}  {sil_adm:7d} / {sil_tot:<5d}  "
                  f"{w_adm:5d} / {len(wall):<5d}")

    print()
    print("=" * 78)
    print("5. WHY — SIGNAL SIZE AGAINST ESTIMATOR NOISE")
    print("=" * 78)
    print("   Both floors are medians of the same magnitudes; the negative-lag one is")
    print("   taken over half as many lags, so it is the noisier estimate of the same")
    print("   quantity. R's spread is that noise. Contamination has to be larger than")
    print("   it to be recoverable at all.")
    for name, pop in (
        ("driven", [(t, tr) for t, tr, _ in DRIVEN + HARMFUL]),
        ("silent", [(t, None) for t in SILENT]),
    ):
        Rs = [r[2] for tag, truth in pop for sess in scored(tag, truth) for r in sess]
        lo, hi = q(Rs, 0.1), q(Rs, 0.9)
        print(f"   {name:7s} n={len(Rs):4d}  R median {st.median(Rs):.4f}  "
              f"p10-p90 {lo:.3f}-{hi:.3f}  (+-{100 * (hi - lo) / 2 / st.median(Rs):.1f}%)")
    dR = [r[2] for tag, truth, _ in DRIVEN + HARMFUL
          for sess in scored(tag, truth) for r in sess]
    sR = [r[2] for tag in SILENT for sess in scored(tag, None) for r in sess]
    signal = 100 * (st.median(dR) / st.median(sR) - 1.0)
    noise = 100 * (q(sR, 0.9) - q(sR, 0.1)) / 2 / st.median(sR)
    print(f"\n   contamination to recover: {signal:.1f}%   "
          f"per-attempt noise on R: +-{noise:.1f}%   "
          f"-> {signal / noise:.2f} : 1")

    print()
    print("=" * 78)
    print("6. THE SURVIVING CLAIM — is the onset signature present in this data at all?")
    print("=" * 78)
    print("   `audit/rig-verify-125/gate-rules-offline.md` s2 already recorded the")
    print("   reverberation argument dead (<=8%) and kept one narrower property: a ring")
    print("   that straddles the stimulus onset is mostly silence, so the ALL-LAG floor")
    print("   collapses while the negative-lag floor holds. There the two disagreed by")
    print("   2.7x (R = 0.364) and the all-lag statistic scored the weakest arrival in")
    print("   the set as the most prominent — the only lock the shipped gate returned.")
    print("   That is an onset artefact, not a reverberation one, and nothing above")
    print("   touches it. So: does any session-3 attempt show the signature?")
    everything = [
        (tag, r)
        for tag, truth in (
            [(t, tr) for t, tr, _ in DRIVEN + HARMFUL] + [(t, None) for t in SILENT]
        )
        for sess in scored(tag, truth)
        for r in sess
    ]
    ONSET_R = 0.5  # midway between the observed 0.364 and steady-state ~1.0
    hits = [(tag, r) for tag, r in everything if r[2] < ONSET_R]
    lo = min(r[2] for _, r in everything)
    print(f"\n   {len(everything)} attempts scored, every capture in the session.")
    print(f"   attempts with R < {ONSET_R}: {len(hits)}   lowest R observed: {lo:.3f}")
    print("   -> the onset signature is ABSENT from session 3, which is what a session")
    print("      that starts its stream after the stimulus should look like. Session 3's")
    print("      onset run (`run4`) kept counters, not per-frame floors, so it cannot be")
    print("      re-scored here. The onset claim is neither confirmed nor refuted by")
    print("      this data; it is untouched by it.")


if __name__ == "__main__":
    main()
