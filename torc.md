# RC-UX-PLAN — release-candidate UX tightening for ac-ui / ac-cli

Status: planning — to be sliced into 11 GitHub issues, one PR each.

## 0. Decisions (binding)

| Q | Decision |
|---|----------|
| FFT-line `Spectrum` view | Hide from W cycle, keep `--view spectrum` working (1a). Empirical investigation continues against the old renderer. |
| `[`/`]` and `+`/`-` for dB span/floor | Drop entirely. Scroll + Ctrl+Shift+Scroll cover the same ground. |
| `Nyquist` view | Stays in default cycle. Important to get along with. |

## 1. Default W cycle (binding) — 9 ember-substrate slots

`SpectrumEmber → Goniometer → IoTransfer → BodeMag → Coherence → BodePhase → GroupDelay → Nyquist → Ir → SpectrumEmber`

Hidden but reachable via `ac-ui --view <name>`:

- `spectrum` — old wgpu line plot. Investigation target. Keep parser entry.
- `waterfall` — old wgpu colormap (FFT/CWT/CQT/reassigned). Keep parser entry.
- `scope` — synthetic 1 kHz validation view. Keep parser entry.

The `ViewMode` enum stays unchanged. The parser in `ac-ui/src/main.rs::parse_view_mode` keeps every existing token. Only the `W`-key cycle and the persisted-view fallback change.

## 2. Scroll-zoom rules (binding)

| Modifier on cell scroll | Action |
|---|---|
| (none) | Zoom both axes toward the cursor |
| Shift | Zoom freq-axis (X) only |
| Ctrl | Zoom Y-axis (dB / amplitude / time-rows) only |
| Ctrl+Shift | Pan dB window ±2 dB/tick (gain trim) — unchanged |

Scroll on grid background continues to resize cells.

For views with no axes (`Goniometer`, `IoTransfer`, `Coherence`, `Nyquist`): plain scroll prints a one-shot notification `no zoom on <view>` rather than silently swallowing.

## 3. Box zoom (binding)

Removed entirely. Right-button:

- Click (no drag) → reset hovered cell view (kept).
- Drag → unbound. The rubber-band overlay, `BoxZoomState`, and `begin/update/end_box_zoom` are deleted.

## 4. Bottom keytip strip (binding)

Single monospace line at screen-bottom, always visible, listing 3–6 contextual chips for the current view. Each chip pairs the key with its current state (e.g. `A weighting:Z`, `O smooth:1/6`). Universal chips (`H help`, `Esc quit`) appear at the right edge.

Hover readout moves up by one line height to avoid collision; the `● connected` indicator moves to the far right of the keytip strip.

### Per-view keytip table

| View | Keytips shown |
|---|---|
| `SpectrumEmber` | `A` weighting · `I` avg · `O` smooth · `P/M` peak/min · `,/.` ember · `W` view |
| `Waterfall` (only when reached via `--view`) | `A` weighting · `O` smooth · `↑↓` FFT N · `←→` interval · `;` palette · `W` view |
| `Scope` (only via `--view`) | `,/.` intensity · `Sh+,/.` τ_p · `Z` clear · `W` view |
| `Goniometer` | `R` M/S · `,/.` ember · `Z` clear · `W` view |
| `IoTransfer` | `,/.` ember · `Z` clear · `W` view |
| `BodeMag` | `K` γ²-weight · `O` smooth · `Z` clear · `T` add transfer · `W` view |
| `BodePhase` | `K` γ²-weight · `T` transfer · `Z` clear · `W` view |
| `Coherence` | `K` γ²-weight · `T` transfer · `W` view |
| `GroupDelay` | `K` γ²-weight · `T` transfer · `W` view |
| `Nyquist` | `K` γ²-weight · `T` transfer · `W` view |
| `Ir` | `T` transfer · `Z` clear · `W` view |

Universal: `H help` · `Esc quit` · `S screenshot`.

## 5. GPU-init failure → headless fallback (binding)

`RenderContext::new` returning `Err` no longer panics. `ac-ui` exits with status 71 (`EX_OSERR`).

The CLI side:

