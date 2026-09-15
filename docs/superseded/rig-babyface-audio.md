# Superseded: Babyface Pro audio rig setup (192.168.9.25)

**Superseded 2026-09-14.** Audio hardware-in-the-loop testing moved to the
dedicated rig `pupu` (`docs/rigs/pupu.md`, procedure in
`docs/runbooks/rig-testing.md`). 192.168.9.25 stays in use only as the
real-GPU box for the `ac-view` snapshot tests (TESTING.md → "A3 snapshot
reference currency"). Kept because the historical records in `work/rig/` and
`audit/rig-*` were measured on this setup and cite it.

Removed from `.agents/rig.md`, where it was the rig role's default:

- RME Babyface Pro. Speaker on ADAT1/AS1 out (`playback_5`), mic on AN1
  (`capture_1`), electrical loopback reference out AN2 (`playback_2`) into
  IN4 (`capture_4`).
- Normally not connected but reserved: loopback through the master converter
  out ADAT3 (`playback_7`) into IN3 (`capture_3`); master analogue section
  loopback with converter out AS1 (`playback_5`) into AN2 (`capture_2`).
- Interface clock had to stay `AutoSync` (`numid=320 = 0`): an external
  master clocked the card over ADAT, which carried the stimulus leg; setting
  `Internal` silently broke the speaker path rather than erroring.
- Pre-flight mixer state it set without confirming:

  ```
  amixer -c0 cset numid=1 0       # dont monitor mic -> AN1
  amixer -c0 cset numid=14 0      # dont monitor mic -> AN2
  amixer -c0 cset numid=301 36    # mic input gain (36=max)
  amixer -c0 cset numid=295 46341 # playback_7 output level
  amixer -c0 cset numid=293 16384 # playback_5 output level
  amixer -c0 cset numid=294 16384 # playback_6 output level
  amixer -c0 cset numid=308 0     # IN4/capture_4 level (no gain)
  amixer -c0 cset numid=307 0     # IN3/capture_3 level (no gain)
  amixer -c0 cset numid=302 1     # AN1/capture_1 mic input 48V on
  amixer -c0 cset numid=305 0     # AN2/capture_2 mic input 48V off
  amixer -c0 cset numid=289 16384 # AN1/playback_1 output level
  amixer -c0 cset numid=290 16384 # AN2/playback_2 output level
  ```

- 192.168.9.25 is the development VM's hypervisor host: never build there.
