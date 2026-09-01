# Codex Audit

## Executive Summary

`ac` is a Rust bench-audio measurement system: a ZeroMQ daemon owns audio I/O and long-running measurement workers, `ac-core` supplies DSP and archival formats, and CLI/view crates consume the daemon's frames. I reviewed the architecture and protocol documentation, current status/history, daemon lifecycle and routing, capture/transfer/monitor paths, snapshots, configuration persistence, and selected core DSP/serialization code.

The most serious issue is an unsafe deployment default: invoking `ac-daemon` directly exposes an unauthenticated control service on every interface. That service can be used to change the snapshot spool directory; the next transfer recursively deletes that directory. The protocol also has several unbounded cardinality/duration inputs that let one request consume effectively unlimited CPU or memory. A second independent pass retained all five preliminary findings and added archival processing-state corruption and inflated xrun telemetry.

## Repository / Architecture

The workspace consists of `ac-daemon` (ZMQ REP/PUB control plane plus JACK/CPAL/fake backends), `ac-core` (measurement and live DSP, calibration, reports, and `.acsnap`), `ac-cli`, and the pure `ac-scene` / rendering `ac-view` split. Workers own their audio engines and are grouped to avoid conflicting capture/output work. Live transfer uses a reference/measurement pair, continuously captures raw samples, maintains delay locks, and produces H1/coherence frames. Monitor uses a per-channel loop and emits spectrum, loudness, and scope sidecars. Snapshots are a raw, bounded per-session capture ring written as `meta.json` plus multichannel FLAC.

Important intended invariants include: local daemon use should not expose control externally; a snapshot spool is disposable only within its dedicated spool directory; channel indices identify the requested hardware ports exactly; scope frames sharing a `frame_idx` represent aligned channels; and snapshot metadata maps each FLAC stream position to the calibration and input channel used to derive it.

## Findings

### AUDIT-001 — Default daemon exposes unauthenticated remote control and arbitrary recursive deletion

**Severity:** Critical  
**Confidence:** Confirmed  
**Location:** `ac-rs/crates/ac-daemon/src/main.rs:34`, `ac-rs/crates/ac-daemon/src/server.rs:171-183`, `ac-rs/crates/ac-daemon/src/handlers/admin.rs:156-161`, `ac-rs/crates/ac-daemon/src/handlers/snapshot.rs:217-223`

**Verification:** confirmed — Second-pass tracing verified that `setup` persists the untrusted path before `transfer_stream` captures it, and that `transfer_stream` calls `reset_spool_dir` before installing the session ring. The CLI auto-spawner's explicit `--local` and the README's explicit remote-enable workflow are counterexamples to *every* normal CLI launch being exposed, but they do not change the unsafe default of direct daemon invocation.

**Problem**

Running `ac-daemon` without `--local` binds both control and data sockets to `*`. There is no authentication or authorization on the REP protocol. A remote client can send `setup` to choose any `snapshot_spool_dir`; when `transfer_stream` starts, `reset_spool_dir` calls `remove_dir_all` on that client-selected path.

**Why it is wrong**

The README describes remote access as an explicit `ac server enable` action, and the CLI auto-spawner deliberately adds `--local`. Direct invocation instead defaults to public binding. The snapshot cleanup invariant only holds if the spool path is trusted and confined to the daemon's own spool directory. Here it is arbitrary configuration received from any network peer.

**Failure scenario**

On a host where a user starts `ac-daemon` in the documented direct form (without `--local`), a LAN peer sends:

1. `{"cmd":"setup","update":{"snapshot_spool_dir":"/path/to/victim-directory"}}`
2. `{"cmd":"transfer_stream","pairs":[[0,1]]}`

The worker executes `remove_dir_all("/path/to/victim-directory")` with the daemon user's privileges before creating a replacement directory. The same unauthenticated peer can also stop workers, alter audio/configuration, or request output-driving commands.

**Evidence**

`main.rs` makes `local_only` true only when `--local` is supplied. `server::run` selects `"*"` and records `listen_mode: "public"` otherwise. `setup` stores a supplied string directly as `snapshot_spool_dir`; `transfer_stream` resolves it and calls `reset_spool_dir`; that function ignores errors from `fs::remove_dir_all` and recursively removes its argument. No dispatch path authenticates a request.

