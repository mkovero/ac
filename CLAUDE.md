# measuring — project context for Claude Code

Agent specs are in `.agents/`. Before doing anything, read the relevant spec
for your current role. The active role for this session will be stated in the
first user message.

Repo structure: ac-rs/ (cargo workspace: ac-core, ac-daemon, ac-ui, ac-cli) — stddocs/ docs/ tests/
Build (run in ac-rs/): cargo test | cargo clippy -- -D warnings | cargo fmt --check
See ac-rs/CLAUDE.md and ARCHITECTURE.md for the crate/module map.
