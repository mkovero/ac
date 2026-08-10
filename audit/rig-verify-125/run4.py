"""Run 4 — the fault indicator.

Mirrors ac-scene/src/fault.rs so the states can be scored headlessly:

    COHERENCE_THRESHOLD      = 0.5
    COHERENCE_ALIVE_FRACTION = 0.10
    coherence_dead(c) = !c.is_empty() && alive(c) < 0.10 * c.len()
    settled  = frame.mtw.is_some()
    refusing = settled && delay_locked == false

Precedence in FaultState::update: level -> lock -> coherence, so a refusal
outranks CheckRouting. The coherence columns come from `mtw`.

Two conditions:

  A. unrelated legs — pink on playback_2 only (the AN2 electrical loopback,
     which is the reference leg). The mic hears room noise. Nothing is driven
     into the speakers. This is session 2's induction, which put 22 of 504
     columns over the mask and left the banner dark under the old
     all-columns rule.

  B. healthy — pink on playback_2 and playback_5, the Run 1 configuration.
     A healthy measurement must never read CHECK ROUTING.
"""

import pickle
import time

import rig

LEVEL = -30.0
MEAS, REF = 0, 3
CAPTURE_S = 20.0

COHERENCE_THRESHOLD = 0.5
COHERENCE_ALIVE_FRACTION = 0.10


def coherence_of(f):
    m = f.get("mtw")
    if not m:
        return None
    return m.get("coherence") or []


def coherence_dead(c):
    if not c:
        return False
    alive = sum(1 for x in c if x >= COHERENCE_THRESHOLD)
    return alive < COHERENCE_ALIVE_FRACTION * len(c)


def score(name, fs):
    n = len(fs)
    with_mtw = [f for f in fs if coherence_of(f) is not None]
    locked = sum(1 for f in fs if f.get("delay_locked"))
    # settled == mtw present; refusing == settled and not locked
    refusing = sum(
        1 for f in fs if coherence_of(f) is not None and not f.get("delay_locked")
    )
    dead = sum(1 for f in with_mtw if coherence_dead(coherence_of(f)))
    print(f"\n-- {name} --")
    print(f"frames                     : {n}")
    print(f"frames with mtw (settled)  : {len(with_mtw)}")
    print(f"locked frames              : {locked}")
    print(f"refusing (settled & !lock) : {refusing}   <- gates LOST LOCK / NO LOCK")
    if with_mtw:
        cols = [len(coherence_of(f)) for f in with_mtw]
        alives = [
            sum(1 for x in coherence_of(f) if x >= COHERENCE_THRESHOLD)
            for f in with_mtw
        ]
        fracs = [a / c for a, c in zip(alives, cols) if c]
        fracs.sort()
        print(f"columns per frame          : {min(cols)}..{max(cols)}")
        print(
            f"alive fraction             : min {min(fracs):.3f} "
            f"median {fracs[len(fracs)//2]:.3f} max {max(fracs):.3f} "
            f"(gate {COHERENCE_ALIVE_FRACTION})"
        )
        print(f"CHECK ROUTING frames       : {dead} / {len(with_mtw)}")
    else:
        print("no mtw on any frame -> coherence slice empty ->")
        print("  coherence_dead() returns false -> CHECK ROUTING cannot fire")
    return fs


out = {}
try:
    # ---- A: unrelated legs (speaker silent) ----
    rig.stop()
    time.sleep(0.5)
    r = rig.pink_on(LEVEL, [1])  # playback_2 only — ref leg, no speaker
    assert r.get("ok"), r
    time.sleep(2.0)
    s = rig.sub()
    r = rig.req("transfer_stream", meas_channel=MEAS, ref_channel=REF)
    assert r.get("ok"), r
    fs = rig.frames(s, CAPTURE_S)
    s.close()
    rig.stop()
    out["unrelated"] = score("A. unrelated legs (ref=pink, meas=room)", fs)
    time.sleep(1.0)

    # ---- B: healthy ----
    r = rig.pink_on(LEVEL, [1, 4])
    assert r.get("ok"), r
    time.sleep(2.0)
    s = rig.sub()
    r = rig.req("transfer_stream", meas_channel=MEAS, ref_channel=REF)
    assert r.get("ok"), r
    fs = rig.frames(s, CAPTURE_S)
    s.close()
    rig.stop()
    out["healthy"] = score("B. healthy (both legs from one pink source)", fs)
finally:
    rig.stop()
    pickle.dump(out, open("run4.pkl", "wb"))
    print("\nsaved run4.pkl", flush=True)
