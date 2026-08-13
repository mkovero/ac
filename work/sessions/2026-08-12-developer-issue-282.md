<!-- developer session issue-282 — 2026-08-12T05:44+00:00 — exit 1 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-12-developer-issue-282.jsonl -->

Now add the sweep.rs unit tests footer check — there were no tests in this file originally (it had none per earlier listing at 328 lines, all handler code, no `#[cfg(test)]`). Good. Now update `handlers/audio/mod.rs`, `handlers/mod.rs`, `workers.rs`, `server.rs`.
