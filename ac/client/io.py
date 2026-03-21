# io.py
import csv
import numpy as np
from ..conversions import fmt_vrms, vrms_to_dbu

def save_csv(results, path):
    fields = ["freq_hz", "drive_db", "out_vrms", "out_dbu",
              "fundamental_dbfs", "in_vrms", "in_dbu",
              "thd_pct", "thdn_pct", "noise_floor_dbfs"]
    rows = []
    for r in results:
        row = dict(r)
        row["freq_hz"] = row.get("freq_hz") or row.get("fundamental_hz")
        rows.append(row)
    with open(path, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields, extrasaction="ignore")
        w.writeheader()
        w.writerows(rows)
    print(f"  CSV  -> {path}")

def print_summary(results, device_name, cal=None):
    if not results:
        return
    clean      = [r for r in results if not r.get("clipping") and not r.get("ac_coupled")]
    clipped_n  = sum(1 for r in results if r.get("clipping"))
    ac_n       = sum(1 for r in results if r.get("ac_coupled"))
    valid      = clean if clean else results
    worst_thd  = max(r["thd_pct"]  for r in valid)
    worst_thdn = max(r["thdn_pct"] for r in valid)
    avg_thd    = float(np.mean([r["thd_pct"] for r in valid]))

    print(f"\n{'='*62}")
    print(f"  SUMMARY -- {device_name}")
    print(f"{'─'*62}")
    print(f"  Levels measured:  {len(results)}")
    if clipped_n:
        print(f"  Clipped points:   {clipped_n}  (excluded)")
    if ac_n:
        print(f"  AC-coupled pts:   {ac_n}  (excluded -- coupling cap rolloff)")
    print(f"  Worst THD:        {worst_thd:.4f}%")
    print(f"  Worst THD+N:      {worst_thdn:.4f}%")
    avg_note = "  (valid points only)" if (clipped_n or ac_n) else ""
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
