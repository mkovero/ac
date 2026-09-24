# Rig testing runbook

The one procedure for testing `ac` binaries on real hardware. Everything a
session runs is a script under `scripts/rig/`; this file says which, in what
order, and what each one proves.

| what | where |
|---|---|
| procedure (this file) | `docs/runbooks/rig-testing.md` |
| interlocks and required record fields | `.agents/rig.md` — hard constraints, step 4 |
| scripts | `scripts/rig/*.sh` (helpers in `scripts/rig/lib/`) |
| per-rig facts | `docs/rigs/<rig>.md` |
| per-rig machine profile | `scripts/rig/hosts/<rig>.env` |
| record template | `scripts/rig/record-template.md` → `work/rig/<session>-results.md` |
| live queue of what needs a rig | `$AC_HOME/rig-verify-queue.md` |

## Rigs

| rig | use | profile |
|---|---|---|
| `pupu` | **default** — audio hardware-in-the-loop: FF400, Genelec 1083, mic at 1 m, electrical reference loopback | `docs/rigs/pupu.md` |
| 192.168.9.25 | real-GPU `ac-view` snapshot tests only | TESTING.md → "A3 snapshot reference currency" |

A new rig gets both a `docs/rigs/<name>.md` and a `scripts/rig/hosts/<name>.env`.
Its address, user and SSH key go in `$AC_HOME/rig-hosts/<name>.access.env`
(`RIG_HOST`, `RIG_USER`, `RIG_SSH_KEY`), never in this repo.

## Session order

Every step names its script. Steps 1–4 emit nothing. Steps marked **EMITS**
need the operator's per-run consent first (step 5).

### 1. Build — development host

From a worktree checked out at the commit under test:

```bash
scripts/rig/build-portable.sh            # refuses uncommitted ac-rs/ changes
scripts/rig/build-portable.sh --allow-dirty   # records the dirty count instead
```

Stages `ac`, `ac-daemon`, `ir_probe`, `transfer_probe` and `it_loopback_ir`
with `MANIFEST.txt`, `SHA256SUMS` and `CARRYSUMS` in
`$AC_HOME/target-rig-stage/<rev12>/`.
What it guarantees, and why each is there:

- **One cargo target dir per commit** (`$AC_HOME/target-rig-<rev12>`). A
  target dir shared across worktrees can print `Finished` without compiling
  and hand back another ref's binaries; the manifest records whether this run
  compiled.
- **Portable CPU** (`-C target-cpu=x86-64`). `ac-rs/.cargo/config.toml` builds
  for the host CPU; rigs reject that with `SIGILL`.