**Recommendation**

Bind loopback by default and require an explicit, clearly named public-listen option. Do not expose destructive configuration or hardware-driving commands on an unauthenticated TCP interface; use authentication and/or a separately secured administrative channel. Independently, constrain spool paths to a daemon-owned base directory (or create a unique session subdirectory) and never recursively delete a caller-configurable path.

### AUDIT-002 — Protocol parameters can cause unbounded allocation and CPU denial of service

**Severity:** High  
**Confidence:** Confirmed  
**Location:** `ac-rs/crates/ac-daemon/src/handlers/audio/plot.rs:34-47,287-296,566-587`; `ac-rs/crates/ac-daemon/src/handlers/mod.rs:370-376`; `ac-rs/crates/ac-daemon/src/handlers/admin.rs:150-154`; `ac-rs/crates/ac-daemon/src/handlers/transfer.rs:570-581`

**Verification:** confirmed — the second pass followed each value through its allocating consumer. No CLI-side limit protects direct ZMQ callers. This finding also applies to an unbounded `monitor_spectrum.channels` array: each requested entry creates several per-channel rings/states before the worker starts. Normal monitor FFT parameters are bounded, but that does not bound channel cardinality or the listed measurement inputs.

**Problem**

Several wire inputs are converted from `u64`/`f64` to allocation sizes without a maximum. `plot.ppd` controls the length of a collected frequency vector; `plot_level.steps` controls a generated level vector; `plot_ir.n_harmonics` and `window_len` control harmonic/window allocations; and `setup.snapshot_ring_s` is converted to a per-channel `VecDeque::with_capacity` size at transfer startup.

**Why it is wrong**

The daemon accepts protocol input from a client before spawning the worker, but does not impose an operational resource budget. Floating-point-to-`usize` casts saturate for very large finite values, so they do not make a giant request safe. The worker can be left allocating, looping, or aborting from allocation failure, preventing normal measurement and control.

**Failure scenario**

`{"cmd":"plot","start_hz":20,"stop_hz":20000,"ppd":18446744073709551615}` computes an astronomically large `n_points` and collects `0..n_points` into a `Vec`. A similarly unbounded `steps`, `n_harmonics`, or a `snapshot_ring_s` such as `1e300` reaches enormous vector capacity/allocation paths. If the daemon is started with its default public bind, this is remotely reachable; it is also a local reliability problem.

**Evidence**

`log_freq_points` derives `n_points` directly from `ppd` and immediately collects it. `plot_level` passes `steps` straight to `linspace`; `plot_ir` only changes zero harmonics to one, with no upper bound. `setup` accepts any positive finite `snapshot_ring_s`, and transfer computes `(snapshot_ring_s * sr).round() as usize` before constructing its rings. Existing validation is present for monitor FFT size/interval, demonstrating this class is expected to be bounded, but these command paths lack it.

**Recommendation**

Define and enforce explicit protocol limits before worker spawn for point counts, harmonic count, window size, capture duration, and retained snapshot samples/bytes. Use checked conversions and checked arithmetic, return a request error on overflow or budget excess, and cover maxima/overflow in protocol tests.

### AUDIT-003 — Oversized channel IDs silently select a different hardware channel

**Severity:** Medium  
**Confidence:** Confirmed  
**Location:** `ac-rs/crates/ac-daemon/src/handlers/transfer.rs:39-61,318-336`; `ac-rs/crates/ac-daemon/src/handlers/audio/monitor.rs:383-408`; `ac-rs/crates/ac-daemon/src/handlers/admin.rs:102-128`

**Verification:** confirmed — `serde_json::Value::as_u64` accepts the `4294967296` JSON integer; Rust's `as u32` conversion yields zero, and all subsequent checks operate only on that zero. Existing out-of-range tests exercise only in-range `u32` values that are outside the fake port list.

**Problem**

Protocol channel values are parsed as `u64` and narrowed with `as u32` before bounds checking. Values greater than `u32::MAX` wrap modulo 2^32. Thus a request for channel `4294967296` becomes channel 0 and can pass the later port-range check.

**Why it is wrong**

The range check is intended to reject invalid hardware indices, not to reinterpret them. In measurement software this can route capture/reference/output to a valid but unintended port, yielding plausible measurements of the wrong signal. It also defeats tests that only exercise ordinary out-of-range values.

