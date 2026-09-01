//! Whether a τ reading counts as a measurement — #347.
//!
//! [`measure`] produces one reading; this module runs it twice inside two
//! genuinely separate client lifecycles, compares them, and turns the
//! result into the [`TauOutcome`] that both the `cal_done` frame and the
//! stored [`TauEntry`] derive from. A lone reading is never a storable
//! outcome, which is why the outcome is an enum: `tau_s` simply does not
//! exist on a run that did not corroborate.

mod measure;

use serde_json::{json, Value};

use ac_core::shared::calibration::{compare_tau_readings, TauComparison, TauConditions, TauEntry};

use crate::audio::make_engine;

use measure::{measure_tau, tau_snr_threshold_db, LowSnrRefusal};

/// Method tag stored on every [`TauEntry`] this handler produces. Bumped
/// to `_v2` by #340: the window-sizing change below means a τ captured
/// under the old, uncapped-in-time window (which pinned wrong past
/// ~13–43 ms depending on sample rate, silently) must not go on matching
/// current conditions via `Calibration::tau_for`'s exact lookup — the
/// window it was measured with was never part of `TauConditions`, so the
/// method tag is the only thing that can invalidate it.
pub(super) const TAU_METHOD: &str = "farina_short_ess_v2";

/// Outcome of one independent τ lifecycle attempt (#347): both readings
/// were taken and compared, one lifecycle's peak was below the SNR
/// threshold (#368), or a lifecycle itself failed (engine start /
/// measurement error) before either could happen.
pub(super) enum TauAttempt {
    Compared {
        conditions: TauConditions,
        reading1_s: f64,
        reading2_s: f64,
        comparison: TauComparison,
        /// The worse (lower) of the two lifecycles' pre-impulse SNR. Both
        /// necessarily cleared the τ SNR threshold — a lifecycle that
        /// didn't would have produced [`TauAttempt::LowSnr`] instead — so
        /// this is a diagnostic figure alongside the comparison, not a
        /// second gate.
        pre_impulse_snr_db: f64,
    },
    /// A lifecycle's peak sits below the SNR threshold (#368). Short-
    /// circuits the same way [`TauAttempt::Error`] does: the second
    /// lifecycle does not run once the first has already refused, and
    /// `conditions` is `Some` only when the refusal happened on the second
    /// lifecycle (mirroring `Error`'s own short-circuit shape below).
    LowSnr {
        conditions: Option<TauConditions>,
        pre_impulse_snr_db: f64,
    },
    Error {
        conditions: Option<TauConditions>,
        message: String,
    },
}

/// Run `measure_tau` twice, each inside its own fresh engine lifecycle —
/// `make_engine` → `start` → `measure_tau` → `stop` — and compare the two
/// readings (#347). Each call is a genuinely new JACK client registration
/// (`JackEngine::start` calls `Client::new` fresh every time), which is
/// exactly the boundary the one-period bug lives on: within one lifetime
/// both a real `jack_iodelay` client and `ac-daemon`'s own workers are
/// stable to 0.001 frames, so nothing short of a second, separately
/// re-registered client can catch a shift that only shows up between them.
pub(super) fn measure_tau_twice(
    fake: bool,
    device: u32,
    out_port: &str,
    in_port: &str,
    amp: f64,
) -> TauAttempt {
    let run_once = || -> anyhow::Result<((f64, f64), TauConditions)> {
        let mut eng = make_engine(fake);
        eng.start(std::slice::from_ref(&out_port.to_string()), Some(in_port))?;
        let conditions = TauConditions {
            device,
            backend: eng.backend_name().to_string(),
            sample_rate: eng.sample_rate(),
            period_size: eng.period_size(),
            output_port: out_port.to_string(),
            input_port: in_port.to_string(),
        };
        let reading = measure_tau(&mut *eng, amp);
        eng.set_silence();
        eng.stop();
        reading.map(|r| (r, conditions))
    };

    // #368: a low-SNR refusal is recovered from the error by type, not by
    // matching the message — it is a distinct `tau_state`, and a reworded
    // message must not silently collapse it back into `error`.
    let ((reading1_s, snr1_db), conditions) = match run_once() {
        Ok(r) => r,
        Err(e) => {
            return match e.downcast_ref::<LowSnrRefusal>() {
                Some(refusal) => TauAttempt::LowSnr {
                    conditions: None,
                    pre_impulse_snr_db: refusal.snr_db,
                },
                None => TauAttempt::Error {
                    conditions: None,
                    message: format!("\u{3c4} measurement failed (reading 1 of 2): {e}"),
                },
            }
        }
    };
    let ((reading2_s, snr2_db), conditions2) = match run_once() {
        Ok(r) => r,
        Err(e) => {
            return match e.downcast_ref::<LowSnrRefusal>() {
                Some(refusal) => TauAttempt::LowSnr {
                    conditions: Some(conditions),
                    pre_impulse_snr_db: refusal.snr_db,
                },
                None => TauAttempt::Error {
                    conditions: Some(conditions),
                    message: format!("\u{3c4} measurement failed (reading 2 of 2): {e}"),
                },
            }
        }
    };
    let comparison = compare_tau_readings(
        reading1_s,
        reading2_s,
        conditions2.sample_rate,
        conditions2.period_size,
    );
    TauAttempt::Compared {
        conditions: conditions2,
        reading1_s,
        reading2_s,
        comparison,
        pre_impulse_snr_db: snr1_db.min(snr2_db),
    }
}

