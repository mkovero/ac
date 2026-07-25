# Plan: `ac transfer` field view + `ac monitor` UI launch

Status: v3 — FINAL. All decision points ratified by Markus; no open questions remain.
Ready for the architect gate.

## 0. Decisions taken (this thread)

- D-A  "Peak" was the reference spectrum trace. No peak feature exists; none is built.
- D-B  Two commands, two views, one binary:
       - `ac monitor`  → `ac-view` in **Spectrum** view (the M0–M3 deliverable, kept as-is:
         meas + ref spectra, SPL/cursor readouts, snapshot flow).
       - `ac transfer` → `ac-view` in **Transfer** view (new): H magnitude over H phase,
         coherence-gated, broadband, live. No spectrum traces of any kind here.
       Both replace the current TUI-launch path; TUI survives as `ac monitor --tui` (ssh).
       No view-switch key — the two commands are different instruments (bench vs field),
       and the view is fixed at launch. Ratified.
- D-C  Delay: daemon's existing broadband `delay_ms` shown as-is (ms + meters, c = 343 m/s).
       Field method is per-path runs (mute down to tops or subs), where broadband xcorr is
       trustworthy. No band-limited delay computation. Zero daemon diff for this.
- D-D  Phase drawn **delay-removed by default**, de-rotated by the session's own frozen
       delay estimate. Frozen is a feature: DSP delay changes appear as visible phase tilt
       instead of being re-absorbed by re-estimation. Key toggles raw phase.
- D-E  Coherence gating **on**: columns with coherence < 0.5 blanked/faded on both panes.
       Fixed threshold, no tuning UI in V1.
