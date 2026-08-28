### Loopback IR runbook

`plot_ir`'s real-audio path (`JackEngine::play_and_capture`) is exercised
by an `#[ignore]`'d integration test that needs a live JACK server. It is
not run in `cargo test`; invoke it manually after starting JACK:

```bash
# 1. Start JACK. The dummy driver works — no hardware needed.
jackd -d dummy -r 48000 -p 1024 &

# 2. Run the loopback test.
cargo test -p ac-daemon --test it_loopback_ir -- --ignored
```

The test pre-writes a config with `output_port = "ac-daemon:in"` and
`input_port = "ac-daemon:out"`, so the daemon self-connects its own
JACK output to its own input — no `jack_connect` and no system audio
devices required. It then runs a 2.0 s exponential sweep, deconvolves,
and asserts the recovered linear IR has a dominant peak at least 25 dB
above the pre-impulse floor, at or after the gate centre and within a
60 ms round-trip margin of it — both derived from measurement, not round
numbers; see `it_loopback_ir.rs`'s `MAX_ROUND_TRIP_S`/`SNR_FLOOR_DB`
comments and #341. The 2.0 s duration is itself load-bearing, not just a
default value: shorter sweeps shrink the window the round-trip bound is
measured against, and below ~1.0 s that window can no longer hold
`MAX_ROUND_TRIP_S` at all (#361).

**That default routes through no converter.** The self-loop stays inside
the daemon's own JACK client, so it exercises the ring and the
deconvolution, not an interface. To put the sweep through real hardware,
name real ports:

```bash
AC_LOOPBACK_OUT="Babyface Pro Pro:playback_2" \
AC_LOOPBACK_IN="Babyface Pro Pro:capture_4" \
AC_LOOPBACK_LEVEL_DBFS=-40 \
  cargo test -p ac-daemon --test it_loopback_ir -- --ignored --nocapture
```

Both port variables must be set together — half a route is a route
through the wrong thing. `AC_LOOPBACK_LEVEL_DBFS` is **mandatory**
whenever they are set: naming real ports means driving real outputs, and
`plot_ir` does not apply the config's `drive_max_dbfs` ceiling (only
`set_drive` does), so that value is the only limit on what reaches the
converter. Unset, all three default to the self-loop at −6 dBFS and the
dummy invocation above is unchanged.

`--nocapture` prints the record block: chain, sample rate, window length,
peak index, peak magnitude, floor, SNR, and the peak's offset from the
window centre — the round-trip latency of that chain. The block is
printed before the SNR assertion, so a failing run still leaves its
numbers behind.

A CPAL equivalent (e.g. via `snd-aloop` or a PipeWire virtual sink) is
deferred until the CPAL routing path is fixed (issue #27).