/// Everything `calibrate` reports about this run's τ attempt — feeds both
/// the `cal_done` wire frame ([`TauOutcome::write_frame`]) and, on a
/// corroborated result, the [`TauEntry`] appended to `tau_history`
/// ([`TauOutcome::stored_entry`]).
///
/// One reading is never a storable outcome (#347), and that is the whole
/// reason this is an enum rather than a bag of `Option`s: `tau_s` and
/// `agreement_count` exist only on the variant where two independent
/// lifecycles agreed, so a caller cannot reach for either on a run that
/// did not corroborate. The previous shape carried nine parallel
/// `Option` fields and three `.expect("… when tau_state is measured")`
/// at the one call site that stored an entry.
pub(super) enum TauOutcome {
    /// #368: a lifecycle's deconvolved peak sat below the τ SNR threshold,
    /// so the sweep ran but found nothing distinguishable from noise. This
    /// replaces the old `NotMeasuredNoLoopback`, which reported on a
    /// *captured level* measured before the sweep — see [`tau_result`].
    /// `conditions` is `None` when the refusal came from the first
    /// lifecycle, which short-circuits before any were captured.
    NotMeasuredLowSnr {
        conditions: Option<TauConditions>,
        pre_impulse_snr_db: f64,
        snr_threshold_db: f64,
    },
    /// Two independent lifecycles agreed to the whole sample.
    Measured {
        conditions: TauConditions,
        /// Average of the two readings — they agreed to the whole sample,
        /// so neither is more right than the other.
        tau_s: f64,
        agreement_count: u32,
        reading1_s: f64,
        reading2_s: f64,
        /// #368: the worse of the two lifecycles' pre-impulse SNR, and the
        /// threshold it cleared. Reported on every state that reached a
        /// deconvolution, not only on the refusal, so an operator can see
        /// how much margin a *passing* run actually had.
        pre_impulse_snr_db: f64,
        snr_threshold_db: f64,
    },
    /// Both lifecycles ran and their readings disagreed. Nothing is
    /// stored; `periods` separates #347's own root cause (a
    /// graph-buffering shift of whole periods) from any other mismatch.
    Disagree {
        conditions: TauConditions,
        reading1_s: f64,
        reading2_s: f64,
        /// #368: as on [`TauOutcome::Measured`] — both lifecycles cleared
        /// the threshold, they just did not agree with each other.
        pre_impulse_snr_db: f64,
        snr_threshold_db: f64,
        delta_samples: i64,
        periods: Option<i64>,
        message: String,
    },
    /// A lifecycle failed before a comparison was possible. `conditions`
    /// is `Some` only when the first lifecycle got far enough to report
    /// them.
    Error {
        conditions: Option<TauConditions>,
        message: String,
    },
}

