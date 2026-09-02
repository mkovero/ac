//! Launch resolution: request, config and calibration folded into one
//! [`SessionPlan`] before any thread starts.
//!
//! Everything here can refuse the request, and every refusal is a
//! synchronous `{"ok": false, "error": ...}` reply rather than a worker
//! that exits quietly — a launch that returned `ok: true` and then
//! published nothing is indistinguishable to a client from a slow start,
//! which is the failure #254 was.
//!
//! Splitting it out also fixes an ordering hazard by construction. The
//! resolution reads config, ports and calibration; the worker then owns
//! all of it. With both in one function they were one scope, and the
//! reply had to be built from clones taken before the closure captured
//! the originals.

use std::path::PathBuf;

use serde_json::{json, Value};

use ac_core::shared::calibration::Calibration;
use ac_core::visualize::weighting_curves::WeightingCurve;

use crate::handlers::{
    apply_drive_ceiling, make_engine_for_state, ref_output_migration_warning, resolve_output,
    resolve_ref_output,
};
use crate::server::ServerState;

use super::frame::FrameStatics;
use super::pair::PairCtx;
use super::request::{parse_params, TransferParams};

/// Everything a `transfer_stream` session needs that is decided before the
/// worker thread exists, and nothing that needs the thread.
///
/// Moved into the worker whole. The launch reply is built from it before
/// the move, so no field has to be cloned just to be echoed back.
pub(super) struct SessionPlan {
    // ---- launch parameters, past validation ----
    pub(super) drive: bool,
    pub(super) drivable: bool,
    /// Already clamped to `cfg.drive_max_dbfs` (#360), so the stored
    /// `DriveState` never holds an unclamped value whether or not
    /// `set_drive` is ever called in this session.
    pub(super) level_dbfs: f64,
    pub(super) fake: bool,
    pub(super) backend_required: Option<String>,
    pub(super) backend: String,
    pub(super) fake_correlated_pair: Option<(f64, usize)>,
    pub(super) fake_ring_process_secs: Option<f64>,
    pub(super) fake_ring_period: usize,
    pub(super) mtw_ppo: f64,
    pub(super) mtw_n_blocks: usize,
    pub(super) weighting: WeightingCurve,
    pub(super) integration_tag: String,
    pub(super) integration_tau_s: f64,
    pub(super) pairs: Vec<(u32, u32)>,

    // ---- resolved against config, ports and calibration ----
    /// Unique capture channels in port-registration order; each pair
    /// indexes into it.
    pub(super) unique_chans: Vec<u32>,
    pub(super) unique_ports: Vec<String>,
    /// One entry per `unique_chans` entry — the snapshot ring's
    /// per-channel provenance copy, and the source every per-pair view in
    /// `pair_ctx` was taken from.
    pub(super) unique_cals: Vec<Option<Calibration>>,
    pub(super) pair_ctx: Vec<PairCtx>,
    pub(super) out_port: String,
    pub(super) ref_out_port: String,

    // ---- config values the worker needs after `cfg`'s lock is gone ----
    pub(super) snapshot_spool_dir: PathBuf,
    pub(super) snapshot_ring_s: f64,
    /// #225 migration notice, resolved here so the reply does not need the
    /// config again.
    pub(super) migration_warning: Option<String>,
}