**Failure scenario**

A `transfer_stream` request with `pairs:[[4294967296,1]]` is parsed as `(0,1)`. If capture ports 0 and 1 exist, it starts a transfer using channel 0 rather than rejecting the supplied measurement channel. The same narrowing pattern affects monitor channel arrays and persisted setup indices.

**Evidence**

`parse_transfer_pairs` calls `as_u64` and immediately casts both values to `u32`; only later does transfer validate the narrowed `unique_chans` against `capture_ports`. Monitor does the same conversion while building `channels`. The out-of-range integration test uses an ordinary invalid index, not a value above the destination type's range.

**Recommendation**

Use `u32::try_from` for every protocol/configuration channel conversion and reject conversion failure before any state change or port lookup. Add tests at `u32::MAX + 1` for every command accepting a channel number.

### AUDIT-004 — Multi-channel scope frames claim synchronization for sequential, non-overlapping captures

**Severity:** Medium  
**Confidence:** Confirmed  
**Location:** `ac-rs/crates/ac-daemon/src/handlers/audio/monitor.rs:70-98,1159-1197,1207-1216`

**Verification:** confirmed — the outer loop reconnects and flushes the sole input ring for each channel before calling `capture_block`; JACK's `reconnect_input` clears that ring. The shared frame ID and timestamp are generated before this channel loop. This is not merely different buffers from a simultaneous capture source. The fake backend makes the issue less visible because its reconnect is a simple port assignment, but the real JACK path is sequential by construction.

**Problem**

For more than one monitor channel, the worker captures each channel sequentially with `capture_block`; it divides the requested interval between channels and reconnects/clears input between captures. It nevertheless assigns every channel in that loop the same `frame_idx` and timestamp. The scope frame documentation expressly says this lets consumers establish that L and R came from the same capture.

**Why it is wrong**

They did not come from the same capture. With the default 200 ms interval and two channels, the first scope buffer represents approximately the first 100 ms and the second approximately the next 100 ms, plus reconnect/processing time. Pairing samples by index creates a time-shifted L/R trajectory. A goniometer/phase display will report phase/correlation characteristics that are artifacts of acquisition order, especially for changing material.

**Failure scenario**

Monitor two channels carrying a stereo transient or a phase-sensitive test signal. The consumer accepts same-`frame_idx` scope frames as a pair and plots samples from different time intervals. The display can show a rotated, diffuse, or otherwise incorrect stereo image while advertising a common capture identity.

**Evidence**

The worker's multi-channel branch calls `capture_block` once per channel; its own comment says reconnecting input clears the ring on every switch. `per_ch_budget` is `interval / channels`. Immediately after each sequential capture, `emit_scope_frame` receives the tick-wide `frame_idx`/`tick_ts_ns`. The scope-frame doc and protocol test both assert that shared `frame_idx` means the UI can pair L/R, but neither test checks temporal alignment.

**Recommendation**

Acquire multichannel scope samples from simultaneous capture rings, or do not publish them as synchronizable pairs. If sequential capture must remain, give each channel its own capture timestamp/range and prevent phase/goniometer consumers from pairing them.

### AUDIT-005 — Snapshot reader accepts internally inconsistent channel metadata and can derive with mismatched calibration/audio

**Severity:** Medium  
**Confidence:** Confirmed  
**Location:** `ac-rs/crates/ac-core/src/snapshot/mod.rs:95-165,169-212,218-260`

**Verification:** confirmed — a normal writer-produced snapshot is internally consistent (and the focused snapshot tests pass), but the public reader accepts a metadata-only permutation of `per_channel`. `derive_pair` then resolves stream positions from the permuted metadata rather than from FLAC position/`channel_map`. No reader validation or test rules out this malformed but parseable archive.

**Problem**

`read_acsnap` validates format version, sample rate, and FLAC channel count against `channel_map`, but does not validate `per_channel` length, its positional correspondence to `channel_map`, uniqueness of `input_channel`, pair references, delay count, or equal decoded channel lengths. `derive_pair` locates stream positions by searching `per_channel.input_channel`, then indexes `channels` and `per_channel` at that metadata-selected position.

**Why it is wrong**

