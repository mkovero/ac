# handoff-ac-view-repaint — M3 fix: live view sluggish / stale

Parent: `work/handoff/handoff-ac-view.md` (branch `m3-ac-view`). Found by hand-running
the merged UI: the live spectrum updates smoothly only while the mouse
moves over the window; at rest it's sluggish on both NVIDIA and Intel
(so not GPU — the NVIDIA `BadValue` was a stale driver, fixed by reboot;
that separate handoff is moot). All changes are in `ac-view/src/app.rs`;
no numeric, wire, `ac-scene`, or `ac-core` changes.

## Root cause

Two compounding bugs in the `eframe::App::ui` loop:

1. **Passive repaint = 10 fps at rest.** `ctx.request_repaint_after(100ms)`
   only schedules a lazy wake; mouse movement floods input events that
   force immediate repaints, which is why it's smooth only under the
   mouse.
2. **One frame drained per repaint = growing latency.** `poll_frame` runs
   only inside `ui()`, and `ui()` runs only on repaint, so the DATA SUB
   socket is drained at most once per repaint cycle. The daemon publishes
   faster than that; frames queue and the display falls progressively
   behind — stale, not just slow.

## Fix (both in `app.rs`)

1. **Drain to newest each pass.** Replace the single `if let Some(frame)
   = session.poll_frame(...)` with a `while let` loop at 0 ms timeout;
   since `self.scene` is overwritten each iteration it naturally keeps
   the freshest frame and discards the backlog (correct for a live
   display). For efficiency, parse/discard in the loop and construct the
   `Scene` **once** after it, from the last `WireFrame` — don't build a
   scene per backlog frame.
2. **Continuous repaint while live, lazy when idle.**
   ```rust
   if self.session.is_some() { ctx.request_repaint(); }
   else { ctx.request_repaint_after(Duration::from_millis(250)); }
   ```
   Continuous repaint paces to vsync (~60 fps) while streaming and drops
   to lazy when there's no session, so a static screen doesn't burn CPU.

## Latent bug to fix in the same pass (not perf, correctness)

Scene construction is currently tied to **frame arrival**, so zoom/pan
(`handle_action` mutates `state.freq_range`/`db_range`) produces no
visible change until the next frame rebuilds the scene — zoom appears
frozen on a paused or slow stream. Cache the last `WireFrame` and rebuild
the scene when **either** a new frame arrives **or** the range changed.
Continuous repaint hides this for fake-audio, but it will bite on a
paused/snapshot scene. Fix now while in the file.

## Acceptance criteria

1. Live fake-audio view updates smoothly (~display refresh) with **no
   mouse movement**, on both NVIDIA and Intel.
2. No growing latency under a sustained stream: displayed frame tracks
   the newest published frame (drain-to-latest confirmed — e.g. a
   time-marked stimulus shows current, not lagging, content).
3. Zoom/pan updates the view immediately even when no new frame has
   arrived (paused-stream or snapshot scene).
4. Idle (no session) does not peg a CPU core — lazy repaint confirmed.
5. Workspace green, clippy `-D warnings`, fmt; zero edits to pre-existing
   assertions.

## Out of scope

- Any `ac-scene`/`ac-core`/wire change. If scene construction proves too
  heavy for 60 fps (measure first — ~480 columns, likely fine), the only
  sanctioned response is building the scene once per drain, not moving
  math out of `ac-scene`.
- Trace colors (separate UX item).

## Routing

Developer fix. No architect/UX gate (no contract or visible-value
surface). QA: a glance that AC2 (drain-to-latest) has a test — the one
behavior here worth guarding against regression.