impl SessionPlan {
    /// Resolve a launch request. `Err` carries the REP reply to send back
    /// verbatim.
    pub(super) fn resolve(state: &ServerState, cmd: &Value) -> Result<Self, Value> {
        let TransferParams {
            drive,
            drivable,
            level_dbfs,
            fake_correlated_pair,
            fake_ring_process_secs,
            fake_ring_period,
            mtw_ppo,
            mtw_n_blocks,
            pairs,
            weighting,
            integration_tag,
            integration_tau_s,
        } = parse_params(cmd).map_err(|e| json!({"ok": false, "error": e}))?;

        let cfg = state.cfg.lock().unwrap().clone();
        // #360: a second, independent unclamped path from the same field
        // `set_drive` already clamps — this seeds `DriveState` directly when
        // `drive: true`, and is never touched by `set_drive`'s own clamp
        // unless the client calls it again later. Clamped here, before the
        // `DriveState::new` construction below, so the stored state never
        // holds an unclamped value regardless of whether `set_drive` is ever
        // called in this session.
        let level_dbfs = apply_drive_ceiling(cfg.drive_max_dbfs, level_dbfs);
        let capture_ports = crate::handlers::cached_capture_ports(state);

        // Resolve each unique capture channel to a port name once. `unique_ports`
        // drives the JACK port-registration order; each pair indexes into it.
        let mut unique_chans: Vec<u32> = Vec::new();
        for &(m, r) in &pairs {
            for c in [m, r] {
                if !unique_chans.contains(&c) {
                    unique_chans.push(c);
                }
            }
        }
        let mut unique_ports = Vec::with_capacity(unique_chans.len());
        for &ch in &unique_chans {
            match capture_ports.get(ch as usize) {
                Some(p) => unique_ports.push(p.clone()),
                None => {
                    return Err(json!({"ok": false,
                "error": format!("channel {ch} out of range (n_capture={})", capture_ports.len())}))
                }
            }
        }
        let out_port = resolve_output(&cfg, state).map_err(|e| json!({"ok": false, "error": e}))?;
        let ref_out_port =
            resolve_ref_output(&cfg, state).map_err(|e| json!({"ok": false, "error": e}))?;

        // Sync routing-capability check so CPAL-only environments get an
        // immediate REP error instead of a silent worker exit that the UI never
        // sees (the async `send_pub("error", …)` path below is still needed for
        // per-capture failures once the worker is live).
        let probe_eng =
            make_engine_for_state(state).map_err(|e| json!({"ok": false, "error": e}))?;
        if !probe_eng.supports_routing() {
            return Err(json!({
                "ok": false,
                "error": format!("{} backend does not support port routing", probe_eng.backend_name()),
            }));
        }
        let backend = probe_eng.backend_name().to_string();

        let out_ch = cfg.output_channel;

        // One calibration load per unique capture channel, and every
        // per-pair view below is taken from it. `Calibration::load` re-reads
        // and re-parses the whole calibration file on every call, and this
        // handler used to ask the same question of the same file four times
        // over — the ref-curve refusal, the meas mic-curve, the meas and ref
        // full calibrations, and the snapshot ring's per-channel copy.
        //
        // Not only cheaper: it also makes those views consistent by
        // construction. Separate loads read the file at separate instants,
        // so a calibration rewritten mid-launch could give the refusal check
        // a channel with no curve and the correction path one with a curve —
        // the exact split this check exists to prevent.
        let unique_cals: Vec<Option<Calibration>> = unique_chans
            .iter()
            .map(|&ch| Calibration::load(out_ch, ch, None).ok().flatten())
            .collect();
        // Every channel named in `pairs` is in `unique_chans` by
        // construction, so this never misses: `None` means "no calibration
        // stored for this channel", never "channel not found".
        let cal_for = |ch: u32| -> Option<&Calibration> {
            unique_chans
                .iter()
                .position(|&c| c == ch)
                .and_then(|i| unique_cals[i].as_ref())
        };

        // Calibration discipline (#101): a mic-curve on the **reference**
        // channel is a misconfiguration — H1 = Y/X is a ratio, applying a
        // mic correction to the reference leg cancels against the meas-leg
        // correction (or worse, biases it the wrong way when only one leg
        // has a curve). Refuse the request synchronously with a clear
        // message so the user knows to clear the curve or swap channels.
        for &(_, ref_ch) in &pairs {
            if cal_for(ref_ch).is_some_and(|c| c.mic_response.is_some()) {
                return Err(json!({
                    "ok": false,
                    "error": format!(
                        "transfer ref channel {ref_ch} has a mic-curve loaded — \
                         transfer is a ratio, applying a curve to the reference leg \
                         is wrong. Run `ac calibrate mic-curve clear input {ref_ch}` \
                         or swap meas/ref."
                    ),
                }));
            }
        }

        // Session-static per-pair context: buffer indices (precomputed so the
        // worker loop doesn't re-scan per tick) plus the full per-leg
        // calibration that feeds `meas_spectrum`/`ref_spectrum`/`spl`/
        // `cal_tags` (handoff: transfer-frame-v2 M0).
        //
        // `meas_curve` stays a field of its own rather than being read back
        // out of `meas_cal` at each use: the mag/phase/re/im correction path
        // takes the curve alone and is left untouched (additive-only
        // discipline). Both come from the same `unique_cals` load, so they
        // cannot disagree about a channel.
        let pair_ctx: Vec<PairCtx> = pairs
            .iter()
            .enumerate()
            .map(|(pos, &(meas, r))| PairCtx {
                pos,
                meas_ch: meas,
                ref_ch: r,
                mi: unique_chans.iter().position(|&c| c == meas).unwrap(),
                ri: unique_chans.iter().position(|&c| c == r).unwrap(),
                meas_cal: cal_for(meas).cloned(),
                ref_cal: cal_for(r).cloned(),
                meas_curve: cal_for(meas).and_then(|c| c.mic_response.clone()),
            })
            .collect();

        Ok(SessionPlan {
            drive,
            drivable,
            level_dbfs,
            fake: state.fake_audio,
            backend_required: cfg.backend.clone(),
            backend,
            fake_correlated_pair,
            fake_ring_process_secs,
            fake_ring_period,
            mtw_ppo,
            mtw_n_blocks,
            weighting,
            integration_tag,
            integration_tau_s,
            pairs,
            unique_chans,
            unique_ports,
            unique_cals,
            pair_ctx,
            out_port,
            ref_out_port,
            snapshot_spool_dir: ac_core::config::snapshot_spool_dir(&cfg),
            snapshot_ring_s: cfg.snapshot_ring_s,
            migration_warning: ref_output_migration_warning(&cfg),
        })
    }

