# Profiling

Performance reference for the `ac` Rust stack: how to profile, current
per-tick latency for the most obvious user paths, and where the next
measurable wins are.

## Tooling

| Tool | Use |
|------|-----|
| `samply` | Sampling profiler. Produces a Firefox Profiler JSON and opens `profiler.firefox.com`. |
| `cargo bench` / `cargo run --example bench_*` | Micro-benches for Tier 1 / Tier 2 hot paths. |
| `scripts/profile-samply.sh` | Full daemon profile with synthetic workload over ZMQ. |
| `scripts/profile-ui-samply.sh` | Full ac-ui profile with synthetic backend. |

### One-time host setup

```
echo 1 | sudo tee /proc/sys/kernel/perf_event_paranoid
# persist:
echo 'kernel.perf_event_paranoid = 1' | sudo tee /etc/sysctl.d/99-perf.conf
```

### Profiling the daemon / UI

```
scripts/profile-samply.sh 20
scripts/profile-ui-samply.sh 20
samply load /tmp/ac-profile-<ts>.json.gz
```

Env knobs on `profile-samply.sh`: `FFT_N=16384`, `CHANNELS=8`, `MODE=cwt|fft`,
`BACKEND=fake|jack`. The script starts the daemon standalone and attaches
samply via `--pid` — launching `samply record -- <bin>` misses the initial
exec `MMAP` events and leaves `libs=[]` in the profile, which breaks
symbolication. Same trap if you profile an `examples/bench_*` binary by
hand — prefer the `--pid` attach pattern.

### Profiling a library micro-bench

`crates/ac-core/examples/bench_cwt.rs` and `bench_analyze.rs` honor
`AC_BENCH_ITERS` so the binary runs long enough for meaningful samples.

```
cd ac-rs
cargo build --profile profiling -p ac-core --example bench_cwt
AC_BENCH_ITERS=80000 target/profiling/examples/bench_cwt >/tmp/bench.log 2>&1 &
bench_pid=$!
samply record --no-open -o /tmp/cwt.json.gz --pid "$bench_pid"
wait "$bench_pid"
# Ctrl-C samply (or kill -TERM) to finalize the file.
samply load /tmp/cwt.json.gz
```

`--profile profiling` inherits from release, adds `debug = line-tables-only`
and `strip = false` so `addr2line -f -C -i <bin> 0xADDR` can resolve inline
chains offline. Handy when you want leaf-function breakdown without the
live Firefox UI.

## Tier 1 — `measurement::thd::analyze` (reference FFT + THD+N)

Per-call cost measured on i7-1260P (AVX2+FMA):

| N | avg | notes |
|----:|------:|------|
| 1024  | 0.049 ms | CLI `ac analyze` fast path, short captures |
| 8192  | 0.308 ms | default sweep window |
| 65536 | 3.208 ms | report-grade long capture |

Scales ~N log N with realfft. Not currently the bottleneck in any live
path — single-shot per sweep point.

## Tier 2 — `visualize::cwt::morlet_cwt_into` (live CWT column)

Per-tick cost at n=7200 samples (≈ 150 ms @ 48 kHz), 512 log-spaced scales,
σ=12, 20 Hz – 21.6 kHz:

| version | ms/tick | column rate | Δ vs baseline |
|---|---:|---:|---:|
| v0 baseline (`rustfft` full-complex, naive MAC) | 0.360 | 2.78 kHz | — |
| v1 `(-1)^k` folded into cached kernel h[] | 0.150 | 6.67 kHz | −58% |
| v2 thread-local scratch + `norm²` + `realfft` | 0.132 | 7.58 kHz | −63% |
| v3 AVX2+FMA single-accumulator (latency-bound) | 0.136 | 7.35 kHz | −62% |
| v4 AVX2+FMA **4 accumulators**, h pre-duplicated | 0.104 | 9.62 kHz | −71% |
| v5 realfft scratch via `process_with_scratch` | ~0.095 | ~10.5 kHz | −74% |

### v5 hotspot breakdown

