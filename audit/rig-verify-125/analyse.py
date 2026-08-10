"""Run 2 (evidence completeness) and Run 3 (the two numbers) over run1.pkl.

Run 2 is the property session 2 could not satisfy: the accepted lag is present
in the capture's own candidate list, so the decision can be reproduced offline.
Checked on locked frames; the structural invariants are checked on every frame.

Run 3 records prominence in the NEW definition (measured against the strongest
causal peak) as a fresh baseline — session 2's 13.6-27.8 is a different
statistic and is not comparable — and negative_lag_median beside median_value.
"""

import pickle
import statistics as st

SR = 96000


def ms(lag):
    return lag / SR * 1000.0


sessions = pickle.load(open("run1.pkl", "rb"))
print(f"sessions: {len(sessions)}\n")

# ---------- Run 2: evidence completeness ----------
tot = locked_n = 0
miss_accepted = miss_peak = miss_nc = 0
dup = unordered = over_cap = 0
dup_peak = dup_nc = 0
counts = []

for fs in sessions:
    for f in fs:
        ev = f.get("delay_evidence") or {}
        cand = ev.get("candidates") or []
        lags = [c["lag"] for c in cand]
        tot += 1
        counts.append(len(lags))
        if lags != sorted(lags):
            unordered += 1
        if len(set(lags)) != len(lags):
            dup += 1
        if len(lags) > 35:
            over_cap += 1
        if ev.get("peak_lag") not in lags:
            miss_peak += 1
        if ev.get("noncausal_peak_lag") not in lags:
            miss_nc += 1
        if lags.count(ev.get("peak_lag")) > 1:
            dup_peak += 1
        if lags.count(ev.get("noncausal_peak_lag")) > 1:
            dup_nc += 1
        if f.get("delay_locked"):
            locked_n += 1
            if f.get("delay_samples") not in lags:
                miss_accepted += 1

print("== Run 2 — evidence completeness ==")
print(f"frames {tot}, locked {locked_n}")
print(f"candidate count min/median/max: {min(counts)} / "
      f"{sorted(counts)[len(counts)//2]} / {max(counts)}")
print(f"over the 35 cap          : {over_cap}")
print(f"not strictly ascending   : {unordered}")
print(f"duplicate lags           : {dup}")
print(f"accepted lag missing     : {miss_accepted}  (of {locked_n} locked)")
print(f"peak_lag missing         : {miss_peak}")
print(f"noncausal_peak missing   : {miss_nc}")
print(f"peak_lag not exactly once: {dup_peak}")
print(f"noncausal not exactly once: {dup_nc}")

# ---------- Run 3: the two numbers ----------
print("\n== Run 3 — prominence (new definition) and the two floors ==")
print(f"{'sess':>4} {'frames':>6} {'lock':>5} {'peak_lag':>9} {'ms':>8} "
      f"{'prom':>7} {'prom_neg':>8} {'med_all':>9} {'med_neg':>9} "
      f"{'ratio':>6} {'mic dBFS':>9}")

all_prom, all_prom_neg, all_ratio = [], [], []
for i, fs in enumerate(sessions, 1):
    ev = [f.get("delay_evidence") or {} for f in fs]
    pl = st.median([e.get("peak_lag", 0) for e in ev])
    pv = st.median([e.get("peak_value", 0.0) for e in ev])
    mv = st.median([e.get("median_value", 0.0) for e in ev])
    nl = st.median([e.get("negative_lag_median", 0.0) for e in ev])
    prom = st.median([e.get("prominence", 0.0) for e in ev])
    prom_neg = pv / nl if nl else float("nan")
    ratio = mv / nl if nl else float("nan")
    mic = st.median([f["meas_peak_dbfs"] for f in fs if f.get("meas_peak_dbfs")])
    nlock = sum(1 for f in fs if f.get("delay_locked"))
    all_prom.append(prom)
    all_prom_neg.append(prom_neg)
    all_ratio.append(ratio)
    print(f"{i:>4} {len(fs):>6} {nlock:>5} {pl:>9.0f} {ms(pl):>8.3f} "
          f"{prom:>7.2f} {prom_neg:>8.2f} {mv:>9.5f} {nl:>9.5f} "
          f"{ratio:>6.3f} {mic:>9.1f}")

print(f"\nprominence, all-lag floor  (new defn): "
      f"min {min(all_prom):.2f}  median {st.median(all_prom):.2f}  "
      f"max {max(all_prom):.2f}")
print(f"prominence, negative-lag floor       : "
      f"min {min(all_prom_neg):.2f}  median {st.median(all_prom_neg):.2f}  "
      f"max {max(all_prom_neg):.2f}")
print(f"median_all / median_neg              : "
      f"min {min(all_ratio):.3f}  median {st.median(all_ratio):.3f}  "
      f"max {max(all_ratio):.3f}")

# ---------- locked-frame detail ----------
print("\n== Locks, if any ==")
any_lock = False
for i, fs in enumerate(sessions, 1):
    lk = [f for f in fs if f.get("delay_locked")]
    if not lk:
        continue
    any_lock = True
    d = [f["delay_ms"] for f in lk]
    s = [f["delay_samples"] for f in lk]
    print(f"session {i}: {len(lk)}/{len(fs)} locked  "
          f"delay_ms {min(d):.3f}..{max(d):.3f} median {st.median(d):.3f}  "
          f"samples {min(s)}..{max(s)}")
if not any_lock:
    print("no locks in any session")
