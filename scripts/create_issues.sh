#!/usr/bin/env bash
# Creates GitHub issues for mkovero/ac project.
# Requires: gh CLI authenticated (gh auth login)
# Usage: bash create_issues.sh

set -e
REPO="mkovero/ac"

echo "Creating labels..."
gh label create "software"  --color "0075ca" --description "ac Python/Rust codebase"       --repo $REPO 2>/dev/null || true
gh label create "hardware"  --color "e4a853" --description "Analog frontend / io board"     --repo $REPO 2>/dev/null || true
gh label create "testing"   --color "0e8a16" --description "Test suite"                     --repo $REPO 2>/dev/null || true
gh label create "blocker"   --color "d93f0b" --description "Blocking other work"            --repo $REPO 2>/dev/null || true
gh label create "backlog"   --color "ededed" --description "Not yet started"                --repo $REPO 2>/dev/null || true
gh label create "phase-4"   --color "bfd4f2" --description "Rust daemon phase 4 work"       --repo $REPO 2>/dev/null || true

echo "Creating milestones..."
gh api repos/$REPO/milestones -f title="Hardware: office visit + schematic prep" -f state="open" 2>/dev/null || true
gh api repos/$REPO/milestones -f title="Hardware: KiCad + fabrication"           -f state="open" 2>/dev/null || true
gh api repos/$REPO/milestones -f title="Software: Phase 4 daemon parity"         -f state="open" 2>/dev/null || true

echo ""
echo "Creating hardware issues..."

gh issue create --repo $REPO \
  --title "Office visit: confirm transformer specs and relay inventory" \
  --label "hardware,blocker,backlog" \
  --body "Blocking item — must be resolved before schematic work can start.

- [ ] Lundahl transformer part numbers (2× input CH1/CH2, 2× output CH1/CH2)
- [ ] Studer transformer specs + quantity on hand
- [ ] Relay coil voltage — check relay bin
- [ ] Relay contact rating — check relay bin
- [ ] Bosch panel XLR/BNC connector types — measure/photograph
- [ ] Dual-gang rotary sourcing (once pad impedance confirmed)

