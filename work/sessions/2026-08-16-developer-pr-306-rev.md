<!-- developer session pr-306-rev — 2026-08-16T07:56+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-16-developer-pr-306-rev.jsonl -->
<!-- resume: claude --resume f5c477ac-02b6-43a6-8c84-6d326b3cba81 -->

Done. Pushed `db661b0` to `issue-285`, comment posted: https://github.com/mkovero/ac/pull/306#issuecomment-5306417787

Summary:
- correctness issue 1 (mic_correction_applied ambiguity) → fixed, report note added
- test gap (non-finite bins) → fixed, test added verbatim per suggestion
- AES17 clause verify → declined, out of scope (file untouched by this PR, review itself said non-blocking), said so in comment, offered as follow-up
- gate: full workspace test/clippy/fmt clean, no label touched, no new PR.
