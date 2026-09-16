# <session name> — rig record

<!--
Copy to work/rig/<session-name>-results.md. One file per session; when
definitions change (config, gate constant, metric), start a new file.
Required fields come from .agents/rig.md step 4 and its hard constraints —
an empty required field is a defect in the record, not a clean run.
Paste script output blocks verbatim; do not summarise them.
-->

Date (UTC): <yyyy-mm-dd>  ·  Rig: <name> (docs/rigs/<name>.md)  ·  Operator: <who>

## Build under test

<!-- paste scripts/rig/ship.sh's "build under test" block -->

## Pre-flight

<!-- paste scripts/rig/preflight.sh <rig> <rev> output; explain every FAIL -->

## Physically connected (confirmed this session)

<!-- every leg by output/input index. Paste probe-outputs.sh output if the
     wiring was probed; otherwise say why it was not and what was confirmed
     instead. -->

## Clock state

<!-- clock source and why, restated even when unchanged -->

## Emission consent

- Consented by / when:
- Scope (outputs, stimulus, level, duration):
- Ceiling and provenance (standing −40 dBFS or the rig's speaker ceiling,
  or a recorded exception; enforced by the script's `--level` check — the
  daemon has no ceiling below 0 dBFS since #459):

## Background noise

<!-- paste scripts/rig/noise-snapshot.sh output (acoustic sessions) -->

## Runs

### Run 1 — <what is verified>

- **Pass looks like (stated before running):**
- **Command:**
- **Output:** <!-- verbatim block -->
- **Result:** pass / fail / decline to conclude — <what is unresolved if declined>
- **Confound:** <!-- required; "none identified" if none -->

## Rig state left behind

<!-- JACK settings, clock, gains, phantom, what is still running, which
     config files were touched or deliberately left alone, where the mic is -->

## What should happen next

<!-- ordered by what blocks what -->
