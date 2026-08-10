claude --system-prompt-file .agents/architect.md \
  "Audit the full codebase. Read all source files under ac-rs/"

claude --system-prompt-file .agents/ux.md \
  "Audit the full codebase. Read all stdout-producing code across ac-rs/."

claude --system-prompt-file .agents/qa.md \
  "Audit the full codebase. Read all tests and measurement-producing code. Read stddocs/ for applicable standards."

claude --system-prompt-file .agents/audit.md \
  "Read audit/architect-raw.md, audit/ux-raw.md, audit/qa-raw.md. Write consolidated report to audit/audit-$(date +%F).md"
