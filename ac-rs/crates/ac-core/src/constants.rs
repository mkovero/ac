/// Default sample rate (Hz). Must match JACK/sounddevice session rate.
pub const SAMPLERATE: u32 = 48_000;

/// Default fundamental frequency for THD measurements (Hz).
pub const FUNDAMENTAL_HZ: f64 = 1000.0;

/// Number of harmonics to track (2nd through NUM_HARMONICS+1).
pub const NUM_HARMONICS: usize = 10;

/// Number of warm-up capture blocks before a real measurement.
pub const WARMUP_REPS: u32 = 2;

/// FFT window type (scipy.signal naming; Rust implementation uses symmetric Hann).
pub const FFT_WINDOW: &str = "hann";

/// Conventional 0 dBu reference voltage (Vrms). Most datasheets use 0.775 V.
pub const DBU_REF_VRMS: f64 = 0.7746;

/// Exact mathematical 0 dBu = sqrt(0.001 * 600).
pub const DBU_REF_EXACT: f64 = 0.774_596_67;
