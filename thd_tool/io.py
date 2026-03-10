# io.py
import csv
import numpy as np
from .conversions import fmt_vrms, vrms_to_dbu

def save_csv(results, path):
    fields = ["drive_db", "out_vrms", "out_dbu",
              "fundamental_dbfs", "in_vrms", "in_dbu",
              "thd_pct", "thdn_pct", "noise_floor_dbfs"]
    with open(path, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields, extrasaction="ignore")
        w.writeheader()
        w.writerows(results)
    print(f"  CSV  -> {path}")

def print_summary(results, device_name, cal=None):
    if not results:
        return
    worst_thd  = max(r["thd_pct"]  for r in results)
    worst_thdn = max(r["thdn_pct"] for r in results)
    clean      = [r for r in results if not r.get("clipping", False)]
    clipped_n  = len(results) - len(clean)
    avg_src    = clean if clean else results
    avg_thd    = float(np.mean([r["thd_pct"] for r in avg_src]))

    print(f"\n{'='*62}")
    print(f"  SUMMARY -- {device_name}")
    print(f"{'─'*62}")
    print(f"  Levels measured:  {len(results)}")
    if clipped_n:
        print(f"  Clipped points:   {clipped_n}  (excluded from average)")
    print(f"  Worst THD:        {worst_thd:.4f}%")
    print(f"  Worst THD+N:      {worst_thdn:.4f}%")
    avg_note = "  (clean points only)" if clipped_n else ""
    print(f"  Average THD:      {avg_thd:.4f}%{avg_note}")

    if cal and cal.output_ok:
        lo = results[0].get("out_vrms")
        hi = results[-1].get("out_vrms")
        if lo and hi:
            print(f"\n  Output range:  {fmt_vrms(lo)} ({vrms_to_dbu(lo):+.1f} dBu)"
                  f"  ->  {fmt_vrms(hi)} ({vrms_to_dbu(hi):+.1f} dBu)")
    if cal and cal.input_ok:
        iv = [r["in_vrms"] for r in results if r.get("in_vrms")]
        if iv:
            print(f"  DUT out range: {fmt_vrms(min(iv))} ({vrms_to_dbu(min(iv)):+.1f} dBu)"
                  f"  ->  {fmt_vrms(max(iv))} ({vrms_to_dbu(max(iv)):+.1f} dBu)")
    print(f"{'='*62}\n")