impl TauOutcome {
    /// The `tau_state` wire value. See ZMQ.md's `cal_done` table.
    pub(super) fn state(&self) -> &'static str {
        match self {
            Self::NotMeasuredLowSnr { .. } => "not_measured_low_snr",
            Self::Measured { .. } => "measured",
            Self::Disagree { periods, .. } => {
                if periods.is_some() {
                    "disagree_period_shift"
                } else {
                    "disagree_other"
                }
            }
            Self::Error { .. } => "error",
        }
    }

    /// Conditions the attempt ran under, when any were captured.
    /// `calibrate` falls back to the voltage-cal engine's own sample rate
    /// and period size when this is `None`, because ZMQ.md requires
    /// `tau_sample_rate` / `tau_period_size` on every `cal_done`.
    pub(super) fn conditions(&self) -> Option<&TauConditions> {
        match self {
            Self::Measured { conditions, .. } | Self::Disagree { conditions, .. } => {
                Some(conditions)
            }
            Self::NotMeasuredLowSnr { conditions, .. } | Self::Error { conditions, .. } => {
                conditions.as_ref()
            }
        }
    }

    /// The history entry to append, or `None` when this run measured
    /// nothing storable. A lone or disagreeing reading never produces one.
    pub(super) fn stored_entry(&self, method: &str) -> Option<TauEntry> {
        match self {
            Self::Measured {
                conditions,
                tau_s,
                agreement_count,
                ..
            } => Some(TauEntry {
                conditions: conditions.clone(),
                tau_s: *tau_s,
                measured_at: ac_core::shared::time::now_utc_iso8601(),
                method: method.to_string(),
                agreement_count: *agreement_count,
            }),
            _ => None,
        }
    }

    /// Write this outcome's `tau_*` fields into a `cal_done` frame.
    ///
    /// `tau_state`, `tau_s` and `tau_agreement_count` are always present
    /// (the latter two null / `0` when nothing was measured); every other
    /// field appears only on the states ZMQ.md lists it under — a healthy
    /// agreeing run must not serialize `tau_delta_samples` at all (QA
    /// #348, correctness 1).
    pub(super) fn write_frame(&self, frame: &mut Value) {
        frame["tau_state"] = json!(self.state());
        frame["tau_s"] = match self {
            Self::Measured { tau_s, .. } => json!(tau_s),
            _ => Value::Null,
        };
        frame["tau_agreement_count"] = match self {
            Self::Measured {
                agreement_count, ..
            } => json!(agreement_count),
            _ => json!(0),
        };
        // #368: present on every state that reached a deconvolution at
        // least once — `measured`, `not_measured_low_snr`, `disagree_*` —
        // and absent on `error`, which can fail before a peak was ever
        // located.
        if let Self::Measured {
            pre_impulse_snr_db,
            snr_threshold_db,
            ..
        }
        | Self::NotMeasuredLowSnr {
            pre_impulse_snr_db,
            snr_threshold_db,
            ..
        }
        | Self::Disagree {
            pre_impulse_snr_db,
            snr_threshold_db,
            ..
        } = self
        {
            frame["tau_pre_impulse_snr_db"] = json!(pre_impulse_snr_db);
            frame["tau_snr_threshold_db"] = json!(snr_threshold_db);
        }
        match self {
            Self::NotMeasuredLowSnr { .. } => {}
            Self::Measured {
                reading1_s,
                reading2_s,
                ..
            } => {
                frame["tau_reading1_s"] = json!(reading1_s);
                frame["tau_reading2_s"] = json!(reading2_s);
            }
            Self::Disagree {
                reading1_s,
                reading2_s,
                delta_samples,
                periods,
                message,
                ..
            } => {
                frame["tau_reading1_s"] = json!(reading1_s);
                frame["tau_reading2_s"] = json!(reading2_s);
                frame["tau_delta_samples"] = json!(delta_samples);
                if let Some(p) = periods {
                    frame["tau_periods"] = json!(p);
                }
                frame["tau_error"] = json!(message);
            }
            Self::Error { message, .. } => {
                frame["tau_error"] = json!(message);
            }
        }
    }
}

/// Turn a τ attempt into the [`TauOutcome`] `calibrate` reports — the exact
/// decision #281 QA flagged as untestable because it was inlined in the
/// worker closure, reachable only through a full daemon spawn.
///
/// `attempt` always runs (#368). τ used to be gated on the `is_loopback`
/// flag established at step 2 — a captured-level proxy that a hot (+3.01 dB)
/// or low-gain (−4.19 dB) but genuinely patched loopback both fail, and
/// that a loud uncorrelated interferer could pass. The gate now lives
/// inside `measure_tau` itself, on the deconvolved peak's own pre-impulse
/// SNR, so it applies regardless of what step 2 observed and answers the
/// question that actually matters: did this sweep find a real arrival.
pub(super) fn tau_result(attempt: impl FnOnce() -> TauAttempt) -> TauOutcome {
    match attempt() {
        TauAttempt::Error {
            conditions,
            message,
        } => TauOutcome::Error {
            conditions,
            message,
        },
        TauAttempt::LowSnr {
            conditions,
            pre_impulse_snr_db,
        } => TauOutcome::NotMeasuredLowSnr {
            conditions,
            pre_impulse_snr_db,
            snr_threshold_db: tau_snr_threshold_db(),
        },
        TauAttempt::Compared {
            conditions,
            reading1_s,
            reading2_s,
            comparison: TauComparison::Agree,
            pre_impulse_snr_db,
        } => TauOutcome::Measured {
            conditions,
            tau_s: (reading1_s + reading2_s) / 2.0,
            agreement_count: 2,
            reading1_s,
            reading2_s,
            pre_impulse_snr_db,
            snr_threshold_db: tau_snr_threshold_db(),
        },
        TauAttempt::Compared {
            conditions,
            reading1_s,
            reading2_s,
            comparison: TauComparison::Disagree(d),
            pre_impulse_snr_db,
        } => TauOutcome::Disagree {
            conditions,
            reading1_s,
            reading2_s,
            pre_impulse_snr_db,
            snr_threshold_db: tau_snr_threshold_db(),
            delta_samples: d.delta_samples,
            periods: d.periods,
            message: d.message(),
        },
    }
}
#[cfg(test)]
mod tests {
    use super::measure::TAU_SNR_THRESHOLD_DB;
    use super::*;

