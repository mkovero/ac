# handoff: field transfer view (`ac transfer`), stimulus control, channel persistence

Audience: triage → architect → developer → qa → ux, in that order. This document is
self-contained: do not rely on `.agents/*` module maps (stale) or on conversation history.
All decisions herein are ratified by Markus; do not reopen them. Anything not listed as a
deliverable is out of scope — flag conflicts, do not silently pick a side.

## 1. Mission

Two commands, two fixed views, one binary (`ac-view`):

- `ac monitor` → **Spectrum view**: the existing M0–M3 deliverable, unchanged except one
  new toggle (ref-trace visibility). Bench instrument.
- `ac transfer` → **Transfer view** (new): H1 transfer magnitude over phase, coherence-
  gated, broadband, live, with delay readout, input-level meters, and safe stimulus
  control. Field instrument for PA/subwoofer alignment.

No view-switch key exists; the view is fixed at launch. The ratatui monitor survives as
`ac monitor --tui`.

Field workflow this serves (context, not a deliverable): mic at mix position; measure
tops alone → snapshot (`S`, draws dashed); measure subs alone live; de-rotate both phase
traces by the snapshot's delay; adjust DSP delay until live phase lies on the dashed
trace through the crossover. Per-path muting is the operator's job; broadband delay
estimation is therefore trustworthy per run.

## 2. Ratified decisions (do not relitigate)

- D1  No ref-spectrum trace and no spectrum traces of any kind in the transfer view.
- D2  Delay readout = the daemon's existing broadband `delay_ms`, shown in ms and meters
      (c = 343 m/s exactly). No band-limited delay computation.
- D3  Phase is delay-removed by default (de-rotated); a key toggles raw. De-rotation τ is
      selectable: this session's delay, an open snapshot's delay, or 0 (raw).
- D4  The session's delay estimate is frozen at session start (existing daemon behavior).
      This is correct, not a bug: operator DSP-delay changes must appear as phase tilt.
      Do not add continuous re-estimation.
- D5  Coherence gate on by default: columns with coherence < 0.5 render as gaps (never
      zero-lines) on both panes. Fixed threshold, no tuning UI.
- D6  Input-level meters: transfer view only, peak-only, always on (not toggleable).
- D7  Stimulus: Space arms → Enter fires → any of Space/Enter/Esc stops. ↑/↓ adjust level
      while armed AND while driving. Dead-man 1.5 s. Details §5.
- D8  Channel persistence: last writer wins in `~/.config/ac/config.json`, same fields
      `ac setup` writes (`input_channel`, `reference_channel`, `output_channel`). No
      separate UI prefs file.
- D9  Exactly two wire/daemon additions, no more: `set_drive` (CTRL) and per-frame
      `meas_peak_dbfs`/`ref_peak_dbfs` (DATA). Everything else is client-side additive.
- D10 Key letters for new toggles are assigned in ONE pass at M4b over the full table.
      Fixed constraints: Space/Enter/Esc/↑/↓ belong to stimulus; Q = quit; S = snapshot;
      forbidden keys `[` `]` `+` `-` (Finnish layout; `assert_no_forbidden_keys` enforces).

## 3. Repo orientation (verified against current tree — trust this over agent-spec maps)

```
ac-rs/crates/
  ac-core/    src/config.rs            — Config struct, ~/.config/ac/config.json
  ac-daemon/  src/handlers/transfer.rs — transfer_stream worker (pairs, drive, publish)
              src/workers.rs           — Arc<AtomicBool> stop-flag pattern for workers
  ac-cli/     src/commands/plot.rs     — launch_ui / LaunchKind (Monitor → TUI today)
              src/commands/monitor.rs  — `ac monitor`, resolve_channels_or_default
              src/commands/monitor_tui.rs — ratatui fallback (keep, flag-gate)
  ac-scene/   src/{wire,scene,ticks,readout,dbfs}.rs — pure display-truth crate
  ac-view/    src/{main,app,view,keys,session,zmq_client,snapshot_flow,range,
                   geometry,computes_nothing}.rs — eframe shell
```

Discipline (architectural law, tested): `ac-scene` computes every number, string, and
normalized [0,1]² coordinate. `ac-view` performs only the affine viewport map;
`computes_nothing.rs` forbids `log10`/formatting there — this workstream extends the
prohibition to trig (de-rotation lives in ac-scene, never the renderer).

## 4. Wire contract

### 4.1 Consumed (already published today, currently unmodelled in ac-scene)

`transfer_stream` v2 DATA frame fields to ADD to `ac_scene::wire::WireFrame`
(serde ignores unknown fields, so this is deserialization-only):

