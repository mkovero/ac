<!-- developer session pr-296-rev — 2026-08-12T16:53+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-12-developer-pr-296-rev.jsonl -->
<!-- resume: claude --resume 7e38f634-31a2-4612-86c6-faa5a5cbb1e5 -->

Done. Caveman summary:

Fixed PR #296, branch `issue-282`, pushed commit `00861a9`. No new PR, no label touch.

**Fix:** `check_tail_decay` window was flat `tail_len/4` — shorter than lowest band's settle time at shipped `tail_s=0.5`, so that band always `NEG_INFINITY`'d and got silently dropped. Now window sizes off widest settle requirement (`Filterbank::settle_samples()`, new accessor), capped at half-tail. Bands still too short to settle get counted (`bands_settled`/`bands_total`) and surfaced in `note()` instead of vanishing.

Also killed dead disjunct (issue 2), added regression test at shipped default + coverage-assertion test that actually discriminates old vs fixed code.

`cargo test --workspace` all green, clippy clean, fmt clean.

Posted reply comment on PR addressing each QA point — including one open disagreement I flagged rather than hid: didn't tie `passed` to full band coverage (kept it scoped to bands actually evaluated + separate visibility for gaps), since that felt like a behavior-semantics call beyond bugfix scope. Said so explicitly, asked reviewer's call.

Also noted honestly: QA's suggested regression test (ported near-verbatim) doesn't actually discriminate the bug by my analysis — kept it for real gap coverage anyway, but the coverage-assertion test is the one that actually proves the fix.
