#!/usr/bin/env bash
# Profile ac-daemon under samply with a repeatable synthetic workload.
#
# Usage:
#   scripts/profile-samply.sh [duration_sec]
#
# Env overrides:
#   FFT_N=16384 CHANNELS=8 MODE=cwt scripts/profile-samply.sh 30
#
# Output: /tmp/ac-profile-<timestamp>.json.gz
# View with: samply load <file>   (opens profiler.firefox.com)

set -euo pipefail

DURATION="${1:-20}"
FFT_N="${FFT_N:-16384}"
CHANNELS="${CHANNELS:-4}"            # 1,2,4,8 — monitor on N channels
MODE="${MODE:-cwt}"                  # fft | cwt — cwt is heavier
CTRL_PORT="${CTRL_PORT:-5560}"       # off-default so we don't clash with a running daemon
DATA_PORT="${DATA_PORT:-5561}"

repo_root=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo_root"

if ! command -v samply >/dev/null; then
    echo "samply not found — install with: cargo install samply" >&2
    exit 1
fi

paranoid=$(cat /proc/sys/kernel/perf_event_paranoid 2>/dev/null || echo 2)
if [[ "$paranoid" -gt 1 ]]; then
    cat >&2 <<EOF
perf_event_paranoid=$paranoid — samply needs <= 1 for non-root profiling.
Fix now:     echo 1 | sudo tee /proc/sys/kernel/perf_event_paranoid
Persist:     echo 'kernel.perf_event_paranoid = 1' | sudo tee /etc/sysctl.d/99-perf.conf
EOF
    exit 1
fi

if ! python3 -c "import zmq" 2>/dev/null; then
    echo "pyzmq not found — install with: pip install pyzmq" >&2
    exit 1
fi

echo ">> building ac-daemon (profile=profiling)..."
(cd ac-rs && cargo build --profile profiling -p ac-daemon)

daemon_bin="ac-rs/target/profiling/ac-daemon"
out="/tmp/ac-profile-$(date +%Y%m%d-%H%M%S).json.gz"

cleanup() {
    if [[ -n "${samply_pid:-}" ]] && kill -0 "$samply_pid" 2>/dev/null; then
        echo ">> stopping samply (pid=$samply_pid)"
        kill -TERM "$samply_pid" 2>/dev/null || true
        wait "$samply_pid" 2>/dev/null || true
    fi
    if [[ -n "${daemon_pid:-}" ]] && kill -0 "$daemon_pid" 2>/dev/null; then
        echo ">> stopping daemon (pid=$daemon_pid)"
        kill -TERM "$daemon_pid" 2>/dev/null || true
        wait "$daemon_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

echo ">> launching daemon (will attach samply by PID)..."
# Start daemon standalone first. Attaching samply by --pid makes it synthesize
# PERF_RECORD_MMAP events from /proc/<pid>/maps, which preserves library
# mappings. Launching under `samply record -- <bin>` misses the initial exec
# mmaps and leaves libs=[] in the profile.
"$daemon_bin" --local --fake-audio \
    --ctrl-port "$CTRL_PORT" --data-port "$DATA_PORT" \
    >/tmp/ac-profile-daemon.log 2>&1 &
daemon_pid=$!

# Wait for CTRL socket to answer `status` before attaching samply.
python3 - "$CTRL_PORT" <<'PY'
import sys, time, zmq
port = int(sys.argv[1])
ctx = zmq.Context.instance()
for _ in range(50):
    s = ctx.socket(zmq.REQ)
    s.setsockopt(zmq.LINGER, 0)
    s.setsockopt(zmq.RCVTIMEO, 200)
    s.connect(f"tcp://127.0.0.1:{port}")
    s.send_json({"cmd": "status"})
    try:
        s.recv_json()
        print("daemon ready")
        sys.exit(0)
    except zmq.error.Again:
        s.close()
        time.sleep(0.1)
print("daemon did not come up", file=sys.stderr)
sys.exit(1)
PY

echo ">> attaching samply to pid=$daemon_pid → $out"
samply record --no-open -o "$out" --pid "$daemon_pid" \
    >/tmp/ac-samply.log 2>&1 &
samply_pid=$!
sleep 0.5   # give samply a moment to set up perf_event_open

echo ">> driving workload: mode=$MODE channels=$CHANNELS fft_n=$FFT_N duration=${DURATION}s"
python3 - "$CTRL_PORT" "$DATA_PORT" "$MODE" "$CHANNELS" "$FFT_N" "$DURATION" <<'PY'
import sys, time, threading, zmq

ctrl_port, data_port, mode, nch, fft_n, duration = sys.argv[1:]
nch, fft_n, duration = int(nch), int(fft_n), float(duration)

ctx = zmq.Context.instance()

def req(cmd):
    s = ctx.socket(zmq.REQ)
    s.setsockopt(zmq.LINGER, 0)
    s.setsockopt(zmq.RCVTIMEO, 3000)
    s.connect(f"tcp://127.0.0.1:{ctrl_port}")
    s.send_json(cmd)
    return s.recv_json()

# Subscriber — drains DATA so PUB HWM doesn't stall the worker.
stop = threading.Event()
def drain():
    sub = ctx.socket(zmq.SUB)
    sub.setsockopt(zmq.SUBSCRIBE, b"")
    sub.setsockopt(zmq.RCVTIMEO, 100)
    sub.connect(f"tcp://127.0.0.1:{data_port}")
    n = 0
    while not stop.is_set():
        try:
            sub.recv(); n += 1
        except zmq.error.Again:
            pass
    print(f"sub drained {n} frames")
threading.Thread(target=drain, daemon=True).start()

print("set_analysis_mode:", req({"cmd": "set_analysis_mode", "mode": mode, "sigma": 12.0, "n_scales": 256}))
channels = list(range(nch))
print("monitor_spectrum:", req({
    "cmd":      "monitor_spectrum",
    "channels": channels,
    "freq_hz":  1000.0,
    "interval": 0.05,
    "fft_n":    fft_n,
}))

t0 = time.time()
while time.time() - t0 < duration:
    time.sleep(0.5)

print("stop:", req({"cmd": "stop"}))
stop.set()
time.sleep(0.2)
print("quit:", req({"cmd": "quit"}))
PY

# Daemon has quit. In --save-only (-s) mode samply writes the gzip profile
# and exits on its own. Wait up to 20s for that — gzipping can take a bit.
# Only signal if it's clearly stuck (e.g. still serving a local UI).
echo ">> waiting for samply to finalize profile..."
for i in $(seq 1 20); do
    kill -0 "$samply_pid" 2>/dev/null || break
    sleep 1
done
if kill -0 "$samply_pid" 2>/dev/null; then
    echo ">> samply still running after 20s — signalling"
    kill -TERM "$samply_pid" 2>/dev/null || true
    sleep 2
    kill -KILL "$samply_pid" 2>/dev/null || true
fi
wait "$samply_pid" 2>/dev/null || true
unset samply_pid

echo
if [[ -s "$out" ]]; then
    echo ">> profile written: $out ($(stat -c%s "$out") bytes)"
    echo ">> view: samply load $out"
else
    echo "!! profile missing or empty — check /tmp/ac-profile-daemon.log and samply flags" >&2
    exit 1
fi