```json
"freqs":         [f64, ...],   // H grid — DISTINCT from spec_freqs
"magnitude_db":  [f64, ...],   // |H| in dB, same length as freqs
"phase_deg":     [f64, ...],   // arg(H) in degrees, wrapped ±180
"coherence":     [f64, ...],   // γ² in [0,1], same length
"delay_samples": i64,
"delay_ms":      f64
```

### 4.2 Addition A — per-frame raw input peaks (DATA)

Worker computes per published frame, from RAW capture blocks BEFORE any calibration,
weighting, or aggregation (rationale: meters exist to judge gain staging; calibrated or
band-aggregated values hide clipping):

```json
"meas_peak_dbfs": f64,   // 20*log10(max|sample|) over the frame's blocks; -inf → null
"ref_peak_dbfs":  f64
```

### 4.3 Addition B — `set_drive` (CTRL, REQ/REP)

```json
{"cmd": "set_drive", "on": true|false, "level_dbfs": -20.0}
→ {"ok": true, "on": true, "level_dbfs": -20.0}
```

Semantics:
- Valid only while a transfer_stream worker runs; otherwise `{"ok":false,"error":...}`.
- Worker holds an `Arc<DriveState>` (same ownership pattern as workers.rs stop_flag):
  atomic on-flag, atomic level bits, atomic last-keepalive timestamp (millis). Polled
  once per audio block. `on:false` must silence output within one block.
- EVERY `set_drive` message refreshes the keepalive timestamp — there is no separate
  keepalive command. The UI re-sends the current state every 250 ms while driving
  (idempotent). Worker drops drive (on-flag → false) when now − last_keepalive > 1500 ms;
  the session keeps running. Keepalive clock is monotonic (Instant baseline), not
  wall-clock epoch millis — a clock step must not extend or trip the dead-man.
