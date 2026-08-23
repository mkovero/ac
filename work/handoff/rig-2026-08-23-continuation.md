# Continuation point — 2026-08-23 rig session (jackd direct)

**Written:** 2026-08-23, mid-session, at a resource limit.
**Expires:** when the four issues below are filed and the rig doc is committed.
Delete it then — everything durable lives in
`work/rig/rig-2026-08-23-jackd-direct-results.md`, which is the record; this
file is only the "where I was standing" note.

## Settled this session — do not re-litigate

- **The #363 τ jump was PipeWire.** Operator removed pipewire-jack; `jackd`
  drives ALSA directly. 65 `ac calibrate` runs / 130 client lifetimes returned
  one value; `jack_iodelay` agreed to 0.18 samples. Commented on #363
  (`issuecomment-5382966295`). `work/handoff/unstable-periods-handover.md` is
  marked expired, `rme-re` exonerated.
- **τ on this rig is 4.4167 ms, not 43.75 ms.** ~90 % of the old figure was
  PipeWire buffering. Everything derived from 43.75 ms must be re-derived.
- **3 m acoustic check passes**: 3.17 m after the peak-late bias, from a fully
  measured decomposition.

## Rig state as left

`jackd --realtime --realtime-priority 95 -d alsa -d hw:Pro71990237 -r 96000
-p 64 -n 2 -i 10 -o 10 -I 116 -O 116`. Ports are `system:capture_N` /
`system:playback_N`; no `monitor_N`; nothing returns digitally to the Babyface.

| what | value | note |
|---|---|---|
| numid=304 `2-AN2 Capture Volume` | **16** | **changed by this session, was 12** — the +4 dB that clears `calibrate`'s unity gate on the master-section return |
| numid=295 `07-ADAT3 Playback Volume` | 46341 | operator set; 65535/√2, so the direct converter loop passes the same gate |
| numid=301 `1-AN1 Capture Volume` | 36 | operator set mid-session |
| numid=320 Sample Clock Source | 0 (AutoSync) | must stay — ADAT master carries the stimulus leg |
| patch | converter master output → `capture_2`/AN2 | operator's patch, still in place; normally feeds the speakers |
| daemon | `/usr/local/bin/ac-daemon --local`, `HOME=/home/mui` | restored to as-found |

Session dirs on the rig: `~/rig-2026-08-23/home-{disc,acoustic,conv,dac,scan}`,
each an isolated `HOME` with its own `config.json` naming sticky ports. Test
binaries are the 08-22 `bin-350` build (`19604fc`), sha256 `605bc9f9…`
(daemon) / `7fa422d4…` (`ac`) — the `tau-window-override` feature build, which
also prints per-reading `peak_abs` / `snr_db`.

## Next measurement, if the rig is free — one cable move

Move the existing AN2-out loopback from **IN4 to IN3** and run `ac calibrate
-30dbfs` five times against `playback_2 → capture_3`. That yields
`Ba(IN3) − Ba(IN4)` by subtraction against the 424 already measured, which
separates the two unknowns currently tangled in the 46-sample difference: the
analogue master section versus the AN-vs-line input stage. Half a millisecond
is a lot for passive analogue gear, so this is worth settling rather than
assuming. Method: isolated `HOME`, sticky `output_port`/`input_port`, never the
positional `output N input N` form (#358 misroutes it).

## Four issues to file, none filed yet

1. **Derived τ instead of refusal.** `resolve_tau` (`handlers/audio/plot.rs:518`)
   never falls back, by #281/#283 design, so `plot ir` on the acoustic path
   prints `distance unavailable` forever — `cal.json` is keyed per channel pair
   and `tau_for` additionally demands exact port-name match. But `tau_for`
   already computes `nearest` and `differing_fields` and throws them away, the
   inter-pair spread on this rig is 60 samples / 0.22 m, and the error has a
   known sign. Proposal: a third `InterfaceLatency` state that reports the
   distance using the nearest τ, names the differing conditions, and bounds the
   error directionally — instead of refusing and reporting nothing.
2. **The ±2 dB unity gate, and its message.** `handlers/calibrate.rs:428`
   requires capture within ±2 dB of drive − 3.01 dB. It refused three correct
   loopbacks this session (muted route, +3.01 dB hot, −4.19 dB low), each time
   printing `loopback not detected this run` — which names a cause the
   instrument cannot know. τ is a timing measurement; peak position does not
   depend on level. The window is far too tight for "is a cable patched", and
   on the master-section pair the level depends on an analogue fader position.
3. **`calibrate` ignores the xrun counter.** `plot`, `test` and `monitor_tui`
   all report one; the JACK backend maintains it (`jack_backend.rs:229`,
   exposed `:549`); τ does not. At period 64 an xrun inside the 0.35 s sweep
   corrupts the IR and τ returns a bare number. Latent, not observed —
   `ac-daemon` ran 130 lifetimes clean.
4. **The auto-spawned daemon caches config at startup.** A config edit between
   runs does not reach a daemon that is already up, so a scan loop silently
   re-measures the first channel. It produced ten identical readings that
   looked like a clean negative result. Same family as the port-5556 trap: a
   long-lived daemon under an isolated `HOME` answering on the standard port.

## Uncommitted, in the worktree

- `work/rig/rig-2026-08-23-jackd-direct-results.md` — new, the full record.
- `work/handoff/unstable-periods-handover.md` — expiry banner added.
- `ac-rs/crates/ac-daemon/tests/it_loopback_ir.rs` — module-doc port names now
  show both `system:*` and pipewire forms. Comment-only; `cargo fmt --check`
  clean. Not built or tested beyond that.
- this file.

Nothing is committed and no branch was created. Repo is a shared checkout with
concurrent sessions, so committing to a branch is the safer resting state.

## Two mistakes recorded so they are not repeated

- **Do not infer topology from a small delta.** 438 − 424 = 14 samples was read
  as "too short to contain a converter DAC+ADC", but it is a difference between
  two paths that each contain a converter — they cancel. That produced a false
  "digital ADAT return", and correcting it produced a second version of the
  same claim. Only the operator knows the wiring; ask.
- **Attribute a control change to a measurement only if the timing is known.**
  A mic-preamp change mid-block was read as "24 dB of gain did nothing", which
  led to a wrong guess about where the microphone was plugged in. The blocks
  were probably both at the same gain.