| % | bucket | comment |
|---:|---|---|
| **63%** | `mac_avx2_fma` | AVX `vfmadd231pd` + `vmovupd` loads. Samply labels the loads as `copy_nonoverlapping<u8>` — that's `stdarch`'s internal name for `_mm256_loadu_pd`, not memcpy. Verify via `addr2line -f -C -i`. |
| 12% | `rustfft` butterflies | Inner complex FFT used by realfft. |
| 9%  | `KERNEL_CACHE.with` epilogue | `log10` + `out.push` + cache borrow. |
| 3%  | `realfft::process_with_scratch` | |
| ~8% | libm `__log10_finite` (+ internals) | One per scale. |

### What's left on the table

1. **Fast `log10` approximation** — 3rd-order polynomial on the f64
   mantissa with `frexp` exponent extraction. ±0.1 dB is plenty for a
   waterfall. Saves ~4% of tick time.
2. **rustfft → pocketfft** or similar — 12% share; unlikely to beat
   rustfft's AVX path meaningfully.
3. **Bigger workload amortization** — small-kernel per-scale overhead
   shrinks proportionally as `n_scales` and per-kernel width grow.

## Paths without micro-benches (TODO)

These show up in `scripts/profile-samply.sh` flame graphs but don't have
dedicated bench binaries yet. Order by likelihood of being next:

| path | location | rough per-tick cost | bench? |
|---|---|---|---|
| `visualize::fractional_octave` | IEC 61260-1 filterbank aggregation | ? | ❌ |
| `visualize::transfer` | cross-spectrum + coherence per tick | ? | ❌ |
| `visualize::spectrum` | STFT per monitor tick | ? | ❌ |
| `measurement::loudness` | BS.1770-5 integrated LKFS / LRA / TP | ? | ❌ |
| `measurement::filterbank` | Tier-1 IEC 61260-1 reference filterbank | ? | ❌ |

Add an example under `crates/ac-core/examples/` following the pattern in
`bench_cwt.rs` (env-driven `AC_BENCH_ITERS`, synthetic input, single
`println!`). Then run both standalone for a timing number and under
samply via the `--pid` attach pattern above.

## UI (`ac-ui`)

Profile via `scripts/profile-ui-samply.sh`. Synthetic producer at
`CHANNELS × BINS @ RATE` Hz drives a deterministic workload so runs are
comparable. Most interesting viewports in order of CPU weight:

1. `waterfall` — wgpu texture streams + per-frame palette lookup.
2. `matrix` — repeated `egui::Ui::with_clip_rect` over every channel cell.
3. `cwt` — same producer as `waterfall`; extra cost is the `fractional_octave`
   aggregation optionally layered on top (keys `Shift+O`, `A`, `I`).

No per-frame latency numbers tabulated here yet — add them once there's a
reason to optimize a specific view.

## Reading symbolicated profiles offline

Samply's `libs[]` is empty when recording via `samply record -- <bin>`
because the initial exec `MMAP` events are missed. With the `--pid` attach
pattern it's populated and each frame resolves via `resourceTable → lib`.
Example helper (inline-chain bucket):

```python
import json, subprocess, collections
p = json.load(open("cwt.json"))
libs, t = p["libs"], p["threads"][0]
ft, fn, rt = t["frameTable"], t["funcTable"], t["resourceTable"]
for s in t["samples"]["stack"]:
    f    = ft["func"][ft["func"][s] if False else t["stackTable"]["frame"][s]]
    res  = fn["resource"][f]
    addr = ft["address"][t["stackTable"]["frame"][s]]
    lib  = libs[rt["lib"][res]]["debugPath"] if res >= 0 else None
    if lib:
        chain = subprocess.check_output(
            ["addr2line", "-f", "-C", "-i", "-e", lib, f"0x{addr:x}"],
            text=True).splitlines()
        # chain is innermost-first: chain[0] = deepest inline, chain[2] next out
```

This is what differentiated real memcpy from stdarch-labeled AVX loads in
the v4/v5 CWT traces.