The snapshot contract says FLAC stream order, `channel_map`, and `per_channel` describe the captured inputs and calibrations. Without cross-validation, a crafted or corrupted archive can attach the calibration for one channel to another FLAC stream, or select an arbitrary stream by duplicating/reordering input-channel metadata. The reader returns success instead of refusing an archive it cannot interpret faithfully.

**Failure scenario**

Take a valid two-channel snapshot and alter only `meta.json` by swapping the two complete `per_channel` entries while leaving FLAC and `channel_map` unchanged. `read_acsnap` accepts it. For the original `(0,1)` pair, `derive_pair` finds input 0 at metadata position 1 and consequently uses FLAC stream 1 with channel-0 calibration; it does the converse for input 1. The resulting transfer is numerically plausible but is calculated from reversed audio/calibration identities.

**Evidence**

`write_acsnap` itself checks only `channels.len() == channel_map.len()`. `read_acsnap` repeats only that count check after decoding. `channel_index_for` searches metadata rather than confirming the metadata index maps to the corresponding stream, and `derive_pair` uses that returned index for both samples and calibration. Tests cover a normal round trip and missing entries, but not malformed cross-field metadata.

**Recommendation**

Treat metadata relationships as format validity: require `per_channel.len() == channels.len() == channel_map.len()`, require `per_channel[i].role == channel_map[i]`, require unique input channels, validate every pair/delay reference, and require equal decoded frame counts. Reject inconsistent archives before exposing `Snapshot` or deriving measurements.

### AUDIT-006 — `plot` archives mixed mic-correction states as one processing chain

**Severity:** Medium  
**Confidence:** Confirmed  
**Location:** `ac-rs/crates/ac-daemon/src/handlers/audio/plot.rs:120-161,173-219`; `ac-rs/crates/ac-daemon/src/handlers/calibrate.rs:710-721`

**Verification:** confirmed — `set_mic_correction_enabled` is an unrestricted process-wide atomic toggle. The point loop reads it separately for every frequency and mutates that point's analysis accordingly. Only after all capture ends does `plot` read it again to produce the single report-level `mic_correction_applied` flag; `FrequencyResponsePoint` has no per-point processing field.

**Problem**

An operator can toggle mic correction while a stepped-sine plot is running. Points before the toggle are calculated with one correction state and points after it with another, but the saved `MeasurementReport` labels the entire frequency-response payload according to whichever state happens to be set at report construction.

**Why it is wrong**

The report is intended to be archival and reproducible. A single processing-chain statement cannot truthfully describe a response assembled from different correction states, and the report contains insufficient information to recover which frequencies were corrected.

**Failure scenario**

Start `plot` with a loaded mic curve and correction enabled. After low-frequency points are emitted, send `set_mic_correction_enabled:false`. The low-frequency points contain corrected magnitude/THD; later points do not. The final report records `mic_correction_applied: false`, incorrectly claiming all points are raw. Reversing the toggle produces the converse false claim.

**Evidence**

The per-point code loads `mic_corr_enabled` at lines 126-130 and immediately changes `AnalysisResult`. The report code reads it again at line 179 after `eng.stop()`. The toggle handler has no busy guard and documents itself as process-wide. Existing tests verify a stable correction setting, not a mid-run transition.

**Recommendation**

Freeze processing-affecting settings for the lifetime of an archival measurement, or reject/stop the measurement on a requested change. If live changes are deliberately supported, record the per-point state and make mixed-state reports explicitly non-reproducible.

### AUDIT-007 — Cumulative xrun counter is summed once per sweep point

**Severity:** Low  
**Confidence:** Confirmed  
**Location:** `ac-rs/crates/ac-daemon/src/handlers/audio/plot.rs:96-119,221-230,335-356`; `ac-rs/crates/ac-daemon/src/audio/jack_backend.rs:549-551`

**Verification:** confirmed — `JackEngine::xruns()` returns the session-cumulative atomic counter. Both stepped plot loops add that cumulative value on every point rather than assigning it. The monitor implementation explicitly documents and avoids this same mistake by assigning `xruns_total = eng.xruns()` each tick.

**Problem**

The completion frame's xrun total is inflated whenever an xrun occurs before the final point. One xrun occurring before point 2 of a 20-point plot is reported as 19, not 1.

