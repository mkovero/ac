## architect audit — 2026-05-27

> **Scope note:** spec module map (`ac/`, `thd_tool/`, `ds/` with `estimator.rs`,
> `session.rs`, `level.rs`, `signal.rs`) is **stale**. Reality audited instead:
> a 4-crate Rust workspace under `ac-rs/crates/` — `ac-core` (shared lib),
> `ac-daemon` (ZMQ server + audio), `ac-ui` (egui client), `ac-cli` (CLI client) —
> plus a standalone **Python** package `ds/`. No `thd_tool` crate exists; THD is a
> library module `ac-core/src/measurement/thd.rs`. ~45k LOC total.

### module boundaries

Clean dependency graph. Single shared lib:

```
ac-core  ← ac-daemon
ac-core  ← ac-ui
ac-core  ← ac-cli
```

- `ac-core` depends on no sibling crate. `ac-daemon`/`ac-ui`/`ac-cli` depend only on
  `ac-core`. No circular deps. No client depends on another client.
- `ds/` is Python, fully out of the cargo workspace. **Not** ZMQ-coupled —
  `ds/session.py` reads/writes a local `session.json`; `ds/files.py:9` +
  `ds/context.py:91` scan the shared session dir for `.csv`/`.png` ac outputs.
  Coupling to `ac` is **on-disk file layout**, not the wire protocol.

One smell: wire-frame construction lives in `ac-daemon` handlers as inline
`json!()` macros, while the typed mirror of those frames lives in
`ac-ui/src/data/types.rs`. The schema's two halves sit in two crates with no
shared type. Belongs in `ac-core` (see structural risks).

### invariant audit

| invariant | enforced | notes |
|---|---|---|
| ZMQ schema single-sourced | ✗ | Triplicated: daemon emits inline `json!()`; ac-ui has typed serde structs; ac-cli parses untyped `serde_json::Value`. Agreement is manual. `ZMQ.md` (51 KB) is the only authority. |
| no freq-dependent level ref | ✓ | `shared/reference_levels.rs` + `conversions.rs` are all scalar (`vrms_to_dbu`, `dbu_to_vrms`, single `vrms_at_0dbfs` point). No freq tables. Mic curve (`MicResponse`, freq/gain vecs) is a **separate** structure applied post-measurement on the measurement leg only — not baked into level ref. |
| H1 matches Müller-Massarani | ✗ | **Invariant is misstated.** Two distinct estimators exist, neither is MM: (1) live `transfer.rs` is Welch averaged-periodogram **H1 = Gxy/Gxx**, Hann window, 50% overlap, coherence `|Gxy|²/(Gxx·Gyy)`. (2) `measurement/sweep.rs` is **Farina** exp-sweep deconvolution (Tier 1 IR). The cited PDF `Simultaneous_Measurement_of_Impulse_Response_and_D.pdf` is **Farina 2000**, not Müller-Massarani 2001 — and `sweep.rs:1-23` cites it correctly. No "Müller"/"Massarani" string anywhere in code. |
| thd_tool standalone | ✓ (N/A) | No `thd_tool` crate. THD is `ac-core/measurement/thd.rs`, a pure lib module consumed by daemon handlers (plot/monitor/test_dut/test_software). No runtime coupling issue. `ds` python has zero THD coupling. |

### interface surface

**ZMQ protocol** — REP ctrl `tcp://127.0.0.1:5556`, PUB data `tcp://127.0.0.1:5557`
(`ac-daemon/src/server.rs:120-131`, `PUB_HWM=50_000`). Data frames are
`<topic> <json>` text. Ctrl replies always carry `{"ok": bool, ...}`.

- Tier 1 frames: `measurement/frequency_response/{point,complete}`,
  `measurement/spectrum_bands`, `measurement/impulse_response`,
  `measurement/loudness`, `measurement/report`.
- Tier 2 frames: `visualize/{spectrum,cwt,cqt,reassigned,fractional_octave,
  fractional_octave_leq,scope,ir}`, plus `transfer_stream`, `keepalive`.
- **Versioning:** only Tier 1 `measurement/report` carries `schema_version`
  (`measurement/report.rs:25`, `SCHEMA_VERSION=3`). **All Tier 2 / streaming
  frames are unversioned** — identified by `type` tag alone.
- Documented: `ac-rs/ZMQ.md`. Robustness aid: ac-ui types use `#[serde(default)]`
  ~31× so added fields don't break old UI; renamed/removed fields still break.

**CLI** — `ac-cli` clap subcommands (devices, dmm, generate, gpio, probe, report,
server, session, setup, stop, sweep, test, calibrate, monitor, plot). Consistent
style. `ds` exposes its own Python CLI (`ac new`, `ls`, notes, files) — separate
surface, separate style, intentional.

**Undocumented assumptions a new dev needs:**
- ac-cli reads frames untyped; ac-ui reads them typed — changing a JSON key
  silently breaks ui deserialize while cli may keep limping.
- `ds` depends on ac writing `.csv`/`.png` into the session dir; changing ac's
  output filenames/dir layout breaks `ds` context summaries with no compile error.

### structural risks

ranked by severity:

1. **ZMQ schema is triple-defined, no shared type, Tier 2 unversioned.** Most
   brittle spot. A field rename in a daemon `json!()` breaks ac-ui at runtime
   only (no compile-time link), and `ZMQ.md` can drift from code freely. Any
   issue touching frame fields is high-risk. *Mitigation:* hoist frame structs
   into `ac-core`, have daemon serialize them and ui/cli deserialize them; add a
   protocol version to the data envelope.
2. **Stale architecture map vs. reality.** Spec/onboarding docs describe modules
   that don't exist (`estimator.rs`, `level.rs`, `signal.rs`, `thd_tool`). A dev
   following them wastes time / edits wrong place. *Mitigation:* refresh the map.
3. **Misnamed H1 invariant.** Calling the estimator "Müller-Massarani" when it is
   Welch-H1 (live) + Farina (sweep) invites a dev to "fix" correct code toward the
   wrong derivation. *Mitigation:* correct the invariant wording; cite Welch and
   Farina explicitly.
4. **ds↔ac file-layout coupling is implicit.** No schema, no test. *Mitigation:*
   document the session-dir contract or expose an `ac` command ds can call.

No significant dead code found in the level-ref / conversion paths (no
`removed`/`deprecated`/`unreachable` markers; the removed freq-correction curve is
genuinely gone, not commented out — invariant holds).

### what is solid

- Crate dependency graph: clean, acyclic, single shared lib. No refactor needed.
- Level reference: correctly scalar; mic curve cleanly separated and applied
  post-measurement. Matches invariant exactly.
- Farina sweep path (`measurement/sweep.rs`): correctly derived and correctly
  cited against the PDF. The math is sound; only the *name* in the invariant is wrong.
- `#[serde(default)]` discipline in ac-ui gives partial forward-compat against
  additive schema changes.
