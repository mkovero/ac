"""Headless client for the ac daemon over an SSH tunnel.

CTRL 15556 -> rig 127.0.0.1:5556 (REQ/REP)
DATA 15557 -> rig 127.0.0.1:5557 (PUB/SUB, single-part "<topic> <json>")

Emission safety: every helper that starts a stimulus goes through `drive_on`
or `pink_on`, both of which refuse a level above LEVEL_CEILING_DBFS. The
daemon also clamps server-side via drive_max_dbfs in ~/rig2-home.
"""

import json
import time

import zmq

CTRL = "tcp://127.0.0.1:15556"
DATA = "tcp://127.0.0.1:15557"

# Operator-authorised ceiling for this session. Nothing here may exceed it.
LEVEL_CEILING_DBFS = -30.0

_ctx = zmq.Context.instance()


def req(cmd, timeout_ms=15000, **kw):
    """One REQ/REP round trip. Fresh socket per call: REQ is strictly
    lockstep, and a timed-out socket cannot be reused."""
    s = _ctx.socket(zmq.REQ)
    s.setsockopt(zmq.LINGER, 0)
    s.setsockopt(zmq.RCVTIMEO, timeout_ms)
    s.connect(CTRL)
    try:
        s.send_json({"cmd": cmd, **kw})
        return s.recv_json()
    finally:
        s.close()


def sub(topics=("data",)):
    """SUB socket. Subscribe before the producer starts or frames are lost.

    The wire topic is `data` for every measurement frame; what kind of frame
    it is lives in the payload's `type` (transfer_stream, visualize/ir, ...).
    Subscribing on `transfer_stream` matches nothing."""
    s = _ctx.socket(zmq.SUB)
    s.setsockopt(zmq.LINGER, 0)
    s.setsockopt(zmq.RCVHWM, 0)
    for t in topics:
        s.setsockopt_string(zmq.SUBSCRIBE, t)
    s.connect(DATA)
    time.sleep(0.3)  # let the subscription reach the publisher
    return s


def frames(s, seconds, want="transfer_stream"):
    """Collect payloads whose `type` is `want`, for `seconds`."""
    out = []
    deadline = time.time() + seconds
    poller = zmq.Poller()
    poller.register(s, zmq.POLLIN)
    while time.time() < deadline:
        remaining = max(0, int((deadline - time.time()) * 1000))
        if not poller.poll(min(remaining, 500)):
            continue
        raw = s.recv_string()
        _topic, _, payload = raw.partition(" ")
        try:
            msg = json.loads(payload)
        except json.JSONDecodeError:
            continue
        if msg.get("type") != want:
            continue
        msg["_t"] = time.time()
        out.append(msg)
    return out


def check_level(level_dbfs):
    if level_dbfs > LEVEL_CEILING_DBFS:
        raise ValueError(
            f"level {level_dbfs} dBFS exceeds the authorised ceiling "
            f"{LEVEL_CEILING_DBFS} dBFS"
        )


def pink_on(level_dbfs, channels):
    """Standalone pink worker. Must be started BEFORE a transfer session so
    the correlation rings never fill against silence (issue #226)."""
    check_level(level_dbfs)
    return req("generate_pink", level_dbfs=level_dbfs, channels=channels)


def stop(name=None):
    """Stop one worker by name, or every worker when name is None. The reply
    is synchronous with respect to the busy guard — the handler joins the
    worker threads before replying."""
    return req("stop") if name is None else req("stop", name=name)


def status():
    return req("status")


def summarise(f):
    """The fields the handoff asks to record, per frame."""
    ev = f.get("delay_evidence") or {}
    return {
        "delay_ms": f.get("delay_ms"),
        "delay_samples": f.get("delay_samples"),
        "delay_locked": f.get("delay_locked"),
        "prominence": ev.get("prominence"),
        "peak_lag": ev.get("peak_lag"),
        "peak_value": ev.get("peak_value"),
        "noncausal_peak_lag": ev.get("noncausal_peak_lag"),
        "noncausal_peak_value": ev.get("noncausal_peak_value"),
        "median_value": ev.get("median_value"),
        "negative_lag_median": ev.get("negative_lag_median"),
        "n_candidates": len(ev.get("candidates") or []),
        "meas_peak_dbfs": f.get("meas_peak_dbfs"),
        "ref_peak_dbfs": f.get("ref_peak_dbfs"),
        "sr": f.get("sr"),
    }