Ref: \`HARDWARE.md\` Pending Items"

gh issue create --repo $REPO \
  --title "Calculate H-pad resistor values" \
  --label "hardware,backlog" \
  --body "Cumulative balanced H-pad, 3×20dB stages → 0/20/40/60dB total.

- Impedance: 600Ω for transformer channels (CH1/CH2); TBC for CH3/CH4 direct
- Stage 1 resistors: 1W minimum rating (sees full input up to ~77Vrms / 109V peak)
- Stages 2/3: 0.25W sufficient
- Maximum input: ~77Vrms (3kW/2Ω power amp)

Depends on transformer impedance confirmation from office visit.
Ref: \`HARDWARE.md\` Pad Network"

gh issue create --repo $REPO \
  --title "Design input protection network" \
  --label "hardware,backlog" \
  --body "XLR inputs CH1–CH4:
- HV series resistor (value TBD from pad calc)
- Back-to-back zener clamp

BNC inputs:
- Series R + TVS clamp to ±24V

Ref: \`HARDWARE.md\` Protection table"

gh issue create --repo $REPO \
  --title "Design INA134 differential receiver stage" \
  --label "hardware,backlog" \
  --body "Fixed 1× gain, 0.0005% THD. Application circuit for all 4 XLR input channels.
- CH1/CH2 receive transformer secondary
- CH3/CH4 receive pad output directly

Ref: INA134 datasheet, \`HARDWARE.md\`"

gh issue create --repo $REPO \
  --title "Design OPA134 HiZ buffer stage (BNC probe path)" \
  --label "hardware,backlog" \
  --body "FET input, 1MΩ+ input impedance.
- BNC pair 1: probe mode (via DPDT toggle)
- BNC pair 2: permanently probe mode
- Output feeds FF400 via Mega-controlled path selection relay

Ref: \`HARDWARE.md\` BNC sections"

gh issue create --repo $REPO \
  --title "Design THAT1646 output stage" \
  --label "hardware,backlog" \
  --body "Balanced line driver, 0.0003% THD, built-in short circuit protection.
- 100Ω series protection on output
- Optional transformer isolation (Lundahl/Studer 1:1) on CH1/CH2 outputs

Ref: THAT1646 datasheet, \`HARDWARE.md\`"

gh issue create --repo $REPO \
  --title "Design relay circuits (impedance, path selection, stereolink)" \
  --label "hardware,backlog" \
  --body "All relays driven by ULN2803 Darlington array + flyback diodes on all coils.

- Impedance relays: 10kΩ/100kΩ balanced per channel (×4 ch)
- Path selection relays: XLR (INA134) vs BNC (OPA134) → FF400 per channel pair
- Stereolink relays: gang CH1+CH2, CH3+CH4
- Output transformer bypass relay CH1/CH2

Coil voltage TBC from office visit. Ref: \`HARDWARE.md\`"

gh issue create --repo $REPO \
  --title "Design PSU section" \
  --label "hardware,backlog" \
  --body "Rails required:
- ±15V analog (INA134, THAT1646, OPA134)
- 5V logic (Mega, relay drivers)
- Relay coil voltage (TBC from office visit)

Ref: \`HARDWARE.md\` Power Supply"

gh issue create --repo $REPO \
  --title "KiCad schematic — analog frontend board (board 1/3)" \
  --label "hardware,backlog" \
  --body "Depends on all analog stage designs being complete.

Covers:
- Input protection + zener/TVS clamps
- Impedance switching relays
- H-pad networks
- Transformer interfaces CH1/CH2
- INA134 stages ×4
- OPA134 HiZ buffers
- THAT1646 output stage
- Path selection relays

Manufacture in China. Ref: \`HARDWARE.md\` PCB Plan"

gh issue create --repo $REPO \
  --title "KiCad schematic — digital backend board (board 2/3)" \
  --label "hardware,backlog" \
  --body "Arduino Mega2560 interface headers, ULN2803 relay drivers, I2C header (optional VFD), power distribution.

Mixed signal layout — careful ground plane separation from analog board.
Manufacture in China. Ref: \`HARDWARE.md\` PCB Plan"

gh issue create --repo $REPO \
  --title "KiCad schematic — PSU board (board 3/3)" \
  --label "hardware,backlog" \
  --body "±15V analog rails, 5V logic, relay coil voltage rail.
Manufacture in China. Ref: \`HARDWARE.md\` PCB Plan"

gh issue create --repo $REPO \
  --title "Arduino Mega firmware" \
  --label "hardware,backlog" \
  --body "Serial protocol reporting to \`ac\`:
- Stereolink state per pair
- Active input path (XLR/BNC) per channel
- Impedance setting per channel
- Pad position per channel (rotary encoder or ADC)

\`ac\` uses this for calibration factors, stereo/mono mode, active path.
Optional: I2C to VFD display (TBC).

Ref: \`HARDWARE.md\` Serial Protocol"

gh issue create --repo $REPO \
  --title "Make measurement cables" \
  --label "hardware,backlog" \
  --body "- 2× XLR male → grabber clips, 30cm — stereo power amp measurement
- 1× XLR male → grabber clips, 50cm — general purpose

Wiring: pin 2 hot, pins 1+3 GND (single-ended power amp output into balanced XLR input).

**Label all: \"HV MEAS — CHECK PAD BEFORE CONNECTING\"**"

echo ""
echo "Creating software issues..."

gh issue create --repo $REPO \
  --title "Implement interactive calibrate / cal_reply loop in Rust daemon" \
  --label "software,phase-4,backlog" \
  --body "Currently \`calibrate\` emits a \`cal_prompt\` frame but does not wait for a real \`cal_reply\` with a Vrms reading before completing. Wire up the full prompt loop so \`ac calibrate\` works end-to-end with and without a DMM.

Ref: \`ac-rs/crates/ac-daemon/src/handlers.rs\`, \`ac-rs/PLAN.md\` Phase 4"

gh issue create --repo $REPO \
  --title "Implement dmm_read in Rust daemon (SCPI over TCP)" \
  --label "software,phase-4,backlog" \
  --body "Currently always returns 'no DMM configured'. Implement SCPI TCP client:
- Connect to configured IP:5025
- Send \`MEAS:VOLT:AC?\`
- Average 3 readings

Ref: \`ac/dmm.py\` has the Python reference implementation."

gh issue create --repo $REPO \
  --title "Implement transfer command in Rust daemon" \
  --label "software,phase-4,backlog" \
  --body "\`ac-core/src/transfer.rs\` already has H1 transfer function estimation — wire it up to the daemon ZMQ command dispatch.

Ref: \`ac-rs/PLAN.md\` Phase 4, \`ac/transfer.py\`"

gh issue create --repo $REPO \
  --title "Implement probe command in Rust daemon" \
  --label "software,phase-4,backlog" \
  --body "Auto-detect analog ports and loopback pairs (DMM + capture scan). Requires dmm_read to be working first.

Ref: \`ac-rs/PLAN.md\` Phase 4"

gh issue create --repo $REPO \
  --title "Implement test_hardware / test_dut commands in Rust daemon" \
  --label "software,phase-4,backlog" \
  --body "Port hardware validation and DUT characterization commands.

See \`TESTING.md\` for full pass criteria: noise floor, level linearity, THD floor, frequency response, channel match, repeatability, DMM cross-check, DUT gain/THD/freq response/clipping."

gh issue create --repo $REPO \
  --title "CPAL/sounddevice fallback audio backend" \
  --label "software,phase-4,backlog" \
  --body "Implement CPAL backend in \`ac-rs/crates/ac-daemon/src/audio/\` as fallback when JACK is not running. Must implement the same \`AudioEngine\` trait as \`jack_backend.rs\`.

Required for macOS and Windows support. Ref: \`ac-rs/PLAN.md\` Phase 4"

gh issue create --repo $REPO \
  --title "Port GPIO handler to Rust daemon" \
  --label "software,phase-4,backlog" \
  --body "Port \`ac/gpio/gpio.py\` (usb2gpio / Arduino Mega2560 USB serial) to Rust.
- SINE button → generate 1kHz tone at calibrated level
- STOP button → silence
- LEDs reflect active state
- Triggers ZMQ commands internally

Ref: \`ac-rs/PLAN.md\` Phase 4"

gh issue create --repo $REPO \
  --title "Stale server detection for Rust daemon" \
  --label "software,backlog" \
  --body "Python client currently compares \`_SRC_MTIME\` of Python server source files. For the Rust daemon, either:
- Expose build timestamp in the \`status\` reply and update the client to compare it
- Or remove the mechanism entirely (Rust binary won't have source files to stat)

Ref: \`ac/client/ac.py\` \`_ensure_server()\`"

echo ""
echo "Creating testing issues..."

gh issue create --repo $REPO \
  --title "Add transfer command integration tests" \
  --label "testing,backlog" \
  --body "Add to \`tests/test_server_client.py\` once the \`transfer\` command is implemented in the Rust daemon.

- Use the session-scoped \`server_client\` fixture
- Must drain to \`done\`/\`error\` before returning
- Verify H1 magnitude, phase, coherence fields present in frames

Ref: \`TESTING.md\` Adding tests"

echo ""
echo "All done! View issues at: https://github.com/$REPO/issues"
echo "Tip: use 'gh issue list --repo $REPO --label backlog' to see the backlog."