    fn dummy_conditions() -> TauConditions {
        TauConditions {
            device: 0,
            backend: "fake".to_string(),
            sample_rate: 48_000,
            period_size: Some(1024),
            output_port: "out".to_string(),
            input_port: "in".to_string(),
        }
    }

    /// The `cal_done` τ fields this outcome would ship. Asserting through
    /// the frame rather than the enum's own shape is deliberate: ZMQ.md's
    /// contract is about which keys are *present*, and only a serialized
    /// frame can show a key's absence.
    fn frame_for(outcome: &TauOutcome) -> Value {
        let mut frame = json!({});
        outcome.write_frame(&mut frame);
        frame
    }

    /// #368: replaces `tau_result_no_loopback_short_circuits_without_
    /// measuring` — the pre-attempt `is_loopback` gate that test pinned
    /// down is gone, `attempt` now always runs, and a low-SNR peak is
    /// refused *inside* the attempt instead. This is the "measured because
    /// the gate was deleted" guard AC8 of #368 asks for at the
    /// `tau_result` level: even though `attempt` ran and returned a real
    /// conditions/SNR pair, a `LowSnr` outcome must still surface as
    /// `not_measured_low_snr` rather than being folded into `measured` or
    /// into the generic `error` state.
    #[test]
    fn tau_result_low_snr_reports_new_state_and_fields() {
        let outcome = tau_result(|| TauAttempt::LowSnr {
            conditions: Some(dummy_conditions()),
            pre_impulse_snr_db: -3.45,
        });
        assert_eq!(outcome.state(), "not_measured_low_snr");
        assert!(outcome.conditions().is_some());
        // Refused, so nothing reaches `tau_history`.
        assert!(outcome.stored_entry("m").is_none());
        let f = frame_for(&outcome);
        assert_eq!(f["tau_s"], Value::Null);
        assert_eq!(f["tau_agreement_count"], json!(0));
        assert_eq!(f["tau_pre_impulse_snr_db"].as_f64(), Some(-3.45));
        assert_eq!(
            f["tau_snr_threshold_db"].as_f64(),
            Some(TAU_SNR_THRESHOLD_DB)
        );
        // A refusal is not an error, and carries no readings — neither
        // lifecycle produced one.
        assert!(f.get("tau_error").is_none(), "{f}");
        assert!(f.get("tau_reading1_s").is_none(), "{f}");
    }

    /// #347: two independent readings agreeing is what "measured" means
    /// now — a single reading is no longer a storable outcome, so
    /// `tau_agreement_count` must always be 2 alongside it.
    #[test]
    fn tau_result_agreeing_readings_reports_measured_with_agreement_count() {
        let outcome = tau_result(|| TauAttempt::Compared {
            conditions: dummy_conditions(),
            reading1_s: 0.000_667,
            reading2_s: 0.000_667,
            comparison: TauComparison::Agree,
            pre_impulse_snr_db: 40.0,
        });
        assert_eq!(outcome.state(), "measured");
        assert!(outcome.conditions().is_some());

        let entry = outcome.stored_entry("farina_test").expect("storable");
        assert_eq!(entry.agreement_count, 2);
        assert_eq!(entry.method, "farina_test");
        assert!((entry.tau_s - 0.000_667).abs() < 1e-12);

        let f = frame_for(&outcome);
        assert_eq!(f["tau_agreement_count"], json!(2));
        assert!((f["tau_s"].as_f64().unwrap() - 0.000_667).abs() < 1e-12);
        assert!(f.get("tau_reading1_s").is_some(), "{f}");
        assert!(f.get("tau_reading2_s").is_some(), "{f}");
        assert!(f.get("tau_error").is_none(), "{f}");
        // ZMQ.md: tau_delta_samples / tau_periods are present only on
        // disagree_* — a healthy Agree run must not carry a stray 0 (QA
        // #348 correctness 1).
        assert!(f.get("tau_delta_samples").is_none(), "{f}");
        assert!(f.get("tau_periods").is_none(), "{f}");
        // #368: present on every state that reached deconvolution, so a
        // passing run shows how much margin it actually had.
        assert_eq!(f["tau_pre_impulse_snr_db"].as_f64(), Some(40.0));
        assert_eq!(
            f["tau_snr_threshold_db"].as_f64(),
            Some(TAU_SNR_THRESHOLD_DB)
        );
    }

