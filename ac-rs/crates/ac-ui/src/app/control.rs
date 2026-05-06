//! Daemon control plane — ZMQ REQ (CTRL) commands and the transfer/monitor
//! worker start/stop lifecycle. Methods land in `impl App` here and are
//! dispatched from input.rs (key-driven changes) or from app.rs's
//! ApplicationHandler lifecycle hooks (resumed, exiting).

use crate::data::control::CtrlClient;
use crate::data::types::{CellView, LayoutMode, TransferPair};

use super::helpers::{DataSource, SourceKind};
use super::App;

impl App {
    /// Stop any currently running `transfer_stream` worker and restart it
    /// against the current virtual-channel set. No-op when no pairs are
    /// registered — stopping the worker is enough.
    pub(super) fn restart_transfer_stream(&mut self) {
        self.send_transfer_stream_stop();
        let pairs = self.collect_transfer_pairs();
        if pairs.is_empty() {
            return;
        }
        self.send_transfer_stream_start(0, 0);
    }

    /// Resolve which `TransferPair` the active transfer-derived view
    /// (BodeMag, BodePhase, GroupDelay, Coherence, Nyquist, IR) should
    /// render. Read-only — pair registration is the user's job via
    /// Space-select + `T` (see `KeyT` in input.rs).
    ///
    /// Used to live as `ensure_transfer_pair_for_active` and auto-
    /// registered `(active, active+1)` whenever the user entered a
    /// transfer view. Dropped because the active+1 convention only
    /// holds for two-channel mic setups; multichannel users were
    /// getting unwanted virtual channels they hadn't asked for.
    pub(super) fn resolve_transfer_pair_for_active(
        &self,
    ) -> Option<crate::data::types::TransferPair> {
        let pairs = self.virtual_channels.pairs();
        let active = self.config.active_channel;
        let n_real = self.store.as_ref().map(|s| s.len()).unwrap_or(0);
        resolve_transfer_pair(&pairs, active, n_real)
    }

    pub(super) fn start_data_source(&mut self) {
        let init = match self.init.take() {
            Some(i) => i,
            None => return,
        };
        self.cell_views = vec![CellView::default(); init.store.len()];
        self.selected = vec![false; init.store.len()];
        self.waterfall_inited = vec![false; init.store.len()];
        self.store = Some(init.store);
        let transfer_store = init.transfer_store.clone();
        self.transfer_store = Some(transfer_store.clone());
        self.virtual_channels = init.virtual_channels.clone();
        let virtual_channels = init.virtual_channels.clone();
        let sweep_store = init.sweep_store.clone();
        self.sweep_store = Some(sweep_store.clone());
        let loudness_store = init.loudness_store.clone();
        self.loudness_store = Some(loudness_store.clone());
        let scope_store = init.scope_store.clone();
        self.scope_store = Some(scope_store.clone());
        let ir_store = init.ir_store.clone();
        self.ir_store = Some(ir_store.clone());
        match init.source_kind {
            SourceKind::Synthetic => {
                let (n, bins, rate) = init.synthetic_params.unwrap_or((1, 1000, 10.0));
                let src = crate::data::synthetic::SyntheticSource {
                    n_channels: n,
                    n_bins: bins,
                    update_hz: rate,
                    transfer: transfer_store,
                    virtual_channels,
                };
                let handle = src.spawn(init.inputs, self.wake.clone());
                self.source = Some(DataSource::Synthetic(handle));
            }
            SourceKind::Daemon => {
                let handle = crate::data::receiver::spawn(
                    init.endpoint,
                    init.inputs,
                    transfer_store,
                    virtual_channels,
                    sweep_store,
                    loudness_store,
                    scope_store,
                    ir_store,
                    self.wake.clone(),
                );
                self.source = Some(DataSource::Receiver(handle));
                if !matches!(self.config.layout, LayoutMode::Sweep) {
                    self.send_monitor_spectrum_start();
                }
            }
        }
    }

