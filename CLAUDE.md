# measuring — project context for Claude Code

Agent specs in `.agents/`. Read spec for your role before anything. Active role stated in first user message.

Repo: ac-rs/ (cargo workspace, five crates) — stddocs/ docs/ tests/

Documents: root holds entry points only (this file, README, ARCHITECTURE, TESTING). `docs/` = durable reference — `docs/design/` design notes + briefs, `docs/superseded/` dead-but-kept plans. `work/` = in-flight, expiring — `work/rig/`, `work/planning/`. Out of tree in `$AC_HOME`: handoffs (`handoff/`), session records (`session/`), and the live rig queue (`rig-verify-queue.md`) — one copy each, never per-branch. Read a handoff only when a task names it. Nothing under `$AC_HOME` is versioned, so treat a deletion there as permanent.

| crate | binary | role |
|-------|--------|------|
| `ac-core` | — | Measurement library. Tier 1 (`measurement/`) + Tier 2 (`visualize/`), plus `shared/` calibration, config, generator. No sockets. |
| `ac-daemon` | `ac-daemon` | ZMQ REP+PUB server. Audio I/O (JACK/CPAL/fake), worker management. |
| `ac-cli` | `ac` | CLI client. Positional parser, ZMQ REQ/SUB, CSV export, daemon auto-spawn. |
| `ac-scene` | — | Pure scene/data layer for views: traces, axes, readouts as plain data. No rendering, no egui, no ZMQ. |
| `ac-view` | `ac-view` | Keyboard-driven egui shell. Draws `ac-scene` scenes; no numeric computation of own. |

Build (run in ac-rs/): cargo test | cargo clippy -- -D warnings | cargo fmt --check
Crate/module map: ac-rs/CLAUDE.md + ARCHITECTURE.md. Tier 1 / Tier 2 split in ARCHITECTURE.md decides where new analysis feature belongs; `ac-scene` vs `ac-view` = display-truth boundary.