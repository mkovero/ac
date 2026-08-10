claude --system-prompt-file .agents/triage.md \
  "Read audit/proposed-issues.md and audit/audit-$(date +%F).md. Create exactly the issues listed in proposed-issues.md in mkovero/ac."