    /// The `{"ok": true, ...}` launch reply. Built before the plan is
    /// moved into the worker, so the port names and pair list are read
    /// straight off it rather than cloned ahead of the move.
    pub(super) fn launch_reply(&self) -> Value {
        let meas_port = self.unique_ports.first().cloned().unwrap_or_default();
        let ref_port = self
            .unique_ports
            .get(1)
            .cloned()
            .unwrap_or_else(|| meas_port.clone());
        let mut reply = json!({
            "ok":           true,
            "out_port":     self.out_port,
            "meas_port":    meas_port,
            "ref_port":     ref_port,
            "pairs":        self.pairs,
            // Legacy fields — filled with the first pair so old clients keep working.
            "meas_channel": self.pairs.first().map(|p| p.0).unwrap_or(0),
            "ref_channel":  self.pairs.first().map(|p| p.1).unwrap_or(0),
            "backend":      self.backend,
        });
        // #225 migration notice — see `ref_output_migration_warning`. Repeated on
        // every launch reply rather than once, so a client that connects to an
        // already-running daemon still sees it.
        if let Some(w) = &self.migration_warning {
            reply["warnings"] = json!([w]);
        }
        reply
    }

    /// Every frame input fixed for the worker's life, once the sample rate
    /// is known. Consumes the plan's `integration_tag` and the ladder
    /// description, both of which the frame builder then owns.
    pub(super) fn frame_statics(&self, sr: u32, backend: &str) -> FrameStatics {
        // Fixed log-column grid for `meas_spectrum`/`ref_spectrum` (D18) —
        // computed once since it's constant for the worker's lifetime
        // (sr never changes mid-session); `spectrum_to_columns_wire`'s
        // returned freqs are then identical on every tick by construction
        // (AC #1).
        let spec_f_min = 20.0_f64;
        let spec_f_max = sr as f64 / 2.0;
        let spec_n_columns =
            ac_core::visualize::aggregate::transfer_spectrum_n_columns(spec_f_min, spec_f_max);

        // Ladder description, session-static (it derives from `sr`, which does
        // not change mid-session). Shipped with every frame so a consumer can
        // interpret the per-column `stage` index without knowing the layout
        // rules, and so a saved frame stays interpretable if those rules
        // change later.
        let mtw_stages: Value = match ac_core::visualize::mtw::ladder::layout(sr) {
    Ok(l) => Value::Array(
        l.stages
            .iter()
            .map(|s| {
                json!({
                    // `W + hop·(N−1)` — how long this rung takes to
                    // fill its average. Shipped so a viewer can say
                    // how stale a band is without deriving it from
                    // the frame rate.
                    "settling_s": ac_core::visualize::mtw::settling_seconds(s, self.mtw_n_blocks),
                    "decim":     s.decim,
                    "rate":      s.rate,
                    "df":        s.df,
                    "window_s":  s.window_s,
                    "hop_s":     s.hop_s,
                    "f_valid":   s.f_valid,
                    "f_top":     s.f_top,
                    "blend_top": s.blend_top,
                })
            })
            .collect(),
    ),
    Err(_) => Value::Null,
};

        // Every frame input that is fixed for this worker's life, gathered
        // once. `mtw_stages` is moved in rather than cloned per frame.
        FrameStatics {
            sr,
            backend: backend.to_string(),
            spec_f_min,
            spec_f_max,
            spec_n_columns,
            weighting: self.weighting,
            integration_tag: self.integration_tag.clone(),
            mtw_ppo: self.mtw_ppo,
            mtw_n_blocks: self.mtw_n_blocks,
            mtw_stages,
        }
    }
}
