# handoff-ac-view-nvidia-startup — M3 fix: glutin BadValue on NVIDIA

Parent: `handoff-ac-view.md` (M3, branch `m3-ac-view`). This is a
startup defect found by hand-running the built UI, not caught by any
gate (GPU context creation is outside the headless harness — same class
as the retired lavapipe fear, opposite conclusion). Small fix, real bug,
mainstream config.

## Symptom

On an NVIDIA desktop, `cargo run -p ac-view` aborts at window creation:

```
Error: Glutin(Error { raw_code: Some(2),
  raw_os_message: Some("BadValue (integer parameter out of range for operation)"),
  kind: BadAttribute })
```

Same crate opens fine on an Intel (Mesa) laptop. No frame is ever drawn;
this is GL config/context negotiation, before any rendering.

## Triage already done (do not redo — this is the conclusion, not a start)

| probe | result | meaning |
|---|---|---|
| `__GLX_VENDOR_LIBRARY_NAME=mesa cargo run -p ac-view` | **works** | Mesa's GLX accepts the requested config |
| `__GLX_VENDOR_LIBRARY_NAME=nvidia` (native) | **fails**, same error | NVIDIA driver rejects it |
| `LIBGL_ALWAYS_SOFTWARE=1` | **fails**, same error | llvmpipe *also* rejects it |
| `WINIT_UNIX_BACKEND=x11` | no change | not a Wayland issue |

The load-bearing result is the third row: software rendering rejecting
the same attributes NVIDIA rejects means the requested config is
genuinely out-of-range, and only Mesa's NVIDIA-GLX path is lenient
enough to accept it. **This is not "force a backend" — it's an
over-strict glutin config template in `ac-view`'s own window setup.**
Env overrides are an unblock, not the fix.

## Fix

Loosen what the eframe/glutin setup *demands* so the driver can pick a
compatible framebuffer config instead of being handed an unsatisfiable
one. In `ac-view`'s `NativeOptions` / `ConfigTemplateBuilder`, check for
and relax, in order of likelihood:

1. `hardware_acceleration: HardwareAcceleration::Preferred` — **not
   `Required`.** `Required` failing while the software path also fails
   fits this symptom best; start here.
2. `multisampling: 0` unless a feature needs MSAA (none in V1).
3. `depth_buffer: 0`, `stencil_buffer: 0` unless actually used (a 2D
   spectrum polyline uses neither).
4. If `transparent(true)` is set, try `false` — transparent
   framebuffers are a classic NVIDIA/GLX `BadValue`.

Flip #1 first and re-test native NVIDIA; it likely resolves it alone.
Apply the rest as needed until the NVIDIA-native path opens *without*
any `__GLX_VENDOR_LIBRARY_NAME` override.

## Reproduction target

Reproducible on the NVIDIA machine at **192.168.9.25** (already keyed
for SSH). Two constraints or you'll chase a phantom:

- Run against the machine's **local display**, not forwarded X. Plain
  `ssh -X` gives indirect GLX, which has its own unrelated `BadValue`
  modes — reproduce on a real local/VNC session (`DISPLAY=:0` with
  proper perms), not over forwarding.
- Confirm the fix by running with **no GLX env overrides at all** — a
  pass under `__GLX_VENDOR_LIBRARY_NAME=mesa` proves nothing about the
  fix.

## Acceptance criteria

1. `cargo run -p ac-view` opens on NVIDIA-native (no env overrides) and
   still opens on the Intel/Mesa laptop — both, not one.
2. Each loosened `NativeOptions` field carries a one-line comment on
   *why* it's permissive (the "future change violates it on purpose"
   discipline — so a later "require hardware accel for performance"
   edit has something explaining why it breaks NVIDIA). Same pattern QA
   applied to the SPL layer-topology doc.
3. Xvfb + glow + release run (QA's existing A3 recipe) still clean —
   the loosening must not regress the headless path.
4. Workspace green, clippy `-D warnings`, fmt; zero edits to
   pre-existing assertions.

## Out of scope

- No wgpu (crate is on glow; keep it).
- No env-var workarounds baked into code or launch scripts — the config
  template is the fix.
- No new rendering features; this is startup only.

## Docs

Update the environment note in `issues.md` (the one that retired the
lavapipe/wgpu fear): the known-good recipe is glow with a **permissive**
GLX config template; NVIDIA-native `BadValue` was an over-strict
template, not a hardware limit. Add the multi-GPU run note to the
eventual `ac-view` README/quickstart.

## Routing

Architect: none needed (no wire/contract surface). UX: none (no visible
change on success). Straight developer fix + a confirming run on
192.168.9.25.
