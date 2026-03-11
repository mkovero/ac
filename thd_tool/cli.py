# cli.py
import os
import argparse
from datetime import datetime
import sounddevice as sd
from .constants import DEFAULT_INPUT, DEFAULT_OUTPUT, FUNDAMENTAL_HZ
from .calibration import run_calibration, Calibration
from .live import live_monitor
from .sweep import measure_sweep
from .set_output import parse_target_level, set_output_mode
from .io import save_csv, print_summary
from .plotting import plot_results

def main():
    parser = argparse.ArgumentParser(
        description="THD/THD+N measurement tool -- Fireface 400 + Linux",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  List devices:
    python -m thd_tool --list-devices
"""
    )
    parser.add_argument("--list-devices",   action="store_true")
    parser.add_argument("--input",          type=int,   default=DEFAULT_INPUT)
    parser.add_argument("--output",         type=int,   default=DEFAULT_OUTPUT)
    parser.add_argument("--input-channel",  type=int,   default=0)
    parser.add_argument("--output-channel", type=int,   default=0)
    parser.add_argument("--device-name",    type=str,   default="DUT")
    parser.add_argument("--freq",           type=float, default=FUNDAMENTAL_HZ)
    parser.add_argument("--calibrate",      action="store_true")
    parser.add_argument("--cal-level",      type=float, default=-10.0)
    parser.add_argument("--levels",         type=float, nargs=3, metavar=("START", "STOP", "STEP"), default=[-40, 0, 2])
    parser.add_argument("--no-plot",        action="store_true")
    parser.add_argument("--output-dir",     type=str,   default=".")
    parser.add_argument("--live",           action="store_true")
    parser.add_argument("--live-level",     type=float, default=-12.0)
    parser.add_argument("--live-interval",  type=float, default=1.0,
                        help="Display update interval in seconds (default 1.0). ""Analysis window is always DURATION (1s).")
    parser.add_argument("--set-output",     type=str,   default=None, metavar="TARGET")
    parser.add_argument("--tolerance",      type=float, default=1.0)

    args = parser.parse_args()

    if args.list_devices:
        print("\nAvailable audio devices:\n")
        print(sd.query_devices())
        print(f"\nDefault input:  {sd.default.device[0]}")
        print(f"Default output: {sd.default.device[1]}")
        return

    print(f"\n  Input  [{args.input}]: {sd.query_devices(args.input)['name']}")
    print(f"  Output [{args.output}]: {sd.query_devices(args.output)['name']}")
    print(f"  Channels:  in={args.input_channel}  out={args.output_channel}")

    cal = None
    if args.calibrate:
        cal = run_calibration(
            output_device  = args.output,
            input_device   = args.input,
            output_channel = args.output_channel,
            input_channel  = args.input_channel,
            ref_dbfs       = args.cal_level,
            freq           = args.freq,
        )
    else:
        cal = Calibration.load(output_channel=args.output_channel,
                               input_channel=args.input_channel,
                               freq=args.freq)
        if cal is not None:
            print("  Loaded saved calibration:")
            cal.summary()
        else:
            print("  No calibration found for this output/input/freq combination.")
            print("  Run with --calibrate to set levels.")

    if args.live:
        # If --set-output given, convert to dBFS for the live monitor level
        live_dbfs = args.live_level
        if args.set_output and cal is not None and cal.output_ok:
            try:
                from .set_output import parse_target_level
                import math
                target_vrms = parse_target_level(args.set_output)
                live_dbfs   = 20.0 * math.log10(target_vrms / cal.vrms_at_0dbfs_out)
            except Exception:
                pass
        live_monitor(
            input_device   = args.input,
            output_device  = args.output,
            input_channel  = args.input_channel,
            output_channel = args.output_channel,
            level_dbfs     = live_dbfs,
            freq           = args.freq,
            cal            = cal,
            target_vrms    = target_vrms if args.set_output else None,
            interval       = args.live_interval,
        )
        return

    if args.set_output:
        try:
            target_vrms = parse_target_level(args.set_output)
        except ValueError as e:
            print(f"\n  Error parsing --set-output value: {e}")
            return

        if cal is None or not cal.output_ok:
            print("\n  Note: no output calibration -- script cannot calculate")
            print("  the correct dBFS automatically. Playing at -10 dBFS.")
            print("  Run with --calibrate first for accurate output levels.\n")

        set_output_mode(
            output_device  = args.output,
            output_channel = args.output_channel,
            target_vrms    = target_vrms,
            freq           = args.freq,
            cal            = cal,
            tolerance_pct  = args.tolerance,
        )
        return

    results = measure_sweep(
        input_device   = args.input,
        output_device  = args.output,
        level_db_range = tuple(args.levels),
        fundamental    = args.freq,
        input_channel  = args.input_channel,
        output_channel = args.output_channel,
        cal            = cal,
    )

    if not results:
        print("No results recorded.")
        return

    print_summary(results, args.device_name, cal=cal)

    os.makedirs(args.output_dir, exist_ok=True)
    ts        = datetime.now().strftime("%Y%m%d_%H%M%S")
    safe_name = args.device_name.replace(" ", "_")
    csv_path  = os.path.join(args.output_dir, f"{safe_name}_{ts}.csv")
    plot_path = os.path.join(args.output_dir, f"{safe_name}_{ts}.png")

    save_csv(results, csv_path)

    if not args.no_plot:
        plot_results(results, device_name=args.device_name,
                     output_path=plot_path, cal=cal)
