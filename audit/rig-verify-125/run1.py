"""Run 1 — the wrong-lock fix, at the fixed 3 m on-axis position.

One fresh transfer session per attempt: the per-pair delay is estimated once
when the rings first fill and cached for the life of the session, so a repeat
inside one session measures nothing new.

The stimulus is a standalone generate_pink worker started BEFORE the session
(issue #226) on channels [1, 4] = playback_2 (AN2, the reference loopback leg)
and playback_5 (the speaker leg). Emission is stopped between sessions.

Pass: every lock that occurs is at 11.34 ms +/- a sample or two.
Fail: any lock at 14.00 ms, 18.43 ms, or anything inconsistent with 3 m.
Refusals are an acceptable outcome and are not counted against the branch.
"""

import pickle
import statistics as st
import sys
import time

import rig

LEVEL = -30.0
CHANNELS = [1, 4]  # playback_2 (ref loopback) + playback_5 (speaker)
MEAS, REF = 0, 3  # capture_1 (mic), capture_4 (IN4)
CAPTURE_S = 15.0
SETTLE_S = 2.0  # stimulus flowing before the session opens its rings
N = int(sys.argv[1]) if len(sys.argv) > 1 else 8

sessions = []
try:
    for i in range(1, N + 1):
        rig.stop()  # nothing left over from the previous attempt
        time.sleep(0.5)

        r = rig.pink_on(LEVEL, CHANNELS)
        if not r.get("ok"):
            print(f"session {i}: pink failed: {r}", flush=True)
            break
        time.sleep(SETTLE_S)

        s = rig.sub()
        r = rig.req("transfer_stream", meas_channel=MEAS, ref_channel=REF)
        if not r.get("ok"):
            print(f"session {i}: transfer failed: {r}", flush=True)
            rig.stop()
            s.close()
            break

        fs = rig.frames(s, CAPTURE_S)
        s.close()
        rig.stop()  # emission off between runs
        time.sleep(1.0)

        locked = [f for f in fs if f.get("delay_locked")]
        sessions.append(fs)
        if locked:
            d = [f["delay_ms"] for f in locked]
            ev = locked[0].get("delay_evidence") or {}
            print(
                f"session {i}: {len(locked)}/{len(fs)} locked  "
                f"delay_ms median={st.median(d):.3f} "
                f"min={min(d):.3f} max={max(d):.3f}  "
                f"prom={ev.get('prominence'):.2f} "
                f"peak_lag={ev.get('peak_lag')} "
                f"nc_lag={ev.get('noncausal_peak_lag')}",
                flush=True,
            )
        else:
            proms = [
                (f.get("delay_evidence") or {}).get("prominence", 0.0) for f in fs
            ]
            mic = [f["meas_peak_dbfs"] for f in fs if f.get("meas_peak_dbfs")]
            print(
                f"session {i}: REFUSED 0/{len(fs)}  "
                f"prom median={st.median(proms):.2f} max={max(proms):.2f}  "
                f"mic median={st.median(mic):.1f} dBFS",
                flush=True,
            )
finally:
    rig.stop()  # never leave the rig emitting
    pickle.dump(sessions, open("run1.pkl", "wb"))
    print(f"saved {len(sessions)} sessions to run1.pkl", flush=True)