- `ac monitor` — synchronously waits on `ac-ui` (was: spawn-and-forget). On exit code 71 (or `ac-ui not found`), drop into the TUI monitor loop.
- `ac plot show` — spawns `ac-ui` async, runs a 200 ms `try_wait()` poll before sending the CTRL command. On early exit with code 71, log a warning; the inline CSV-style output continues unchanged. No TUI fallback needed (inline output is already there).

## 6. TUI monitor (binding)

When `ac monitor` falls back, run a `htop`-style refreshing display:

```
ac monitor — synthetic — 2 channels — Ctrl+C to exit
─────────────────────────────────────────────────────────────────────
CH0   peak  -12.4 dBFS @  1000 Hz   floor  -84 dB    weight:Z   avg:off
CH1   peak   -3.1 dBFS @   997 Hz   floor  -78 dB    weight:Z   avg:off
─────────────────────────────────────────────────────────────────────
fft N=8192   interval=43 ms   xruns=0
```

Refreshes at the daemon's monitor interval, in place (ANSI cursor return). Ctrl+C sends `stop` and exits cleanly. No keybindings — pure read-only display.

## 7. Help overlay (binding)

Trim `HELP_LINES` to ≤30 lines. Remove every reference to box zoom, the `[`/`]` and `+`/`-` keys, and Shift+Scroll palette. Reorganize by view family (Spectrum-like / Trajectory / Transfer / Universal).

---

# Issue list — 11 PRs, sequenced

Each issue must:

- Compile clean and pass `cd ac-rs && cargo test`.
- Pass `pytest tests/ -q` (no daemon-protocol changes are involved here).
- Update `ac-rs/CLAUDE.md` keymap table for any binding change.
- NOT touch `app.rs` god-object refactor (issue #29 territory — out of scope).

Counterpart should verify file/line references before editing — they may have drifted since this plan was written. `grep` for the symbol name, do not trust line numbers.

---

## Issue RC-1 — Drop box zoom

**Goal:** remove right-drag rubber-band zoom in its entirety.

**Files:**

- `ac-ui/src/app/input.rs` — delete `BoxZoomState` struct, `begin_box_zoom`, `update_box_zoom`, `end_box_zoom`.
- `ac-ui/src/app.rs` — remove the `box_zoom` field from `App`, the right-button-press / cursor-moved / right-button-release dispatch arms.
- `ac-ui/src/ui/overlay.rs` — remove rubber-band rect rendering (search for any reference to `box_zoom`).

**Right-click behaviour:** falls through to the existing `reset_hovered_view` (which `end_box_zoom` already called for sub-5 px movement). Net change: right-click resets always.

**Acceptance:**

- `cargo test -p ac-ui` green.
- Manual: `cargo run -p ac-ui -- --synthetic --channels 2`, right-drag produces no overlay rectangle, right-click on cell resets it.
- Help overlay's box-zoom line is removed in RC-9; not in this PR.

**Out of scope:** scroll-zoom changes (RC-2), help text (RC-9).

---

## Issue RC-2 — Scroll = global zoom; Shift=X-only, Ctrl=Y-only

**Goal:** establish the binding scroll-zoom rules from §2.

**Files:**

- `ac-ui/src/app/input.rs::apply_zoom` (~lines 165–374). Rewrite the modifier-decision block.

**Behavioural change:**

- Spectrum-family today: plain=both, Shift=freq-only, Ctrl=dB-only. Already matches §2 — verify and keep.
- Waterfall today: plain=freq, Ctrl=time. Change to plain=both, Shift=freq, Ctrl=time.
- Scope today: plain=y-gain, Ctrl=window. Change to plain=window, Ctrl=y-gain.
- Goniometer/IoTransfer/Coherence/Nyquist today: silently swallow. Change to one-shot `notify("no zoom on <view>")` and return. Throttle: only emit once per 2 s of continuous scroll on the same view.
- Ir today: silently swallow. Change to plain=both, Shift=time, Ctrl=amplitude.

**Acceptance:**

- `cargo test -p ac-ui` green.
- `--synthetic --channels 2`: cycling W and scrolling on each view produces a notification or visible zoom matching §2.
- A new unit test in `ac-ui/src/app/input.rs` for the modifier-decision block (mock `App` state, assert `(zoom_freq, zoom_y, zoom_time)` tuple for each (view, modifiers) combo).

**Out of scope:** palette cycle remap (RC-3), W-cycle change (RC-4).

---

## Issue RC-3 — Move Waterfall palette cycle off Shift+Scroll

**Goal:** free Shift+Scroll for the new "freq-only zoom" rule (established in RC-2). Bind palette cycle to a key.

**Files:**

- `ac-ui/src/app/input.rs::apply_zoom` — delete the Shift+Scroll palette branch at the top.
- `ac-ui/src/app/input.rs::handle_key` — add a new arm. Suggested key: `Semicolon` (uncluttered, Finnish-friendly). Ctrl+Semicolon cycles backward.
- `ac-rs/CLAUDE.md` — keybinding table update.

**Behavioural change:** palette only changes when explicitly cycled by key. Default palette stays inferno.

**Acceptance:**

- `cargo test -p ac-ui` green.
- `--synthetic --view waterfall`: pressing `;` cycles inferno → magma → inferno; Ctrl+`;` reverses; Shift+Scroll now does freq-only zoom per RC-2.

**Out of scope:** anything outside `apply_zoom` palette block and `handle_key`.

---

## Issue RC-4 — Trim W cycle to 9 ember slots; persist-migrate hidden views

**Goal:** apply §1 binding cycle. Migrate stale `ui.json`.

**Files:**

- `ac-ui/src/app/input.rs::handle_key KeyCode::KeyW` — rewrite cycle. Delete `WSlot::Matrix`, `Single`, `Waterfall`, `Cwt`, `Cqt`, `Reassigned`, `Scope` from the cycle. Keep them callable from `--view`.
- `ac-ui/src/app/input.rs::current_w_slot` — rewrite to map only the 9 ember views. The hidden views map to `None`, which the cycle treats as "jump to SpectrumEmber" (deterministic landing).
- `ac-ui/src/data/persist.rs::load` — when `view_mode` parses to `Spectrum`, `Waterfall`, `Scope`: log `info: persisted view '{x}' is no longer in the default cycle, using SpectrumEmber`, return `SpectrumEmber`. Do not rewrite the on-disk token until the user actively changes the view, so a future revert doesn't lose state.
- `ac-rs/CLAUDE.md` — update the "W cycles view" line.

**Acceptance:**

- `cargo test -p ac-ui` green; add a unit test for the migration in `data/persist.rs` (write `view_mode: "waterfall"`, load, assert `ViewMode::SpectrumEmber`).
- `--synthetic --view waterfall` still works (UI opens in waterfall).
- Repeated W from any starting point cycles through 9 views and returns to start within 9 presses.

**Out of scope:** keytip strip (RC-8), help text (RC-9).

---

## Issue RC-5 — Typed `LaunchKind`; spawn ac-ui after CTRL ack

**Goal:** make `launch_ui` take a typed enum, and only spawn the UI after the daemon has acknowledged the request.

**Files:**

- `ac-cli/src/commands/plot.rs` — define `pub enum LaunchKind { SweepFreq, SweepLevel, Monitor }`. Replace `mode: &str` arg in `launch_ui`. Move the spawn call to after `check_ack(...)` returns successfully.
- `ac-cli/src/commands/monitor.rs` — call `launch_ui(LaunchKind::Monitor, ...)`.
- `ac-ui/src/main.rs` — keep `--view` and `--mode` flags as-is. The CLI already uses `--view` for monitor and `--mode` for sweep; only the type at the CLI side changes.

**Behavioural change:**

- If `ac plot ... show` and the daemon refuses the command (busy, invalid args), the UI window no longer flashes open then disconnects.
- `launch_ui` argument list is grep-friendly (`LaunchKind::Monitor` instead of stringly-typed `"spectrum"`).

**Acceptance:**

- `cargo test -p ac-cli` green.
- Manual: `ac plot 1khz 1khz 0dbu show` while the daemon already runs another worker → CLI prints error, no UI window appears.

**Out of scope:** GPU fallback (RC-6), TUI mode (RC-7).

---

## Issue RC-6 — `ac-ui` exits 71 on wgpu init failure

**Goal:** stop panicking when there's no GPU adapter.

**Files:**

- `ac-ui/src/app.rs::init_graphics` — replace `.expect("wgpu init")` with a `match`. On error: `log::error!`, set `self.gpu_init_failed = true`, request `elwt.exit()`.
- `ac-ui/src/main.rs` — after `event_loop.run_app(&mut app)?` returns, check `app.gpu_init_failed`. If set, exit with `std::process::exit(71)`.

**Behavioural change:**

- Headless servers, broken drivers, missing Vulkan/GL → `ac-ui` exits 71 with a single error line, instead of `panicked at src/app.rs:754`.

**Acceptance:**

- `cargo test -p ac-ui` green.
- Manual: `WGPU_BACKEND=fakebackend ac-ui` (or unset DISPLAY in a headless container) → exits 71, prints `error: failed to initialize wgpu adapter (...)`.
- A unit test that calls `RenderContext::new` with a deliberately broken adapter request would be ideal but may need feature-gating; acceptable to ship with manual-only verification if no clean path exists.

**Out of scope:** what the CLI does with exit 71 (RC-7).

---

## Issue RC-7 — `ac monitor` headless TUI mode + auto-fallback when ac-ui exits 71

**Goal:** §6 binding TUI monitor.

**Files:**

- new `ac-cli/src/commands/monitor_tui.rs` — implement the refresh loop. Subscribe to DATA topic, parse `visualize/spectrum_*` frames, compute broadband stats per channel (reuse `ui::fmt::broadband_stats` pattern but in CLI land — copy the logic, do not share code yet). Render with ANSI escapes (`\x1b[2J\x1b[H` on first frame; `\x1b[H` per refresh). Trap Ctrl+C via `ctrlc` crate (or whatever signal handling already exists in the workspace), send `stop` over CTRL, exit clean.
- `ac-cli/src/commands/plot.rs::launch_ui` — change `Monitor` variant to `Command::status()` (synchronous wait). On exit code 71 or spawn failure → call into `monitor_tui::run`. `SweepFreq` / `SweepLevel` continue to spawn-and-forget with optional 200 ms `try_wait()` early-exit log.
- `ac-cli/Cargo.toml` — add `ctrlc = "3"` if no existing signal-handling dep is reusable. Small, well-maintained.

**Behavioural change:**

- `ac monitor` on a headless host or with no GPU runs in the terminal, refreshing in place.
- `ac monitor` on a normal host launches the GUI; closing the GUI returns control to the shell.

**Acceptance:**

- `cargo test -p ac-cli` green; add a test for the frame-rollup formatting (no audio, just JSON-in / string-out).
- Manual on Linux: `DISPLAY= ac monitor` (or `WGPU_BACKEND=invalid ac monitor`) → TUI appears, refreshes, exits cleanly on Ctrl+C.
- Manual: `ac monitor` on a normal display → GUI opens, no TUI appears.

**Out of scope:** the GUI keytip strip (RC-8); the GUI help text (RC-9). The TUI is a parallel, separate render path.

---

## Issue RC-8 — Bottom keytip strip (per-view, with current state)

**Goal:** §4 binding bottom strip in the GUI.

**Files:**

- new `ac-ui/src/ui/keytips.rs`. Public surface:

  ```
  pub struct KeytipChip { pub key: &'static str, pub label: String }

  pub struct KeytipState {
      pub view:           ViewMode,
      pub band_weighting: BandWeighting,
      pub time_integ:     TimeIntegrationMode,
      pub smoothing_frac: Option<u32>,
      pub peak_hold:      bool,
      pub min_hold:       bool,
      pub coherence_k:    f32,
      pub goniometer_ms:  bool,
  }

  pub fn keytips_for(state: &KeytipState) -> Vec<KeytipChip>;
  ```

  Implements the per-view table from §4. Universal chips (`H help`, `Esc quit`, `S screenshot`) always appended.

- `ac-ui/src/ui/overlay.rs::draw` — paint the strip at `screen.bottom() - 6.0`. Move the existing hover readout up to `screen.bottom() - 6.0 - line_h - 4.0`. Move `● connected` to the strip's right edge.
- `ac-ui/src/app/render_pipeline.rs` — populate `KeytipState` from the snapshot at the same point that builds `OverlayInput`.
- `ac-ui/src/ui/overlay.rs::OverlayInput` — add `pub keytips: &'a [KeytipChip]`.

**Acceptance:**

- `cargo test -p ac-ui` green; add unit tests for `keytips_for` — one assertion per view, verifying the chip set and that state-dependent labels (e.g. `weighting:Z` vs `weighting:A`) update.
- Manual: cycle W through all 9 views; bottom strip changes accordingly.
- Hover readout still readable; `● connected` still visible.

**Out of scope:** help overlay text (RC-9).

---

## Issue RC-9 — Trim help overlay

**Goal:** §7. Help becomes a reference card, not a tutorial.

**Files:**

- `ac-ui/src/ui/overlay.rs::HELP_LINES` — rewrite. Target ≤30 lines. Reorganize by view family. Delete every line about box zoom, `[`/`]`, `+`/`-`, Shift+Scroll palette.
- `ac-rs/CLAUDE.md` — sync the keymap table.
- `ac-ui/src/main.rs::print_help` — same trim applied to the `--help` text printed at startup.

**Acceptance:**

- `cargo test -p ac-ui` green.
- Manual: `H` in a running session shows the new compact panel.

**Out of scope:** none. This is final cleanup.

---

## Issue RC-10 — Drop `[`/`]` and `+`/`-` bindings

**Goal:** Fork-2 (a). Scroll covers it.

**Files:**

- `ac-ui/src/app/input.rs::handle_key` — delete the `BracketLeft`/`BracketRight` arms (~lines 1138–1143) and the `Equal`/`NumpadAdd`/`Minus`/`NumpadSubtract` arms (~lines 935–940).
- Verify no other site dispatches dB-floor/span shifts; the helpers `adjust_hovered_db_span`, `shift_hovered_db_floor` may now be dead code — remove if so.
- `ac-rs/CLAUDE.md` keymap, `print_help` text, `HELP_LINES`.

**Acceptance:**

- `cargo test -p ac-ui` green.
- Manual: pressing `[`, `]`, `+`, `-` does nothing; scroll-zoom and Ctrl+Shift+Scroll cover the same operations.

**Out of scope:** none.

---

## Issue RC-11 — Smoke-test matrix: all 9 default views × 3 data shapes

**Goal:** acceptance gate before tagging RC.

**Files:**

- new `ac-ui/tests/it_views_smoke.rs` (or extend an existing integration test). For each view in the default cycle, run the UI in `--synthetic --benchmark 2 --no-persist --view <name>` for a fixed duration; assert it:
  - Reaches first redraw within 1 s.
  - Produces ≥ N frames in 2 s (N tunable per view; coarse).
  - Exits clean.

  Three data shapes: `--channels 1`, `--channels 2`, `--channels 8`.

- `scripts/` — optional `scripts/rc-smoke.sh` runner that loops the matrix and prints a pass/fail summary, for human-in-the-loop pre-tag verification.

**Acceptance:**

- `cargo test -p ac-ui --test it_views_smoke` green on the dev box.
- The CI block (if/when wired) reports the matrix outcome.

**Out of scope:** test against real audio hardware. Synthetic only.

---

# Verification recipe (apply per issue)

1. `cd ac-rs && cargo test` — must stay green. Do not cite a baseline test count; just verify the count does not drop.
2. `cd ac-rs && cargo build --release` — must succeed.
3. Run `pytest tests/ -q` — no protocol changes here; should remain green.
4. Manual smoke: `--synthetic --channels 2`, exercise the change.
5. If the change touches keymap, update `ac-rs/CLAUDE.md` in the same PR.

# Out of scope for the entire RC pass

- `app.rs` god-object split (#29).
- New analysis features (Phase 4 unified.md items beyond what is already merged).
- Multi-channel daemon protocol changes.
- Daemon-side handlers refactor.
- Real-hardware verification (manual at office/workshop).

# Order of execution

```
RC-1 ──┐
RC-2 ──┼── RC-3 ──┐
       │          │
       ├── RC-4 ──┤
       │          │
       └────── RC-8 ── RC-9 ── RC-11
                       │
RC-5 ── RC-6 ── RC-7 ──┘

RC-10 (independent — last; after RC-2 has rerouted users to scroll)
```

- RC-1, RC-5 can ship in any order; they do not touch the same files.
- RC-3 must follow RC-2 (frees the modifier RC-2 needs).
- RC-4 must follow RC-3 (cycle change touches the same `handle_key`).
- RC-7 depends on RC-6 (CLI relies on exit code 71).
- RC-8/9/11 land late so they describe the final state.
- RC-10 last — wait until users have lived with scroll-zoom for a beat before pulling the keys.
