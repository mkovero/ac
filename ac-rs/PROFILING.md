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

## Tier 2 — `visualize::spectrum::spectrum_only` (live FFT spectrum)

Hann-windowed realfft magnitude. Same i7-1260P, 48 kHz:

| N | avg | notes |
|----:|------:|------|
| 1024  | 0.010 ms | low-latency monitor |
| 4096  | 0.022 ms | default `ac monitor` |
| 8192  | 0.056 ms | |
| 16384 | 0.113 ms | long-window monitor |
| 32768 | 0.461 ms | |
| 65536 | 0.806 ms | |

The N=32768 cliff is cache-driven (complex FFT output stops fitting in
L2). At monitor defaults this path is never the bottleneck.

## Tier 2 — `visualize::fractional_octave::cwt_to_fractional_octave`

Overlay on top of a CWT column (`O`/`A`/`I` hotkeys). 512 log scales input:

| bpo | bands | avg | fraction of CWT tick |
|-----|------:|------:|----:|
|  1  |   9   | 0.012 ms |  13% |
|  3  |  29   | 0.007 ms |   8% |
|  6  |  60   | 0.007 ms |   7% |
| 12  | 120   | 0.008 ms |   9% |
| 24  | 242   | 0.010 ms |  11% |

Monotone single-pass merge: both `cwt_freqs` and the band grid are
monotonically increasing and bands are contiguous, so one forward sweep
places every scale into at most one band — O(scales + bands) instead of
O(scales × bands). `10^(db/10)` is pre-computed once per tick, pulling
512 `powf` calls out of the inner loop.

Previous O(N·M) version cost 0.085 ms at 1/24 oct (89% of a CWT tick);
this drops it to 10% regardless of bpo.

## Tier 2 — `measurement::loudness` (BS.1770-5)

Per 1 s of audio at 48 kHz, single channel:

| op | avg | realtime factor |
|---|------:|---:|
| `KWeighting::apply`   | 0.255 ms | ~3900× |
| `GatingBlock::push`   | 0.117 ms | ~8500× |

Both are IIR + running-sum, nowhere near the per-tick budget. No action
needed.

## Tier 1 — `measurement::filterbank::Filterbank::process`

IEC 61260-1 Class 1 reference filterbank, 1 s of audio at 48 kHz:

| bpo | bands | avg |
|-----|------:|-----:|
|  1  |   9   | 0.55 ms |
|  3  |  29   | 1.27 ms |
|  6  |  59   | 2.30 ms |
| 12  | 119   | 4.56 ms |

Bands are independent IIR chains reading `samples` read-only, so the
outer iterator runs under `rayon::par_iter`. Per-band work (one
6th-order Butterworth bandpass + mean-square over ~48 k samples) is
~ms-scale — well above the threshold where rayon's wake cost is
amortized. Previous serial version cost 25.8 ms at 1/12 oct; parallel
drops it to 4.6 ms, a ~5.6× speedup on this i7-1260P.

## Tier 1 — `visualize::transfer::h1_estimate`

Welch PSD at 1 Hz resolution + H1 + coherence on pseudo-white reference:

| n_averages | capture | n | avg |
|---:|---:|---:|------:|
|  1 | 1.0 s  |  48000 |  9.2 ms |
|  4 | 2.5 s  | 120000 | 17.1 ms |
|  8 | 4.5 s  | 216000 | 31.6 ms |
| 16 | 8.5 s  | 408000 | 36.3 ms |

Previously did three separate Welch passes (`welch_psd(x) + welch_psd(y)
+ welch_csd(x,y)`), which FFT'd each segment twice (once per side, once
in the cross term). `welch_all` shares the per-segment FFT pair across
all three accumulators, halving the FFT count. avgs=16 drops
48.7 → 36.3 ms (−25%); at avgs=1 the cost is dominated by
`estimate_delay`'s single 262 k-point FFT+IFFT, which this change does
not touch.

## CLI commands — `generate`, `sweep level/frequency`, `plot`, `sweep ir`

Most CLI commands are **audio-I/O bound**, not CPU bound — the wall-clock
time is dominated by playing and capturing audio at real-time rate. In
order of compute relevance:

| command | compute cost | wall-clock dominator |
|---|---|---|
| `generate`              | zero (`set_tone` + sleep loop)       | indefinite, user-stopped |
| `sweep level`           | zero (10 ms loop, linear gain ramp)  | `duration` |
| `sweep frequency`       | zero (10 ms loop, log freq ramp)     | `duration` |
| `plot`                  | ~2 ms `analyze` per point            | `n_points · (settle + duration)` |
| `sweep ir` (Farina ESS) | post-capture DSP (see below)         | capture `duration + tail_s` |

### Farina post-capture DSP (`measurement::sweep`)

Per-call cost after the audio capture finishes:

| dur | n | log_sweep | inverse_sweep | deconvolve_full | extract_irs |
|---:|---:|---:|---:|---:|---:|
|  1 s |  48 000 | 0.7 ms |  2.2 ms |  6.1 ms | 0.004 ms |
|  3 s | 144 000 | 2.2 ms |  6.7 ms | 27.2 ms | 0.004 ms |
| 10 s | 480 000 | 6.7 ms | 21.6 ms | 64.5 ms | 0.004 ms |

`inverse_sweep` previously ran a **full FFT linear convolution** of `x`
against the unnormalised `inv` purely to pick the scale factor that
makes the identity-system IR unity-magnitude. Replaced with a direct
evaluation of the convolution at the central ±4 lags — Farina's
construction places the peak exactly at lag `N-1` in continuous math,
so a small window is all that's needed to pin the discrete-sampled
maximum. 64 → 22 ms at 10 s (−66%); the existing identity-IR peak test
(5% tolerance) still passes.

`deconvolve_full` is one forward FFT + one inverse FFT of
`next_pow_of_2(2N-1)` (131 072 at 1 s). Already realfft-backed — no
obvious lever short of rayon-parallelised FFT or a different library.

For a 10 s `sweep ir`: post-capture DSP is now ~93 ms (was ~125 ms),
trivially below the ~10.5 s of wall-clock capture time. At 1 s
post-capture is ~9 ms. None of these paths is user-visible.

## Big-picture latency budget

Default live-monitor tick (`ac monitor`, single channel, 50 ms interval
= 20 Hz):

| stage | cost @ default | cost with everything on |
|---|------:|------:|
| spectrum N=16384 *or* CWT 512 scales | 0.11 ms | 0.11 ms |
| fractional-octave overlay 1/6 | — | 0.03 ms |
| K-weighting 50 ms block | — | ~0.013 ms |
| gating 50 ms block | — | ~0.006 ms |
| **total per tick** | **0.11 ms** | **~0.16 ms** |

At 20 Hz that's ~0.3% of one core per channel. An 8-channel monitor with
the full overlay stack is still ~2.5% of one core — the live path is
not CPU-bound anywhere on current hardware.

The expensive calls (`h1_estimate`, `Filterbank::process`, long-window
`analyze`) are per-sweep, not per-tick, so their budget is human-scale
(~seconds per measurement).

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