**Why it is wrong**

Xrun count is measurement-quality telemetry. Inflating it makes a healthy remainder of a measurement appear much less reliable and defeats comparisons between runs with different point counts.

**Failure scenario**

A JACK xrun increments the backend's atomic counter to one during the first settled capture. Each later iteration adds one again, so the completion event reports the number of subsequent points rather than the number of xruns.

**Evidence**

`plot` and `plot_level` both execute `xruns += eng.xruns()`. The backend counter is cumulative, as its implementation and monitor's corrective comment show. Tests use fake audio with zero xruns and therefore cannot distinguish addition from assignment.

**Recommendation**

Assign the cumulative counter at completion (or compute deltas against the previous value), and add a fake/backend test that injects an xrun before more than one sweep point.

## Positive Findings

The transfer path has substantial, specific handling for contiguous capture, reference-ring alignment, bounded delay-lock retries, and explicit lock evidence. The snapshot writer uses a deterministic content hash and the FLAC encoder rejects mismatched channel lengths. Core DSP and scene tests cover many analytic and standards-derived properties rather than only smoke tests.

## Areas Requiring Deeper Investigation

- Real JACK and physical I/O behavior could not be exercised in this environment; the audit traced their ownership/ring logic but did not validate device routing on hardware.
- The full daemon integration suite aborts in libzmq in this environment, so lifecycle and network timing coverage beyond static tracing remains incomplete.
- The output-driving safety model warrants a dedicated threat-model review if remote operation is a supported deployment, particularly around authentication, `set_drive`, and configuration persistence.

## Testing / Validation Performed

- Inspected repository tree, `README.md`, `ARCHITECTURE.md`, `ac-rs/ZMQ.md`, source layout, status, recent log, selected commit diffs, and targeted blame.
- Ran `cargo test --workspace` in `ac-rs`. Compilation succeeded, but the first daemon-spawning integration test (`ac-cli --test it_plot_ir`) aborted in libzmq's `signaler.cpp` with `SIGABRT`; the workspace run therefore did not complete.
- Ran `cargo test -p ac-core -p ac-scene -p ac-daemon --lib` in `ac-rs`; it completed successfully. The displayed output included the 474 `ac-core` unit tests passing; tool output for the overall command was truncated.
- Second pass: ran `cargo test -p ac-core snapshot::tests --lib`; 10 passed and 1 fixture-regeneration test was ignored. This confirms normal snapshot round trips but does not exercise malformed cross-field metadata.
- Second pass: ran `cargo test -p ac-daemon parse_pairs --bin ac-daemon`; all 6 existing transfer-pair parser tests passed. They cover malformed shapes and ordinary values, not narrowing overflow.

## Second-Pass Assessment

All five preliminary findings survived verification. AUDIT-001 is conditioned on a direct daemon launch without `--local`; normal CLI auto-spawn is local-only, but the direct daemon default remains public and unauthenticated. AUDIT-002 through AUDIT-005 were confirmed by tracing the parsed values or metadata through their actual consumers; no caller-side validation or test invalidates the reported paths.

No preliminary finding was rejected or downgraded. The second pass added AUDIT-006 (mixed correction state in a report that claims one processing chain) and AUDIT-007 (cumulative xruns summed once per point). The latter is low severity but is confirmed and affects diagnostic truth.

Substantially unaudited portions of the roughly 75k LOC repository remain: real JACK/CPAL callback behavior under device loss and xrun pressure, GPIO/DMM interactions, report HTML/PDF rendering, the many standards-specific DSP implementations, CLI parsing/export paths, and GUI rendering/input behavior. The completed passes give high confidence in the seven listed defects because each has a concrete code-level trigger and effect; they do not establish that these are the only important defects in the repository.

## Final Assessment

The DSP core has meaningful test coverage, but the daemon's trust boundary is unsafe. Address AUDIT-001 first: the default network bind plus unauthenticated mutable configuration turns snapshot cleanup into a remote arbitrary-directory deletion primitive. Next, put hard resource budgets on protocol inputs (AUDIT-002), then fix channel identity, multichannel synchronization, snapshot cross-field validation, and archival processing-state capture so reported measurements cannot silently refer to the wrong signal, calibration, or processing state.