    #[test]
    fn tau_result_averages_two_agreeing_readings() {
        let outcome = tau_result(|| TauAttempt::Compared {
            conditions: dummy_conditions(),
            reading1_s: 0.001_000_00,
            reading2_s: 0.001_000_02,
            comparison: TauComparison::Agree,
            pre_impulse_snr_db: 40.0,
        });
        let tau_s = outcome.stored_entry("m").expect("measured").tau_s;
        assert!((tau_s - 0.001_000_01).abs() < 1e-9);
    }

    /// #347 acceptance criterion: "two synthetic readings one period_size
    /// apart are refused, with the period named in the message" — rig data
    /// from the issue body (`4262.064 -> 5286.064` at 96 kHz, +1024
    /// exactly).
    #[test]
    fn tau_result_period_shift_disagreement_refuses_and_names_the_period() {
        let comparison =
            compare_tau_readings(4262.064 / 96_000.0, 5286.064 / 96_000.0, 96_000, Some(1024));
        let outcome = tau_result(|| TauAttempt::Compared {
            conditions: dummy_conditions(),
            reading1_s: 4262.064 / 96_000.0,
            reading2_s: 5286.064 / 96_000.0,
            comparison,
            pre_impulse_snr_db: 40.0,
        });
        assert_eq!(outcome.state(), "disagree_period_shift");
        assert!(
            outcome.stored_entry("m").is_none(),
            "a disagreement must never be stored"
        );

        let f = frame_for(&outcome);
        assert_eq!(f["tau_s"], Value::Null);
        assert_eq!(f["tau_agreement_count"], json!(0));
        assert_eq!(f["tau_periods"], json!(1));
        assert_eq!(f["tau_delta_samples"], json!(1024));
        let msg = f["tau_error"].as_str().expect("disagreement message");
        assert!(msg.contains("1 period"), "got {msg}");
        assert!(msg.contains("1024"), "got {msg}");
    }

    /// #347 acceptance criterion: a disagreement that is *not* a period
    /// multiple is a different fault and must say so, not read as the same
    /// "mismatch" as a period-shift.
    #[test]
    fn tau_result_non_period_disagreement_is_a_different_state() {
        let comparison = compare_tau_readings(0.0, 0.000_5, 48_000, Some(1024));
        let outcome = tau_result(|| TauAttempt::Compared {
            conditions: dummy_conditions(),
            reading1_s: 0.0,
            reading2_s: 0.000_5,
            comparison,
            pre_impulse_snr_db: 40.0,
        });
        assert_eq!(outcome.state(), "disagree_other");
        assert!(outcome.stored_entry("m").is_none());

        let f = frame_for(&outcome);
        assert_eq!(f["tau_s"], Value::Null);
        assert!(
            f.get("tau_periods").is_none(),
            "tau_periods belongs only to disagree_period_shift: {f}"
        );
        assert!(f.get("tau_delta_samples").is_some(), "{f}");
        let msg = f["tau_error"].as_str().expect("disagreement message");
        assert!(msg.contains("not a period multiple"), "got {msg}");
    }

    #[test]
    fn tau_result_loopback_err_reports_error_state_and_message() {
        let outcome = tau_result(|| TauAttempt::Error {
            conditions: None,
            message: "\u{3c4} measurement failed (reading 1 of 2): timeout".to_string(),
        });
        assert_eq!(outcome.state(), "error");
        assert!(outcome.conditions().is_none());
        assert!(outcome.stored_entry("m").is_none());

        let f = frame_for(&outcome);
        assert_eq!(f["tau_s"], Value::Null);
        assert!(
            f.get("tau_reading1_s").is_none(),
            "no lifecycle completed, so there is no reading to report: {f}"
        );
        let msg = f["tau_error"].as_str().expect("error message on failure");
        assert!(
            msg.contains("timeout"),
            "error message should name the failure: {msg}"
        );
        // #368: absent on error — a lifecycle can fail before a peak was
        // ever located, so there is no SNR to report.
        assert!(f.get("tau_pre_impulse_snr_db").is_none(), "{f}");
        assert!(f.get("tau_snr_threshold_db").is_none(), "{f}");
    }
}
