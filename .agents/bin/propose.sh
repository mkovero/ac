claude -p --system-prompt-file .agents/triage.md \
  "Read audit/audit-$(date +%F).md. List the issues you would create — title, category, labels, rationale. Do not post anything." \
  | tee audit/proposed-issues.md
