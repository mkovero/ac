# measuring — project context for Claude Code

Agent specs are in `.agents/`. Before doing anything, read the relevant spec
for your current role. The active role for this session will be stated in the
first user message.

Repo structure: ac/ thd_tool/ ds/ stddocs/
Build: cargo test | cargo clippy -- -D warnings | cargo fmt --check
