# .agents/ changes for the field-transfer workstream

One PR, three edits, one flag. Each edit is exact text to insert; you ratify in PR review
per the human-gates rule.

---

## Edit 1 — `qa.md`: extend the value-display class + add a drive-safety checklist

**(a)** In the value-display gate section (the "[PENDING A3]" block, ~line 264), extend the
enumeration of value-display data:

> spectrum/waterfall/ember/scope trace data, axis …

becomes

> spectrum/waterfall/ember/scope trace data, **transfer magnitude/phase traces, the
> coherence mask, the delay readout (ms and meters), input-level meter heights and clip
> latch, and stimulus banner strings**, axis …

Rationale: these are all `ac-scene`-computed display values; a PR changing any of them is
a value-display PR and must gate identically. Note for QA: `ac-scene`'s display-truth
fixture tests are the enforcement mechanism for scene-computed values (pure crate, no
harness needed); the rendering harness gate applies only to `ac-view` drawing changes.

**(b)** Append a new checklist section:

> ### drive-path safety (any PR touching stimulus/`set_drive`)
> Do not approve unless ALL of the following are demonstrated by tests, not by reading:
> - [ ] Sessions launch with drive **off**; no code path starts drive without an explicit
>       `set_drive on`.
> - [ ] Panic stop works from BOTH armed and driving states (state-machine tests).
> - [ ] Dead-man: drive drops within 1.5 s of keepalive silence (integration test,
>       fake-audio); the session itself keeps running.
> - [ ] Level is clamped to `drive_max_dbfs` at every entry point (arrow keys, overlay,
>       CTRL command) — test each entry point, not one representative.
> - [ ] `set_drive off` silences output within one audio block (fake-audio energy test).

## Edit 2 — `ux.md`: stimulus visibility + meters join the review scope

Append to the review-scope section:

> ### stimulus state visibility (transfer view)
> The ARMED and DRIVING banners are safety UI, not chrome. Review requirements:
> - Large type, top-center, cannot be occluded by any overlay except help.
> - Banner names the output (channel number + sticky JACK port when configured) and the
>   current level in dBFS. Verbatim `ac-scene` strings — reject any reformatting in
>   `ac-view`.
> - DRIVING must be visually louder than ARMED. Ember principle applies: the driving
>   state may use the signal color; never green (success baggage — "noise blasting" is
>   not success feedback).
> - Input-level meters (transfer view only): two thin bars, right edge, M above/left of
>   R, raw dBFS, peak-hold tick, red clip latch. They are health indicators — always on,
>   not part of the toggle set; reject PRs adding a toggle for them.

## Edit 3 — `AGENTS.md`: one routing line + one label

In "routing logic", under the existing tree, add:

> PRs touching stimulus/drive (`set_drive`, arm/fire state machine, keepalive):
>   apply `drive-path` → qa uses the drive-path safety checklist; wire-protocol side
>   routes to architect as usual.

In the label schema table, add:

> | `drive-path` | triage or developer | stimulus/drive safety checklist applies |

## Flag (no edit in this PR) — stale module maps

`architect.md` and `developer.md` still carry the pre-rewrite module map
(`ac/src/main.rs, estimator.rs, session.rs…`). The handoff below is self-contained
precisely so agents don't need those maps, but they will mislead any agent that reads
them for orientation. Recommend a separate housekeeping PR regenerating both maps from
the current workspace (`ac-rs/crates/{ac-core,ac-daemon,ac-cli,ac-scene,ac-view}`).
Kept out of this PR to keep the ratification diff reviewable.
