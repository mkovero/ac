"""What the estimator did across the silence->signal transition.

Session 2's -826 ms lock came from exactly this ordering. The question is
whether the causal-only search keeps the peak positive and plausible while
the correlation ring is still half full of silence.
"""

import pickle
import statistics as st

SR = 96000
runs = pickle.load(open("onset.pkl", "rb"))

for i, r in enumerate(runs, 1):
    pre, post, t_on = r["pre"], r["post"], r["t_on"]
    print(f"\n=== run {i} ===")

    def col(fs, key):
        return [(f.get("delay_evidence") or {}).get(key) for f in fs]

    for name, fs in (("silent", pre), ("driven", post)):
        if not fs:
            print(f"{name}: no frames")
            continue
        pl = [x for x in col(fs, "peak_lag") if x is not None]
        nc = [x for x in col(fs, "noncausal_peak_lag") if x is not None]
        pv = [x for x in col(fs, "peak_value") if x is not None]
        ncv = [x for x in col(fs, "noncausal_peak_value") if x is not None]
        neg = sum(1 for x in pl if x < 0)
        print(
            f"{name:7s} n={len(fs):4d}  "
            f"peak_lag median {st.median(pl):8.0f} ({st.median(pl)/SR*1000:8.3f} ms) "
            f"min {min(pl):8d} max {max(pl):8d}  "
            f"negative peak_lag frames: {neg}"
        )
        print(
            f"{'':7s}          "
            f"peak_value median {st.median(pv):.5f}  "
            f"noncausal_value median {st.median(ncv):.5f}  "
            f"noncausal>causal in {sum(1 for a, b in zip(ncv, pv) if a > b)} frames"
        )

    # first 12 driven frames — the transition itself
    print("  first driven frames (the transition):")
    for f in post[:12]:
        ev = f.get("delay_evidence") or {}
        print(
            f"    t+{f['_t']-t_on:5.2f}s  locked={str(f.get('delay_locked')):5s} "
            f"delay_ms={f.get('delay_ms'):9.3f}  "
            f"peak_lag={ev.get('peak_lag'):7d} ({ev.get('peak_lag')/SR*1000:8.3f} ms)  "
            f"prom={ev.get('prominence'):6.2f}  "
            f"nc_lag={ev.get('noncausal_peak_lag'):8d}  "
            f"mic={f.get('meas_peak_dbfs'):6.1f}"
        )

print("\n=== overall ===")
allpre = [f for r in runs for f in r["pre"]]
allpost = [f for r in runs for f in r["post"]]
print(f"silent frames total : {len(allpre)}, locked {sum(1 for f in allpre if f.get('delay_locked'))}")
print(f"driven frames total : {len(allpost)}, locked {sum(1 for f in allpost if f.get('delay_locked'))}")
neg_locks = [
    f for f in allpre + allpost
    if f.get("delay_locked") and (f.get("delay_ms") or 0) < 0
]
print(f"NEGATIVE LOCKS      : {len(neg_locks)}   <- session 2 had one at -826.35 ms")