- **Reproducible across commits** (`--remap-path-prefix=<target dir>=/ac-target
  -C strip=symbols`). Without them the per-commit target dir and the crate-hash
  symbol suffixes change the binaries of a commit that only edits a comment.
  The build refuses to stage if `ac`, `ac-daemon`, `ir_probe` or
  `transfer_probe` still contains the target dir. Stripped binaries keep panic
  messages with file:line; to symbolise a backtrace, rebuild the same rev
  locally, which now gives the same bytes. This is what makes
  [carry-forward](#carry-forward) possible.
- **On disk, not `/tmp`.** `/tmp` is RAM-backed; a full target dir fills it
  and fails as a link error, not a disk error.
- Never build on a rig — 192.168.9.25 is the development VM's hypervisor host.

### 2. Ship — development host → rig

```bash
scripts/rig/ship.sh pupu [rev|latest] [--install]
```

Copies the stage to `<stage_base>/<rev12>-x86_64/` on the rig, **verifies
sha256 on the rig** against `SHA256SUMS`, and links `it_loopback_ir`'s
compile-time daemon path to the shipped daemon (the test spawns
`$CARGO_TARGET_DIR/release/ac-daemon` by absolute path). `--install` stops
any `ac-daemon`, installs `ac` + `ac-daemon` to `/usr/local/bin` and verifies
those by sha256 too. Prints the record's "build under test" block.

Prefer running staged binaries over installing: the scripts put the stage
directory first on `PATH`, because `ac` looks up `ac-daemon` on `PATH` before
its own directory — a staged `ac` otherwise auto-spawns the installed daemon.
Scripts that spawn a daemon print its `/proc/<pid>/exe`.

### 3. Pre-flight — silent

```bash
scripts/rig/preflight.sh pupu [rev|latest|none]
```

JACK service, rate, period and required flags; where the analog capture
block sits (silently — the FF400's port order moves between orders, and the
emitting scripts refuse to run when it is not where the profile says); the
interface's ALSA baseline; the ac config's ceiling and channel map; running daemons; installed
hashes; the staged build's hashes and daemon link; jackd xruns in the last
10 minutes. Exit 1 on any FAIL — fix it, or record why the session proceeds.
It does **not** check wiring; that needs emission (step 6). Its ALSA rows
read the driver's cached control values, not the interface: on pupu a JACK
restart left the FF400 at driver defaults while every ALSA row still
passed. After a restart or power cycle, confirm levels by probe (step 6).

After a rig power cycle expect the interface baseline to FAIL — the restore
commands are in the rig profile.

### 4. Noise snapshot — silent, acoustic sessions

```bash
scripts/rig/noise-snapshot.sh pupu [seconds]
```

Room noise differs a lot between sessions; record it before judging any
acoustic SNR.

### 5. Consent — interlock

Before the first emitting command: the operator's explicit consent for this
run, naming outputs, stimulus, level and duration (`.agents/rig.md` hard
constraints). Every emitting script requires `--consent "<text>"` and prints
it into its output, and refuses a `--level` above the rig profile's
`RIG_DRIVE_CEILING_DBFS` — or, on any path that drives the speaker, above
`RIG_SPEAKER_CEILING_DBFS` (pupu: −50 dBFS; a −40 dBFS sweep through its
speaker is too loud). These checks are the scripts' own. Since #459 the
daemon has no configurable ceiling: it refuses anything above its fixed
full-scale (0 dBFS) maximum and never clamps, and it refuses every emitting
command while a retired `drive_max_dbfs` key is present in its config. The
rig's −40 dBFS standing limit and the speaker ceiling apply to the value you
type; `it_loopback_ir` enforces the standing limit itself on the real-port
route — see step 7c.

### 6. Wiring probe — EMITS, after any cable work

```bash
scripts/rig/probe-outputs.sh pupu --level -60 --consent "…" [--outputs "0 1"]
```

A bounded 1 kHz tone per output, all analog captures recorded; the table
shows which input sees which output. A stated layout is not evidence — on
pupu it was the reverse of the cabling once.

### 7. Tests

#### 7a. Workspace suite — development host

`cargo test --workspace` and friends: TESTING.md. No hardware.

#### 7b. Loopback IR without hardware — development host

`it_loopback_ir` exercises `plot_ir`'s real-audio path
(`JackEngine::play_and_capture`). With no port variables it self-connects
the daemon's JACK output to its input, so a dummy JACK server is enough:

```bash
jackd -d dummy -r 48000 -p 1024 &
cd ac-rs && cargo test -p ac-daemon --test it_loopback_ir -- --ignored --nocapture
```

It runs a 2.0 s Farina sweep, deconvolves, and asserts a dominant peak at
least 25 dB above the pre-impulse floor, at or after the gate centre and
within a 60 ms round-trip margin (`MAX_ROUND_TRIP_S`, `SNR_FLOOR_DB` in the
test, #341). The 2.0 s is load-bearing: shorter sweeps shrink the window the
round-trip bound is measured in, and below ~1.0 s it can no longer hold 60 ms
at all (#361). This route touches no converter — it tests the ring and the
deconvolution, not an interface.

#### 7c. Loopback IR through real ports — EMITS

```bash
scripts/rig/run-loopback-ir.sh pupu --level -40 --consent "…" [--route ref|speaker]
```

Runs the staged test with `AC_LOOPBACK_OUT` / `AC_LOOPBACK_IN` set to the
rig profile's reference loopback (default) or speaker → mic, and
`AC_LOOPBACK_LEVEL_DBFS` from `--level`. The printed record block carries the
chain, sample rate, window, peak index and magnitude, floor, SNR and the
peak's offset from window centre — the chain's round trip. It is printed
before the assertions, so a failing run still leaves its numbers.

The test spawns its daemon under an isolated `HOME` whose config carries no
`drive_max_dbfs` key (#459). On the real-port route it refuses a level above
the rig's standing −40 dBFS limit before it starts the daemon; the daemon
itself refuses anything above full scale and never clamps. The script's
`--level` check is a convenience in front of the test's refusal, and the only
limit for the speaker route's lower −50 dBFS ceiling, which neither the test
nor the daemon can tell apart from the loopback. The 60 ms
round-trip bound was derived on a Babyface chain (#277); check a new chain's
measured offset against it before reading a red result as a defect. The
speaker route can fail the electrical-chain assertions for acoustic reasons —
read the numbers.

Manual form, when a script is not the right tool: set the three variables
together (one alone panics — half a route is a route through the wrong thing)
and run `<stage>/it_loopback_ir --ignored --nocapture --test-threads=1` from
the rig.

#### 7d. Acoustic IR, headless — EMITS through the speaker

```bash
scripts/rig/acoustic-ir.sh pupu --level -50 --consent "…" --mic-position "1 m on axis"
```

`ac plot ir` runs the measurement and prints nothing (epic #276), so the
script drives `plot_ir` through the staged `ir_probe`, routed with `ac setup`
to the rig's speaker and mic, and reports peak index, magnitude, pre-impulse
floor, SNR, offset from window centre and onset.

`--tau-ms` changes only the `peak arrival` line: it prints the peak offset
minus τ. That difference means something only when τ is itself a peak pick
for the same channel pair (a loopback τ is a different pair; see
`docs/rigs/pupu.md`). The onset line is printed with nothing subtracted, and
`ir_probe` never subtracts τ from it.

On a multi-way speaker the IR peak is not the arrival; the onset is the
better arrival estimate. Do not difference that onset by hand against
`--tau-ms`, a stored `calibrate` τ, or a loopback peak such as pupu's
17.99 ms. An onset pairs only with a τ picked by the same onset rule from the
same capture's reference leg (#460), and `ir_probe` captures no reference leg.
Onsets do not cancel across sweep bands the way peaks do. The pairing rule is
in `ac-core/src/measurement/sweep/peak.rs` (#479).

#### 7e. Transfer function, headless — EMITS when driven

No wrapper yet. `ac transfer` launches `ac-view`, so a headless session uses
the staged `transfer_probe` against a daemon started from the staged build
(prefix `PATH` with the stage dir, as the scripts do):

```bash
<stage>/transfer_probe --pairs "0,1" --seconds 20 --drive-dbfs -50 --out run.jsonl
```

`--pairs` is `meas,ref` capture indices (`0,1` = mic against the reference
loopback on pupu). It starts the session `drivable` (silent), raises drive
with `set_drive`, feeds the 1500 ms dead-man every 250 ms and drops drive on
every exit path; without `--drive-dbfs` it is passive and opens no output.
It prints the level the daemon reported. The daemon refuses only a level
above full scale, and this drives the speaker, so keeping `--drive-dbfs` at or
below the speaker ceiling is on whoever runs it. Both probes take `--ctrl-port` /
`--data-port`; `ac` takes `AC_CTRL_PORT` / `AC_DATA_PORT`, and `ac setup
output <N> input <N>` retargets a running daemon.

#### 7f. xrun soak — silent

```bash
scripts/rig/xrun-soak.sh pupu [seconds]
```

A capture-only `ac monitor --tui` on every analog input; counts jackd xrun
log lines and the daemon's own counter. An idle JACK shows no xruns — the
load is the point. Exit 1 on any xrun.

#### 7g. `ac-view` snapshot references — 192.168.9.25

TESTING.md → "A3 snapshot reference currency".

### 8. Record

Copy `scripts/rig/record-template.md` to `work/rig/<session>-results.md` and
paste each script's output block verbatim. Required fields and when to start
a new file: `.agents/rig.md` step 4 and "where records live". Mark executed
blocks in `$AC_HOME/rig-verify-queue.md` and commit there — nothing commits
`$AC_HOME` automatically.

### 9. Leave the rig

The emitting scripts restore the ac config's channels and stop the daemons
they started. Rerun `preflight.sh` at the end and paste it as "rig state
left behind", with the mic position and anything deliberately left changed.

## Carry-forward

The one statement of when a rig record made at head A still counts at a later
head B of the same PR (#579). `scripts/rig/carry-forward.sh` applies it and
`bin/rig.sh` runs it after building B and before taking the rig lock. Nobody
else re-derives it: not QA, not the rig role, not an architect comment.

1. **Source.** Only the newest *measured* record for the PR, in filing-stamp
   order under `$AC_HOME/session/`. A carried-forward record is never a
   source, so A → B → C compares C with A. A record whose file does not end
   with the runner's machine block (every record filed before #579) never
   carries, and neither does a record already at B: a new session was asked
   for at that head. Every session the runner starts files a record, even
   one that writes no record file: its posted comment in its place, or,
   with no comment either, a placeholder without a machine block. A session
   whose record went missing therefore stops the carry; it never leaves an
   older pass as the newest source.
2. **Verdict.** Only `pass` carries. `fail`, `decline` and `decline-site` run
   a session as before.
3. **Build.** Both manifests have `dirty_files=0` and the same `carry_rule`.
   At each head `CARRYSUMS` lists the same artefacts as `SHA256SUMS`, the two
   heads list the same artefacts, and every digest is equal.
4. **Digest.** For `ac`, `ac-daemon`, `ir_probe` and `transfer_probe` the
   `CARRYSUMS` line *is* the `SHA256SUMS` line, the plain sha256 `ship.sh`
   verifies on the rig. For `it_loopback_ir` it is the sha256 after zeroing
   the `.note.gnu.build-id` descriptor and replacing the stage's
   `cargo_target_dir` with a fixed token. The test binary embeds that path by
   design (`env!("CARGO_BIN_EXE_ac-daemon")`), and it is exactly the path
   `ship.sh` re-points on the rig with its symlink.
5. **Fail closed.** No record, no machine block, a failed build, a missing
   artefact, a stage that no longer matches its own sums, a parse error or
   any mismatch → no carry, with the reason printed. Nothing defaults to
   "carries".

When it carries, the runner posts a `<!-- agent: rig -->` comment at B that
names A and B in full, links A's rig comment and the filed record, gives the
five-row digest table, and ends with `**rig verdict:** pass`. It files that
comment as B's record, with a `kind=carried` block. Such a record counts
exactly as a record measured at B would, and it covers exactly what A's
measured record covers.

The machine block is a ```` ```rig-runner ```` fence of `key=value` lines at
the end of the filed record, written only by the runner
(`bin/common.sh` → `append_rig_block`). A measured block has `kind`, `head`,
`verdict`, `comment`, and the stage's `MANIFEST.txt`, `SHA256SUMS` and
`CARRYSUMS` one line each (`manifest=`, `sha256sum=`, `carrysum=`). A carried
block has `measured_at` and `measured_record` in their place.

To compare two stages by hand, for example the same two heads built twice:

```bash
scripts/rig/carry-forward.sh --stages "$AC_HOME/target-rig-stage/<revA>" "$AC_HOME/target-rig-stage/<revB>"
```

Measured on #578's pair, `210a7ff15d58` → `f64b26d113ec` (a rustdoc-only
delta). Before these build flags, 4 of 5 artefacts differed. With them, all 5
match and the pair carries. That is n = 1. A toolchain change between A and
B makes the digests differ, and the rule then costs a session. It never
carries wrongly.

## Traps that have cost sessions

- **sha256, never size or mtime** — identical size and mtime have been a
  different binary.
- **Identical hashes across refs whose code differs** means nothing was
  rebuilt (shared target dir). Refs that differ only in comments or docs now
  produce identical `ac`, `ac-daemon`, `ir_probe` and `transfer_probe`; that
  is [carry-forward](#carry-forward) working, and `compiled_this_run=yes` in
  the manifest tells the two cases apart.
- **`generate sine` / `generate pink` run until `ac stop`.** Never background
  one; use the bounded commands (`generate level`, `generate frequency`,
  `plot level`, `plot ir`).
- **`ac monitor` needs `--tui` headless**; without it, it wants `ac-view`.
- **An auto-spawned daemon's stderr goes to `/dev/null`**, so an analysis
  error can show as an empty result table.
- **JACK aliases are not evidence of physical ports** — probe by emission.
- **The FF400's JACK port order moves** (ADAT block first or last). A profile
  that was right yesterday can point every port name at ADAT today; the
  scripts check, a manual command does not.
- **A window edge imitates a latency**: an arrival outside the analysis
  window returns a stable, repeatable, wrong number.
