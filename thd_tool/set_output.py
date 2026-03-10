# set_output.py
import numpy as np
import sounddevice as sd
from .signal import make_sine
from .conversions import vrms_to_dbu, vrms_to_vpp, fmt_vrms
from .constants import SAMPLERATE, DURATION, WARMUP_REPS

def parse_target_level(s):
    s = s.strip().lower().replace(" ", "")

    if s.endswith("dbu"):
        dbu = float(s[:-3])
        from .conversions import dbu_to_vrms
        return dbu_to_vrms(dbu)

    if s.endswith("mvpp"):
        return float(s[:-4]) / 1000.0 / (2.0 * np.sqrt(2.0))
    if s.endswith("vpp"):
        return float(s[:-3]) / (2.0 * np.sqrt(2.0))

    if s.endswith("mvrms") or s.endswith("mv") or s.endswith("m"):
        raw = s.rstrip("mvrms").rstrip("mv").rstrip("m")
        return float(raw) / 1000.0

    if s.endswith("vrms") or s.endswith("v"):
        raw = s.rstrip("vrms").rstrip("v")
        return float(raw)

    return float(s)

def set_output_mode(output_device, output_channel=0,
                    target_vrms=None, freq=1000,
                    cal=None, tolerance_pct=1.0):
    if target_vrms is None:
        raise ValueError("target_vrms required")

    target_dbu  = vrms_to_dbu(target_vrms)
    target_vpp  = vrms_to_vpp(target_vrms)
    tol_vrms    = target_vrms * (tolerance_pct / 100.0)

    if cal and cal.output_ok:
        play_dbfs = 20.0 * np.log10(target_vrms / cal.vrms_at_0dbfs_out)
        play_dbfs = max(-60.0, min(play_dbfs, -0.5))
        source_str = (f"{play_dbfs:.1f} dBFS  (calculated from calibration  "
                      f"-> should give ~{fmt_vrms(target_vrms)} at output jack)")
    else:
        play_dbfs  = -10.0
        source_str = f"{play_dbfs:.1f} dBFS  (no calibration -- adjust DUT trimmer to reach target)"

    amplitude = 10.0 ** (play_dbfs / 20.0)
    tone      = make_sine(amplitude, freq, DURATION)

    print(f"\n{'='*64}")
    print(f"  SET OUTPUT  --  {freq:.0f} Hz tone")
    print(f"{'='*64}")
    print(f"\n  Target:   {fmt_vrms(target_vrms)}"
          f"  =  {target_dbu:+.3f} dBu"
          f"  =  {fmt_vpp(target_vrms)}")
    print(f"  Playing:  {source_str}")
    print(f"  Tolerance: ±{tolerance_pct}%  (±{tol_vrms*1000:.3f} mVrms)")
    print(f"\n  Probe your DMM (AC Vrms) at the DUT output.")
    print(f"  Adjust the DUT output trimmer until DMM reads target.")
    print(f"  Enter each DMM reading to check. Press Enter with no")
    print(f"  value when done, or Ctrl+C to abort.\n")

    for _ in range(WARMUP_REPS * 2):
        sd.play(tone, samplerate=SAMPLERATE, device=output_device,
                mapping=[output_channel + 1])
        sd.wait()

    iteration = 0
    confirmed = False

    try:
        while True:
            sd.play(tone, samplerate=SAMPLERATE, device=output_device,
                    mapping=[output_channel + 1])

            iteration += 1
            prompt = f"  [{iteration:02d}] DMM reading (or Enter to finish): "
            raw = input(prompt).strip()

            sd.wait()

            if not raw:
                print("\n  Finished.")
                break

            try:
                measured = parse_target_level(raw)
            except ValueError:
                print("  Couldn't parse -- try: 1.55v  or  1550mv  or  +6dbu")
                continue

            delta_vrms = measured - target_vrms
            delta_pct  = (delta_vrms / target_vrms) * 100.0
            delta_dbu  = vrms_to_dbu(measured) - target_dbu

            if abs(delta_pct) <= tolerance_pct:
                status = "\033[32m  WITHIN TOLERANCE  ✓\033[0m"
                confirmed = True
            elif delta_vrms > 0:
                status = f"\033[33m  too HIGH by {abs(delta_pct):.2f}%  -- trim down\033[0m"
            else:
                status = f"\033[33m  too LOW  by {abs(delta_pct):.2f}%  -- trim up\033[0m"

            print(f"\n      Measured: {fmt_vrms(measured)}"
                  f"  =  {vrms_to_dbu(measured):+.3f} dBu"
                  f"  =  {fmt_vpp(measured)}")
            print(f"      Target:   {fmt_vrms(target_vrms)}"
                  f"  =  {target_dbu:+.3f} dBu")
            print(f"      Delta:    {delta_vrms*1000:+.3f} mVrms"
                  f"  ({delta_pct:+.2f}%)  {delta_dbu:+.3f} dB")
            print(f"      {status}\n")

            if confirmed:
                print(f"  Output confirmed at target. You can proceed.\n")
                break

    except KeyboardInterrupt:
        sd.stop()
        print("\n\n  Aborted.\n")

