<!-- developer session issue-277 — 2026-08-15T15:25+00:00 — exit 0 -->
<!-- record of one run, not status. raw: ~/.local/state/ac/2026-08-15-developer-issue-277.jsonl -->
<!-- resume: claude --resume afb31346-7d4d-466c-9893-3e7eb6b3972e -->

Blocked — need explicit consent before touching hardware.

**Two things collide:**

1. **Which rig, which path.** Issue wants Babyface Pro at 96 kHz, not dummy driver. That's `192.168.9.25` (radv dev box) — its PipeWire is locked to `clock.rate 96000` anyway, so sample rate is free. `.40` rig has FF400, not Babyface — wrong box for this issue. Real converter path there needs either digital loopback (`monitor_N → in`) or analog patch (out→in), both count as "signal leaves the interface" under the consent rule.

2. **Level conflict.** Runbook's `it_loopback_ir.rs` hardcodes `level_dbfs: -6.0` (line 220). Hard consent rule caps real-hardware stimulus at **-40 dBFS** (electrical) unless per-session exception granted. -6 dBFS blows through that ~34 dB. Can't run the test binary verbatim on real hardware as-is — need either your explicit one-off exception for -6 dBFS, or I drive the same sweep manually via ZMQ (`plot_ir` REQ) with `level_dbfs` clamped to -40, bypassing the test's hardcoded value (test itself unedited, scope stays clean).

**Asking before any signal goes out:**
- Consent to emit on 192.168.9.25 this run — which leg: digital loopback (bit-reproducible, no DAC/ADC) or analog out→in (has real converter in the path, closer to what issue's "not the dummy driver" concern is about)?
- Level: run manual `plot_ir` at -40 dBFS instead of test's -6, or grant exception for -6 this run?

No hardware touched yet. Say go + which options and I run it, record peak idx/mag/SNR + τ candidate to `work/rig/`, open PR with just that file (issue is data-only, no code fix in scope).