- D-F  Tops-vs-subs alignment uses the existing snapshot mechanism: snapshot the tops run
       (dashed), watch live sub phase against it, turn the DSP knob until they lie together.
       Requirement this creates: the snapshot carries its delay, and live + snapshot phase
       must be de-rotatable by a **common** delay (the snapshot's) for the overlay to mean
       anything. Scene-level concern, spec'd in M4a.
- D-G  Stimulus control in scope, arm→fire→panic semantics (§2).
- D-H  Persistence: last writer wins, one file. UI writes the same
       `input_channel`/`reference_channel`/`output_channel` fields `ac setup` writes.
       No UI-prefs file for channels.

## 1. Requirements

R1  Transfer view: magnitude pane over phase pane, shared log-f axis, legible at arm's
    length. Ember = live meas H; dashed = snapshot H. No ref trace, no spectrum trace.
R2  Delay readout always on in transfer view: `{delay_ms:.2} ms  ({m:.2} m)`. Formatting
    lives in `ac-scene` per display-truth discipline.
R3  Optional readouts/toggles, all single-key, defaults chosen for minimalism:
    raw/de-rotated phase (default de-rotated), coherence gate (default on),
    SPL readout (default off in transfer view). Spectrum view keeps its M0–M3 defaults;
    ref-trace visibility toggle added there (default on — it's the loopback sanity check).
R4  Channel selection overlay in the transfer view; changing channels relaunches the session
    immediately and persists per D-H.
R5  Stimulus per §2.
R6  Finnish layout: no `[` `]` `+` `-`; enforced by existing `assert_no_forbidden_keys`.
R7  Exact key letters are assigned once, at M4b, over the full table (old + new actions
    together) — not incrementally, to avoid collisions. Constraints: stimulus uses
    Space/Enter/Esc only; Q stays Quit; S stays snapshot.

## 2. Stimulus: arm → fire → panic

- Idle: Space **arms**. Armed: Enter **starts** pink-noise drive. Driving: **Space or
  Enter or Esc stops immediately** — a panic mash of anything in that cluster kills the
  noise; no arming needed to stop.
- Auto-disarm after 5 s of no keypress in the armed state; ↑/↓ (level) resets the timer.
  Esc or any key outside {Enter, ↑, ↓} disarms.
- **Level via ↑/↓ while armed and while driving** (ratified), 1 dB steps (3 dB with
  Shift), clamped to a config-set ceiling. Overlay still sets the starting level.
- **State banner, large font, impossible to miss** (top-center, both armed and driving):
  `ARMED → OUT 3 (Fireface400:AN3)  −20.0 dBFS — Enter starts, Esc cancels`
  `DRIVING  OUT 3 (Fireface400:AN3)  −20.0 dBFS — any of Space/Enter/Esc stops`
  Channel number always; sticky JACK port name appended when configured
  (`output_port`). Banner text is an `ac-scene`-formatted string per discipline.
- Transport: new CTRL command `set_drive {on|off, level_dbfs}` polled by the running
  transfer worker — instant stop/start/level without session relaunch. One deliberate
  wire addition. Ratified.
- Dead-man: UI pings keepalive every 250 ms while driving; worker drops drive after
  **1.5 s** without one. A stall/hiccup kills the noise — fail-safe direction, accepted
  cost is an occasional silent restart via re-arm.
- Stimulus keys live in the UI; sessions always launch with drive off.

## 3. Milestones

### M4a — `ac-scene`: transfer scene (pure)
- `WireFrame` gains `freqs` (H grid), `magnitude_db`, `phase_deg`, `coherence`,
  `delay_ms`, `delay_samples` — already on the wire, currently unmodelled.
- `Scene` gains: mag trace + phase trace (normalized), per-column coherence mask,
  delay readout string (ms + m), phase de-rotation `φ' = φ + 360·f·τ` (wrapped to
  ±180) with τ chosen by caller: this session's, or a snapshot's (D-F), or 0 (raw).
- Snapshot: `.acsnap` derivation exposes its stored delay so the overlay can de-rotate
  live + snapshot by the common τ.
- Renderer discipline unchanged; `computes_nothing` extended (no `log10`, no formatting,
  no trig in `ac-view`).
- AC (falsifiable, QA re-derives): fixture H = pure delay τ₀ + gain g →
  (1) delay string exact; (2) de-rotated phase ≡ 0° at every column (kills sign/wrap
  errors — a wrong sign gives 2× slope, observably nonzero); (3) raw phase matches the
  hand-derived wrap pattern; (4) coherence mask blanks exactly the columns of a fixture
  with one low-coherence band; (5) common-τ overlay: two frames with delays τ₁≠τ₂
  de-rotated by τ₁ must show frame-2 residual slope 360·f·(τ₂−τ₁), not 0.

### M4b — `ac-view`: Transfer view + toggles + key table
- `ViewKind::Transfer` as a new match arm; shell untouched. Two stacked panes, shared
  freq axis, blanked columns rendered as gaps (not zero-lines — a gap can't be misread
  as data).
- Full key table laid out in one pass (R7): raw-phase toggle, coherence-gate toggle,
  ref-trace toggle (Spectrum), settings overlay, stimulus cluster, plus all M0–M3 keys.
  `assert_no_forbidden_keys` + uniqueness tests extended by existing mechanism.
- No view-switch key: the view is fixed by the launch command (D-B). Ratified.
- AC: scene-accessor tests prove each toggle changes what would be drawn; help overlay
  lists every binding; per-view key tables contain no dead keys.

### M4c — channels + persistence + stimulus UI
- `ac-view` reads `input_channel`/`reference_channel` from config at launch; hardcoded
  `0/1` removed. Missing `reference_channel` is an error with a one-line fix hint
  (`ac setup reference <N>`), not a guess — transfer-only session model makes it required.
- Settings overlay: meas / ref / stimulus-out rows + drive level; ↑↓ row, ←→ value,
  Enter applies (session relaunch + `Config::save`), Esc cancels clean.
- Stimulus state machine per §2: arm/fire/panic, ↑/↓ level with ceiling clamp,
  banner strings, 250 ms keepalive ping while driving.
- AC: last-writer-wins round-trip both orders (UI→setup, setup→UI); overlay cancel has
  zero side effects; arm state auto-expires; stop issued from armed *and* driving states;
  headless state-machine tests (no window needed).

### M4d — daemon: `set_drive` + dead-man; CLI launch wiring
- Daemon: `set_drive` CTRL handler (on/off/level) + worker-side flag poll + 1.5 s
  keepalive drop. AC: fake-audio integration test — drive on → output energy present;
  drive off → gone within one block; level change → amplitude follows without session
  restart; keepalive silence 1.5 s → drive drops, session keeps running.
- CLI: `ac monitor` / `ac transfer` spawn `ac-view` with view kind + endpoint from cfg
  (honoring `server_host`); `ac monitor --tui` keeps ratatui. `ac monitor` channel args
  map to meas channel as today.
- AC: both commands against fake-audio daemon reach live traces in the correct view
  (real-adapter run required for harness green, per policy); `--tui` still renders.

Order M4a→M4d, standard architect → developer → QA → UX gates. Nothing is blocked;
every decision point in this document is ratified.

## 4. Known constraint accepted
Remote daemon: UI writes client-local config while the daemon reads its own for
`output_channel`. V1 assumes localhost / shared config; `set_drive.level_dbfs` is the
only stimulus parameter that travels over CTRL. Revisit only if remote field use appears.

## 5. Out of scope
- Waterfall/Scope/Goniometer, SpectrumEmber merge — separate track.
- Touch targets, second theme, live level keys, coherence-threshold UI.
- Any smoothing/averaging of spectrum view (unchanged from M0–M3; raise separately if
  the jagged floor bothers you on the bench).
- Continuous delay re-estimation (frozen-τ is the correct behavior for D-D/D-F).