    /// Lazy-connect the CTRL REQ socket on first use. Called from the
    /// transfer-stream start/stop path. If the daemon isn't up the socket
    /// connect will still succeed (ZMQ is async) but `send` will time out.
    fn ensure_ctrl(&mut self) -> Option<&CtrlClient> {
        if self.ctrl.is_none() {
            match CtrlClient::connect(&self.ctrl_endpoint) {
                Ok(c) => self.ctrl = Some(c),
                Err(e) => {
                    log::warn!("ctrl client connect failed: {e}");
                    return None;
                }
            }
        }
        self.ctrl.as_ref()
    }

    /// Tell the daemon to switch `monitor_spectrum`'s analysis path between
    /// FFT and Morlet CWT. No-op on the synthetic backend (no daemon). On
    /// success the local `analysis_mode` is updated so the W cycle can pick
    /// the next state; on failure we leave it unchanged and notify.
    pub(super) fn send_set_analysis_mode(&mut self, mode: &str) -> bool {
        if matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            self.analysis_mode = mode.to_string();
            return true;
        }
        let sigma = self.cwt_sigma;
        let n_scales = self.cwt_n_scales;
        let Some(ctrl) = self.ensure_ctrl() else {
            self.notify("analysis_mode: no ctrl");
            return false;
        };
        let cmd = serde_json::json!({
            "cmd":      "set_analysis_mode",
            "mode":     mode,
            "sigma":    sigma,
            "n_scales": n_scales,
        });
        match ctrl.send(&cmd) {
            Ok(reply) => {
                if reply.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                    self.analysis_mode = mode.to_string();
                    true
                } else {
                    let err = reply.get("error").and_then(|v| v.as_str()).unwrap_or("?");
                    self.notify(&format!("analysis_mode: {err}"));
                    false
                }
            }
            Err(e) => {
                log::warn!("set_analysis_mode failed: {e}");
                self.notify("analysis_mode: ctrl error");
                false
            }
        }
    }

    /// Push the local `mic_correction_enabled` flag to the daemon. Fire-
    /// and-forget — the toggle is per-session and a missed roundtrip just
    /// means the next frame keeps the previous tag (the local flag is the
    /// authoritative UI-side value).
    pub(super) fn send_mic_correction_enabled(&mut self) {
        if matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            return;
        }
        let enabled = self.mic_correction_enabled;
        let Some(ctrl) = self.ensure_ctrl() else {
            self.notify("mic-cal: no ctrl");
            return;
        };
        let cmd = serde_json::json!({
            "cmd":     "set_mic_correction_enabled",
            "enabled": enabled,
        });
        if let Err(e) = ctrl.send(&cmd) {
            log::warn!("set_mic_correction_enabled failed: {e}");
            self.notify("mic-cal: ctrl error");
        }
    }

    pub(super) fn send_cwt_params(&mut self) {
        if self.analysis_mode != "cwt" {
            return;
        }
        self.send_set_analysis_mode("cwt");
    }

    /// Push the current `ioct_bpo` to the daemon. `None` → `bpo: 0`
    /// disables the per-tick fractional-octave publish; `Some(N)` enables
    /// it. Synthetic backend has no daemon; silent no-op.
    pub(super) fn send_ioct_bpo(&mut self) {
        if matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            return;
        }
        let bpo = self.ioct_bpo.unwrap_or(0);
        let Some(ctrl) = self.ensure_ctrl() else {
            self.notify("ioct: no ctrl");
            return;
        };
        let cmd = serde_json::json!({ "cmd": "set_ioct_bpo", "bpo": bpo });
        if let Err(e) = ctrl.send(&cmd) {
            log::warn!("set_ioct_bpo failed: {e}");
            self.notify("ioct: ctrl error");
        }
    }

    /// Push the current `band_weighting` mode to the daemon.
    pub(super) fn send_band_weighting(&mut self) {
        if matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            return;
        }
        let mode = self.band_weighting.as_str();
        let Some(ctrl) = self.ensure_ctrl() else {
            self.notify("wt: no ctrl");
            return;
        };
        let cmd = serde_json::json!({ "cmd": "set_band_weighting", "mode": mode });
        if let Err(e) = ctrl.send(&cmd) {
            log::warn!("set_band_weighting failed: {e}");
            self.notify("wt: ctrl error");
        }
    }

    /// Push the current `time_integration` mode to the daemon.
    pub(super) fn send_time_integration(&mut self) {
        if matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            return;
        }
        let mode = self.time_integration.as_str();
        let Some(ctrl) = self.ensure_ctrl() else {
            self.notify("ti: no ctrl");
            return;
        };
        let cmd = serde_json::json!({ "cmd": "set_time_integration", "mode": mode });
        if let Err(e) = ctrl.send(&cmd) {
            log::warn!("set_time_integration failed: {e}");
            self.notify("ti: ctrl error");
        }
    }

    /// Ask the daemon to zero the Leq accumulators.
    pub(super) fn send_reset_leq(&mut self) {
        if matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            return;
        }
        let Some(ctrl) = self.ensure_ctrl() else {
            self.notify("ti: no ctrl");
            return;
        };
        let cmd = serde_json::json!({ "cmd": "reset_leq" });
        if let Err(e) = ctrl.send(&cmd) {
            log::warn!("reset_leq failed: {e}");
        }
    }

    /// Ask the daemon to zero the BS.1770-5 loudness accumulators and
    /// clear the local store so the overlay snaps to `—` immediately
    /// (otherwise a stale reading would linger until the next frame
    /// arrives with `-∞`).
    pub(super) fn send_reset_loudness(&mut self) {
        if let Some(store) = self.loudness_store.as_ref() {
            store.clear();
        }
        if matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            return;
        }
        let Some(ctrl) = self.ensure_ctrl() else {
            self.notify("loudness: no ctrl");
            return;
        };
        let cmd = serde_json::json!({ "cmd": "reset_loudness" });
        if let Err(e) = ctrl.send(&cmd) {
            log::warn!("reset_loudness failed: {e}");
        }
    }

    /// Sample rate of the most recent real-channel frame, or 48 kHz if no
    /// frame has arrived yet. Used by `auto_monitor_interval_ms` to pick a
    /// tick that matches the actual capture rate rather than assuming one.
    pub(super) fn current_sr(&self) -> u32 {
        self.last_frames
            .iter()
            .flatten()
            .map(|f| f.meta.sr)
            .find(|&sr| sr > 0)
            .unwrap_or(48_000)
    }

    /// Push the current `monitor_interval_ms` + `monitor_fft_n` to the daemon
    /// via `set_monitor_params`. Silent no-op on the synthetic backend.
    pub(super) fn send_monitor_params(&mut self) {
        if matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            return;
        }
        if !self.monitor_spectrum_active {
            return;
        }
        let interval = self.monitor_interval_ms as f64 / 1000.0;
        let fft_n = self.monitor_fft_n;
        let Some(ctrl) = self.ensure_ctrl() else { return };
        let cmd = serde_json::json!({
            "cmd":      "set_monitor_params",
            "interval": interval,
            "fft_n":    fft_n,
        });
        if let Err(e) = ctrl.send(&cmd) {
            log::warn!("set_monitor_params failed: {e}");
        }
    }

    /// Every pair the worker needs to service — one per registered virtual
    /// channel. Extracted as a helper so `restart_transfer_stream` and the
    /// worker start path see the same set.
    fn collect_transfer_pairs(&self) -> Vec<TransferPair> {
        self.virtual_channels.pairs()
    }

    fn send_transfer_stream_start(&mut self, _meas_ch: usize, _ref_ch: usize) {
        let pairs = self.collect_transfer_pairs();
        if pairs.is_empty() {
            return;
        }
        // Synthetic mode: no daemon involved — the synthetic worker reads
        // `virtual_channels.pairs()` directly on each tick, so we just flip
        // the active flag. The renderer and overlay don't care where the
        // frame came from.
        if matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            self.transfer_stream_active = true;
            self.notify(&format!("transfer_stream: {} pair(s) (synthetic)", pairs.len()));
            return;
        }
        // `transfer_stream` is in the `Transfer` group — coexists with the
        // running `monitor_spectrum` (`Input`) because each worker owns its
        // own JACK client, so no need to pause monitor here.
        let Some(ctrl) = self.ensure_ctrl() else { return };
        // Passive mode: don't ask the daemon to drive the output. The user
        // wires their own stimulus (pink, sweep, speech, music) into the
        // meas/ref inputs externally and we just compute H1 against it.
        let pairs_json: Vec<[u32; 2]> =
            pairs.iter().map(|p| [p.meas, p.ref_ch]).collect();
        let cmd = serde_json::json!({
            "cmd":   "transfer_stream",
            "pairs": pairs_json,
        });
        log::info!("transfer_stream: sending start pairs={pairs_json:?}");
        match ctrl.send(&cmd) {
            Ok(reply) => {
                log::info!("transfer_stream reply: {reply}");
                if reply.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                    self.transfer_stream_active = true;
                    self.notify(&format!("transfer_stream: {} pair(s) live", pairs.len()));
                } else {
                    let err = reply.get("error").and_then(|v| v.as_str()).unwrap_or("?");
                    self.notify(&format!("transfer_stream: {err}"));
                }
            }
            Err(e) => {
                log::warn!("transfer_stream start failed: {e}");
                self.notify("transfer_stream: ctrl error");
            }
        }
    }

    pub(super) fn send_transfer_stream_stop(&mut self) {
        if !self.transfer_stream_active {
            return;
        }
        if !matches!(self.source.as_ref(), Some(DataSource::Synthetic(_))) {
            if let Some(ctrl) = self.ensure_ctrl() {
                let cmd = serde_json::json!({ "cmd": "stop", "name": "transfer_stream" });
                let _ = ctrl.send(&cmd);
            }
        }
        self.transfer_stream_active = false;
        if let Some(ts) = self.transfer_store.as_ref() {
            ts.clear();
        }
        self.transfer_last = None;
    }

    /// Ask the daemon to start publishing spectrum frames. `ac-ui` is a
    /// passive SUB otherwise — without this call every view stays blank.
    /// Requests one slot per preallocated channel so the grid / overlay
    /// layouts can display every input the daemon exposes.
    fn send_monitor_spectrum_start(&mut self) {
        if self.monitor_spectrum_active {
            return;
        }
        if matches!(self.source, Some(DataSource::Synthetic(_))) {
            return;
        }
        let n = self.store.as_ref().map(|s| s.len()).unwrap_or(0);
        if n == 0 {
            return;
        }
        let channels: Vec<u32> = self.monitor_channels.clone()
            .unwrap_or_else(|| (0..n as u32).collect());
        let interval = self.monitor_interval_ms as f64 / 1000.0;
        let fft_n = self.monitor_fft_n;
        let Some(ctrl) = self.ensure_ctrl() else { return };
        let cmd = serde_json::json!({
            "cmd":      "monitor_spectrum",
            "interval": interval,
            "fft_n":    fft_n,
            "channels": channels,
        });
        log::info!("monitor_spectrum: sending start channels={channels:?}");
        match ctrl.send(&cmd) {
            Ok(reply) => {
                log::info!("monitor_spectrum reply: {reply}");
                if reply.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                    self.monitor_spectrum_active = true;
                } else {
                    let err = reply.get("error").and_then(|v| v.as_str()).unwrap_or("?");
                    self.notify(&format!("monitor_spectrum: {err}"));
                }
            }
            Err(e) => {
                log::warn!("monitor_spectrum start failed: {e}");
                self.notify("monitor_spectrum: ctrl error");
            }
        }
    }

    pub(super) fn send_monitor_spectrum_stop(&mut self) {
        if !self.monitor_spectrum_active {
            return;
        }
        if let Some(ctrl) = self.ensure_ctrl() {
            let cmd = serde_json::json!({ "cmd": "stop", "name": "monitor_spectrum" });
            let _ = ctrl.send(&cmd);
        }
        self.monitor_spectrum_active = false;
    }
}

