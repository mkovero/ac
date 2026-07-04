# Known-bad fixture: spectrum HF aggregation units bug

Five `ac monitor` CSV exports captured 2026-07-04T13:31:15Z..13:31:41Z,
96 kHz, dual-FFT (N=8192 >=750 Hz, N=65536 <750 Hz), 4096 log columns.

Defect (see spectrum-hf-garbage-report.md): `spectrum_to_columns` aggregation
branch applies db_to_power to linear-amplitude input; output above the
interpolation->aggregation crossover (~6533 Hz, col 3046) is
10*log10(n_src_bins)-driven garbage up to +19.115 dBFS, interleaved with
-240 dBFS floor clamps.

Invariants these files MUST fail under `--verify`:
1. no bin value > 0 dBFS  (876 violations per file above 6533 Hz)
2. meters-vs-spectrum consistency (spectral peak -5 dBFS vs M -79.5 LUFS)

Deterministic signature for regression matching: values 18.538 / 19.115
repeated across the tail bins, identical on all 10 channels.
