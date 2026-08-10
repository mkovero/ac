"""The onset case — the one thing in the queue that needs the rig to answer.

Session 2's -826 ms lock happened because `transfer_stream` was started
BEFORE the stimulus, so the correlation ring straddled the silence->signal
transition. The daemon-side onset guard was written and then dropped: no
synthetic ring could be built where the causal-only search still returned a
wrong answer, and the guard would have fired forever on a gated stimulus.

So this reverses Run 1's ordering deliberately: session first, stimulus after.

Pass: the first lock lands at a plausible positive lag once the stimulus is
      running.
Fail: a confident wrong lock at a positive lag — the one case causality
      cannot catch.
Uninformative: a refusal throughout, which at this position is the base rate.
"""

import pickle
import statistics as st
import time

import rig

LEVEL = -30.0
CHANNELS = [1, 4]
MEAS, REF = 0, 3
SILENT_S = 6.0  # rings fill against silence first — this is the whole point
DRIVEN_S = 20.0
N = 4

runs = []
try:
    for i in range(1, N + 1):
        rig.stop()
        time.sleep(0.5)

        s = rig.sub()
        r = rig.req("transfer_stream", meas_channel=MEAS, ref_channel=REF)
        if not r.get("ok"):
            print(f"run {i}: transfer failed: {r}", flush=True)
            s.close()
            break

        # Phase 1: session running, nothing driven.
        pre = rig.frames(s, SILENT_S)

        # Phase 2: stimulus arrives mid-session.
        t_on = time.time()
        rp = rig.pink_on(LEVEL, CHANNELS)
        if not rp.get("ok"):
            print(f"run {i}: pink failed: {rp}", flush=True)
            rig.stop()
            s.close()
            break
        post = rig.frames(s, DRIVEN_S)

        s.close()
        rig.stop()
        time.sleep(1.0)

        runs.append({"pre": pre, "post": post, "t_on": t_on})

        lk_pre = [f for f in pre if f.get("delay_locked")]
        lk_post = [f for f in post if f.get("delay_locked")]
        print(
            f"run {i}: silent {len(pre)} frames ({len(lk_pre)} locked)  "
            f"driven {len(post)} frames ({len(lk_post)} locked)",
            flush=True,
        )
        if lk_pre:
            d = [f["delay_ms"] for f in lk_pre]
            print(
                f"   LOCKED WHILE SILENT: delay_ms {min(d):.3f}..{max(d):.3f}",
                flush=True,
            )
        if lk_post:
            first = lk_post[0]
            d = [f["delay_ms"] for f in lk_post]
            print(
                f"   first lock after onset: {first['delay_ms']:.3f} ms "
                f"(lag {first['delay_samples']})  "
                f"t+{first['_t'] - t_on:.1f}s   "
                f"range {min(d):.3f}..{max(d):.3f}",
                flush=True,
            )
        if not lk_pre and not lk_post:
            pr = [(f.get("delay_evidence") or {}).get("prominence", 0.0)
                  for f in post]
            pl = [(f.get("delay_evidence") or {}).get("peak_lag", 0)
                  for f in post]
            print(
                f"   refused throughout: prom median {st.median(pr):.2f}  "
                f"peak_lag median {st.median(pl):.0f} "
                f"({st.median(pl)/96000*1000:.3f} ms)",
                flush=True,
            )
finally:
    rig.stop()
    pickle.dump(runs, open("onset.pkl", "wb"))
    print(f"saved {len(runs)} runs to onset.pkl", flush=True)