- The dead-man arms on the **first `set_drive`** received by the worker and remains armed
  for the session (amended 2026-07-24, #187 QA + ratification). A session launched with
  the legacy `drive: true` param sends no keepalives; that launch-time path retains its
  existing **unsupervised** semantics (drives until stopped, on the operator's deliberate
  responsibility), and the dead-man governs only stimulus started through `set_drive`.
  Every UI-driven session goes through `set_drive`, so the UI is always covered.
- `level_dbfs` clamped server-side to ≤ `drive_max_dbfs` (new Config field, default
  −10.0, filled by serde default for old config files). Client clamps too; server clamp
  is the one QA tests as authoritative.
- Sessions ALWAYS launch with drive off. The existing launch-time `drive` param remains
  for CLI/scripted use but `ac transfer` never sets it.

## 5. Stimulus state machine (client)

States: Idle → (Space) → Armed → (Enter) → Driving. 

- Armed: auto-disarm after 5 s without a keypress in {Enter, ↑, ↓}; ↑/↓ reset the timer.
  Esc or any other key disarms. No audio output occurs in Armed.
- Driving: Space, Enter, or Esc → immediate `set_drive off` + state → Idle. ↑/↓ adjust
  level live: 1 dB steps, 3 dB with Shift, clamped to `drive_max_dbfs`; each change sends
  `set_drive` immediately.
- Banner (large type, top-center, both states, `ac-scene`-formatted verbatim strings):
  `ARMED  →  OUT {ch}{ (port)}   {level:+.1} dBFS  — Enter starts, Esc cancels`
  `DRIVING   OUT {ch}{ (port)}   {level:+.1} dBFS  — Space/Enter/Esc stops`
  `{ch}` = config `output_channel`; `(port)` appended iff `output_port` is Some.
- UI process exit while driving: rely on the dead-man (1.5 s) — no special teardown
  required, but a best-effort `set_drive off` on clean quit is expected.

## 6. Scene mathematics (ac-scene; QA re-derives everything independently)

Sign convention, stated once: a physical delay τ > 0 (later arrival) produces measured
phase φ(f) = −360·f·τ (degrees, f in Hz, τ in seconds). De-rotation therefore ADDS:

    φ'(f) = wrap±180( φ(f) + 360·f·τ_derot )

**The wire does not carry raw phase** (corrected 2026-07-24, #180 architect pass; R1
ratified by Markus). `ac-core/src/visualize/transfer.rs` multiplies Gxy by
exp(+j·2π·f·delay_samples/sr) before forming H1 and takes phase_deg = h1.arg() after
that; the worker estimates delay_samples once and freezes it (D4). So the wire carries

    φ_wire(f) = φ_raw(f) + 360·f·τ_sess

already de-rotated by the session's own delay. The τ_derot each D3 mode supplies is
therefore measured from φ_wire, not from raw phase:

    session delay   →  τ_derot = 0                  (φ_wire as-is)
    raw             →  τ_derot = −τ_sess            (φ_raw)
    snapshot's delay→  τ_derot = τ_snap − τ_sess    (φ_raw + 360·f·τ_snap)

Overlay: the snapshot trace is drawn as-is (already compensated by its own τ_snap, so
its τ_derot is 0) and the live trace takes τ_snap − τ_sess; both land on a common
reference. D4 survives — operator DSP-delay changes still tilt φ_wire.

The superseded enumeration (τ_derot ∈ { τ_sess, τ_snap, 0 }) assumed raw phase on the
wire and double-compensates in session mode: wrong-sign tilt at exactly the magnitude
the operator is nulling. The de-rotation function itself is unchanged — only the
mode→τ_derot mapping was wrong.

Wrap interval: **(−180, +180], negative end open** — the range of `Complex::arg`, which
produced phase_deg upstream. Not a free choice: scene and wire must agree at the
boundary. The idiomatic rem_euclid-then-shift form is half-open the other way and
returns −180 where this returns +180. Required: −900° → +180.000°, −180° → +180.000°,
+180° → +180.000°, +181° → −179.000°.

Delay readout: `{delay_ms:.2} ms  ({delay_ms*0.343:.2} m)` — c = 343 m/s exactly.

Meter normalization: bar height h = clamp((peak_dbfs + 60) / 60, 0, 1); floor −60 dBFS.
Clip latch: peak_dbfs ≥ −0.1 sets a latch that persists ≥ 3 s of scene time.
Peak-hold tick decays after ~1.5 s. null peak → h = 0, no latch.

Coherence mask: boolean per column, `coherence[i] < 0.5` → masked. Masked columns are
absent from the emitted polylines (traces split into segments); they are never emitted
as y=0 points.

## 7. Milestones, deliverables, acceptance criteria

Falsification-first: every AC below names the failure mode it kills. QA derives all
expected values from §6 formulas independently — never from code comments.

### M4a — ac-scene: transfer scene (pure; no window, no daemon needed)

Deliverables:
1. WireFrame extension (§4.1, §4.2 fields; peaks Optional for old daemons).
2. Scene: mag + phase traces (normalized, coherence-masked, segment-split), delay
   readout string, meter model (heights, hold, latch), banner strings, de-rotation with
   caller-supplied τ_derot.
3. Snapshot path: `.acsnap` derivation exposes the stored delay so live + snapshot can
   share a common τ_derot.
4. `computes_nothing` extended: no trig in ac-view.

Fixtures (exact numbers; sr = 48000). **Daemon-shaped**: every phase_deg below is what
the wire actually carries (φ_raw + 360·f·τ_sess). F1′/F1″/F2′ supersede the original
F1/F2, which hand-built raw phase and would have passed against the double-compensating
mapping.
- F1′ no mis-estimate: τ_true = τ_est = 2.5 ms (120 samples), gain 0.5 ⇒ wire phase_deg
  ≡ 0, magnitude_db ≡ −6.0206 dB. Session mode ⇒ 0.000° at every column. Raw mode ⇒
  wrap(−360·f·0.0025): −90.000° at 100 Hz, +135.000° at 250 Hz, +180.000° at 1 kHz (the
  boundary case). Delay string: `2.50 ms  (0.86 m)`. Kills: double-compensation —
  verbatim-§6 session mode shows +90.000° at 100 Hz — and raw-mode sign errors.
- F1″ mis-estimate: τ_true = 2.5 ms, τ_est = 2.0 ms ⇒ wire phase = −360·f·0.0005,
  −18.000° at 100 Hz. Session mode shows exactly that residual, not zero. Kills: the
  "session mode must be flat" misreading — that residual is what the operator nulls.
- F2′ overlay: snapshot session τ_snap = 3.0 ms, live session τ_sess = 2.5 ms, both
  measuring the same physical τ_true = 3.0 ms ⇒ snapshot wire ≡ 0, live wire =
  −360·f·0.0005; live de-rotated by τ_snap − τ_sess = +0.5 ms ⇒ ≡ 0 ⇒ traces overlay
  exactly. Kills: per-trace-own-delay de-rotation, and the sign of the cross-session
  correction (wrong sign doubles the residual to −36.000° at 100 Hz instead of nulling).
- F3 coherence 0.9 everywhere except columns 5..9 at 0.3 ⇒ exactly those columns masked;
  emitted polyline splits into exactly two segments. Kills: threshold off-by-one, and
  masked-as-zero rendering.
- F4 meters: peak 0.5 ⇒ −6.0206 dBFS ⇒ h = 0.89966; peak 1.0 ⇒ 0 dBFS ⇒ h = 1, latch
  set; null ⇒ h = 0, no latch. Kills: calibrated-value leakage (a voltage-cal'd frame
  must NOT move the meter), floor/scale errors.
- F5 banner strings byte-exact for (ch=3, port Some("Fireface400:AN3"), −20.0) and
  (ch=0, port None, −10.0). Kills: renderer-side reformatting.

### M4b — ac-view: Transfer view, toggles, key table

Deliverables: `ViewKind::Transfer` (new match arm; shell untouched) — mag pane stacked
over phase pane, shared log-f axis, gap rendering for masked columns, delay readout and
meters and banner drawn verbatim; single-pass key table (D10) adding: raw-phase toggle,
τ_derot source cycle (session/snapshot/raw), ref-trace toggle (Spectrum view), settings
overlay open, stimulus keys.

AC: keys — uniqueness + forbidden-key + help-lists-everything tests extended; per-view
tables contain no dead keys (an action bound in a view must do something there).
Scene-accessor tests (no shape scraping): each toggle changes `current_scene()` output.
Masked columns render as polyline gaps — geometry test asserts no vertex exists at a
masked column's x.

### M4c — channels, persistence, stimulus client

Deliverables: main.rs reads `input_channel`/`reference_channel` from config (hardcoded
0/1 removed; missing `reference_channel` is a fatal error with hint
`ac setup reference <N>` — transfer-only session model makes it required). Settings
overlay: meas/ref/stimulus-out rows + start level; ↑↓ row, ←→ value, Enter applies
(session relaunch + Config::save), Esc cancels with zero side effects. Stimulus state
machine per §5 (headless-testable module). `drive_max_dbfs` Config field.

AC: last-writer-wins round trip in BOTH orders (UI change → file → simulated
`ac setup` → next app construction uses setup's value; and reverse). Overlay-cancel
side-effect-freeness. State machine: auto-disarm at 5 s; ↑/↓ resets timer; panic stop
from Armed and from Driving; level clamp at the client entry points; 250 ms resend loop
produces monotone keepalive timestamps. Kills: the "armed forever" and "stop only works
from driving" classes.

### M4d — daemon additions + CLI launch wiring

Deliverables: §4.2 peaks in the published frame; §4.3 `set_drive` handler + DriveState
poll + server-side clamp + 1.5 s dead-man. CLI: `ac transfer` command (new parse +
command) and `ac monitor` both spawn `ac-view` with view kind + endpoint from config
(honoring `server_host`); `ac monitor --tui` keeps ratatui; `ac monitor` channel args
map to meas channel as today.

AC (fake-audio integration): known-amplitude fake tone (amplitude 0.5) ⇒
`meas_peak_dbfs` = −6.02 ± 0.01 on the wire; silent ref ⇒ ref peak null/floor (kills
swapped-channel bugs). `set_drive on` ⇒ output energy present; `off` ⇒ gone within one
block; level change follows without session restart; 1.6 s keepalive silence ⇒ drive
drops, session still publishing. Server clamp: request −3 dBFS with ceiling −10 ⇒
applied level −10 (assert on wire echo). Launch: both commands against fake-audio reach
live traces in the correct view; real-adapter run required for harness green (existing
policy — sandbox lavapipe segfaults).

## 8. Issue breakdown for triage

Open one epic + four sub-issues (M4a..M4d), each carrying its §7 block verbatim as the
spec. Labels: epic → `epic`; M4a `ready-to-implement` after architect ACK of §4/§6
(label `needs-design` first — wire additions route to architect); M4b/M4c
`ready-to-implement`, blocked-by chain a→b→c→d; M4d `needs-design` (wire) + `drive-path`
(if the label exists after the .agents PR). Dependencies: M4b needs M4a; M4c needs M4b
(key table); M4d daemon half is parallel to M4b/M4c but its CLI half needs M4c.

## 9. Hard constraints (repeat offenders — enforce, don't trust)

- No trig, no log10, no formatting, no measurement values born in ac-view.
- No dB round-trips: linear→dB happens once (existing single conversion site pattern).
- No forbidden keys; no new keys outside the single M4b assignment pass.
- Sessions launch drive-off. No agent may weaken any drive-safety AC to make a test
  pass — route to architect instead.
- Agents do not close issues, merge PRs, or edit `.agents/*`; Markus ratifies.
- Verify file paths/LOC from the repo, not from this document's orientation section,
  before planning edits.

## 10. Out of scope

Waterfall/Scope/Goniometer, SpectrumEmber merge, spectrum-view smoothing/averaging,
touch targets, second theme, coherence-threshold UI, continuous delay re-estimation,
remote-daemon stimulus-channel config sync (V1 assumes localhost/shared config; only
`set_drive.level_dbfs` travels over CTRL), RMS meter segments.