/// Free-function core of `resolve_transfer_pair_for_active` — the
/// `App`-tied wrapper above just plumbs in `self.virtual_channels`,
/// `self.config.active_channel`, and `self.store.len()`. Lifted out
/// so the resolution rule can be unit-tested without standing up an
/// App + render context.
///
/// Resolution:
/// - No pairs registered → `None`. Overlay hints at Space+T.
/// - `active >= n_real` (Tab'd onto a virtual channel slot) →
///   `pairs[active - n_real]` (or `None` if out of range, e.g. a
///   pair was just removed and the active index hasn't been
///   clamped yet — defensive).
/// - `active < n_real` (a real channel) → `pairs[0]`. Lets
///   "press W → BodeMag" show *something* without forcing the
///   user to Tab onto a virtual channel first; they can still Tab
///   to switch between registered pairs.
fn resolve_transfer_pair(
    pairs: &[TransferPair],
    active: usize,
    n_real: usize,
) -> Option<TransferPair> {
    if pairs.is_empty() {
        return None;
    }
    if active >= n_real {
        pairs.get(active - n_real).copied()
    } else {
        pairs.first().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(meas: u32, ref_ch: u32) -> TransferPair {
        TransferPair { meas, ref_ch }
    }

    /// No registered pairs → `None`, regardless of `active` / `n_real`.
    /// This is what triggers the "Space-select MEAS + REF, then T"
    /// caption on transfer views before the user has wired anything.
    #[test]
    fn resolve_none_when_no_pairs_registered() {
        assert_eq!(resolve_transfer_pair(&[], 0, 4), None);
        assert_eq!(resolve_transfer_pair(&[], 5, 4), None);
        assert_eq!(resolve_transfer_pair(&[], 0, 0), None);
    }

    /// Active is on a real channel (`active < n_real`) → fall back to
    /// the first registered pair so entering BodeMag still shows
    /// something familiar; the user can Tab onto a virtual cell to
    /// pick a different pair.
    #[test]
    fn resolve_first_pair_when_active_is_real_channel() {
        let pairs = vec![pair(0, 1), pair(2, 3)];
        assert_eq!(resolve_transfer_pair(&pairs, 0, 4), Some(pair(0, 1)));
        assert_eq!(resolve_transfer_pair(&pairs, 1, 4), Some(pair(0, 1)));
        assert_eq!(resolve_transfer_pair(&pairs, 3, 4), Some(pair(0, 1)));
    }

    /// Active is on a virtual channel slot (`active >= n_real`) →
    /// resolve to that slot's pair. This is what makes Tab cycling
    /// through virtual channels feel right: each Tab step changes the
    /// pair the transfer view is rendering.
    #[test]
    fn resolve_indexed_pair_when_active_is_virtual_slot() {
        let pairs = vec![pair(0, 1), pair(2, 3), pair(4, 5)];
        // n_real = 4. Active = 4 → first virtual pair.
        assert_eq!(resolve_transfer_pair(&pairs, 4, 4), Some(pair(0, 1)));
        // Active = 5 → second virtual pair.
        assert_eq!(resolve_transfer_pair(&pairs, 5, 4), Some(pair(2, 3)));
        // Active = 6 → third virtual pair.
        assert_eq!(resolve_transfer_pair(&pairs, 6, 4), Some(pair(4, 5)));
    }

    /// Defensive: if `active` points past the end of the registered
    /// pairs (e.g. a pair was just removed and the active index
    /// hasn't been clamped yet) the resolver returns `None` rather
    /// than panicking. The caller (and overlay) treats that as
    /// "no pair this tick", which the next input event will fix.
    #[test]
    fn resolve_none_when_active_past_virtual_pairs() {
        let pairs = vec![pair(0, 1)];
        // n_real = 2; one virtual pair → only active=2 is valid.
        // active=3 falls off the end of pairs.
        assert_eq!(resolve_transfer_pair(&pairs, 3, 2), None);
    }

    /// Edge case: `n_real = 0` (synthetic fallback before any real
    /// channels exist). Every `active` is a virtual slot.
    #[test]
    fn resolve_with_no_real_channels() {
        let pairs = vec![pair(0, 1), pair(2, 3)];
        assert_eq!(resolve_transfer_pair(&pairs, 0, 0), Some(pair(0, 1)));
        assert_eq!(resolve_transfer_pair(&pairs, 1, 0), Some(pair(2, 3)));
        assert_eq!(resolve_transfer_pair(&pairs, 2, 0), None);
    }
}
