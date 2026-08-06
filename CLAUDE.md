# measuring — project context for Claude Code

Agent specs are in `.agents/`. Before doing anything, read the relevant spec
for your current role. The active role for this session will be stated in the
first user message.

Repo structure: ac-rs/ (cargo workspace, five crates) — stddocs/ docs/ tests/

Documents: root holds only the entry points (this file, README, ARCHITECTURE,
HARDWARE, TESTING). `docs/` is durable reference — `docs/design/` for design
notes and briefs, `docs/superseded/` for kept-but-dead plans. `work/` is
in-flight and expiring — `work/handoff/`, `work/rig/`, `work/qa/`,
`work/planning/`. A handoff that has been executed belongs under `work/` with
its expiry condition written in, not at root where it reads as current.

| crate | binary | role |
|-------|--------|------|
| `ac-core` | — | Measurement library. Tier 1 (`measurement/`) and Tier 2 (`visualize/`), plus `shared/` calibration, config, generator. No sockets. |
| `ac-daemon` | `ac-daemon` | ZMQ REP+PUB server. Audio I/O (JACK/CPAL/fake), worker management. |
| `ac-cli` | `ac` | CLI client. Positional parser, ZMQ REQ/SUB, CSV export, daemon auto-spawn. |
| `ac-scene` | — | Pure scene/data layer for the views: traces, axes, readouts as plain data. No rendering, no egui, no ZMQ. |
| `ac-view` | `ac-view` | Keyboard-driven egui shell. Draws `ac-scene` scenes; no numeric computation of its own. |

Build (run in ac-rs/): cargo test | cargo clippy -- -D warnings | cargo fmt --check
See ac-rs/CLAUDE.md and ARCHITECTURE.md for the crate/module map. The
Tier 1 / Tier 2 split in ARCHITECTURE.md decides where a new analysis
feature belongs; `ac-scene` vs `ac-view` is the display-truth boundary.
