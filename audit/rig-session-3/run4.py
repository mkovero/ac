"""Run 4 — CHECK ROUTING on genuinely unrelated legs.

Mirrors ac-scene/src/fault.rs as it stands on 4659b25 (#239):

    COHERENCE_THRESHOLD      = 0.5
    COHERENCE_ALIVE_FRACTION = 0.10
    coherence_dead(c) = !c.is_empty() && alive(c) < 0.10 * len(c)
    settled  = mtw present and lengths agree
    refusing = (settled OR estimator_attempted) && !delay_locked

Precedence is level -> lock -> coherence, so a refusal outranks CheckRouting.
The handoff expects this not to fire: the ladder needs a lock, unrelated legs
refuse, so there is nothing to evaluate. Confirm and move on.

Condition A: pink on playback_2 only — the electrical reference leg is hot,
the speakers are silent, the mic hears the room. Unrelated content on the two
legs, no emission into the speakers at all.
"""

import json
import pickle
import statistics as st

import rig
import s3

COHERENCE_THRESHOLD = 0.5
COHERENCE_ALIVE_FRACTION = 0.10
LEVEL = -30.0
CAPTURE_S = 20.0


def cols(f):
    m = f.get("mtw")
    if not m:
        return None
    return m.get("coherence") or []


def dead(c):
    if not c:
        return False
    alive = sum(1 for x in c if x is not None and x >= COHERENCE_THRESHOLD)
    return alive < COHERENCE_ALIVE_FRACTION * len(c)


def score(tag, fs):
    acou = [f for f in fs if f"{f.get('meas_channel')}-{f.get('ref_channel')}" == s3.ACOU]
    with_mtw = [f for f in acou if cols(f) is not None]
    locked = sum(1 for f in acou if f.get("delay_locked"))
    attempted = sum(1 for f in acou if (f.get("delay_attempts") or 0) > 0)
    alive_frac, n_cols = [], []
    for f in with_mtw:
        c = [x for x in cols(f) if x is not None]
        if not c:
            continue
        n_cols.append(len(c))
        alive_frac.append(sum(1 for x in c if x >= COHERENCE_THRESHOLD) / len(c))
    would_fire = sum(1 for f in with_mtw if dead([x for x in cols(f) if x is not None]))
    print(
        f"  {tag}: frames {len(acou)}  locked {locked}  attempted {attempted}  "
        f"with ladder {len(with_mtw)}",
        flush=True,
    )
    if alive_frac:
        print(
            f"    columns {st.median(n_cols):.0f}  alive fraction "
            f"min/med/max {min(alive_frac):.3f}/{st.median(alive_frac):.3f}/"
            f"{max(alive_frac):.3f}  coherence_dead frames {would_fire}",
            flush=True,
        )
    else:
        print("    no ladder columns at all — nothing for CHECK ROUTING to read",
              flush=True)
    return {
        "tag": tag,
        "frames": len(acou),
        "locked": locked,
        "attempted": attempted,
        "with_ladder": len(with_mtw),
        "alive_fraction": alive_frac,
        "n_cols": n_cols,
        "coherence_dead_frames": would_fire,
    }


def condition(tag, channels):
    rig.stop()
    r = rig.pink_on(LEVEL, channels)
    assert r.get("ok"), r
    import time

    time.sleep(2.0)
    s = rig.sub()
    r = rig.req("transfer_stream", pairs=s3.PAIRS)
    assert r.get("ok"), r
    fs = rig.frames(s, CAPTURE_S)
    s.close()
    rig.stop()
    return fs


if __name__ == "__main__":
    out = {}
    try:
        for tag, ch in (("A-unrelated (ref leg only)", [1]),
                        ("B-healthy (ref + speaker A)", [1, 4])):
            fs = condition(tag, ch)
            out[tag] = score(tag, fs)
            out[tag]["rows"] = [s3.row(f) for f in fs]
    finally:
        rig.stop()
        with open("run4.pkl", "wb") as fh:
            pickle.dump(out, fh)
        with open("run4.json", "w") as fh:
            json.dump({k: {kk: vv for kk, vv in v.items() if kk != "rows"}
                       for k, v in out.items()}, fh)
    print("  saved run4.pkl", flush=True)
