//! Daemon control plane — ZMQ REQ (CTRL) commands and the transfer/monitor
//! worker start/stop lifecycle. Methods land in `impl App` here and are
//! dispatched from input.rs (key-driven changes) or from app.rs's
//! ApplicationHandler lifecycle hooks (resumed, exiting).

use crate::data::control::CtrlClient;
use crate::data::types::{CellView, LayoutMode, TransferPair};

use super::helpers::{DataSource, SourceKind};
use super::App;

impl App {
    /// Active meas channel under the current Transfer convention. `None`
    /// means the selection is too small (< 2) or the resolved index is
    /// out-of-range; the overlay hint shows up in that case.
    pub(super) fn transfer_active_meas(&self) -> Option<usize> {
        let n = self.selection_order.len();
        if n < 2 {
            return None;
        }
        let meas_count = n - 1;
        let idx = self.active_meas_idx.min(meas_count - 1);
        Some(self.selection_order[idx])
    }

    fn transfer_ref_channel(&self) -> Option<usize> {
        if self.selection_order.len() < 2 {
            return None;
        }
        self.selection_order.last().copied()
    }

    /// Stop any currently running `transfer_stream` worker and restart it
    /// with the current union of virtual-channel pairs plus (if in L-transfer
    /// layout) the active meas/ref pair. No-op when the union is empty —
    /// stopping the worker is enough.
    pub(super) fn restart_transfer_stream(&mut self) {
        self.send_transfer_stream_stop();
        let pairs = self.collect_transfer_pairs();
        if pairs.is_empty() {
            return;
        }
        // Args are unused in the new pairs-based implementation; kept for
        // call-site compatibility in case we ever revert.
        self.send_transfer_stream_start(0, 0);
    }

    /// Called after `config.layout` has been advanced by the `l` key. Starts
    /// the transfer_stream worker when entering Transfer (if the pair is
    /// ready) and stops it when leaving — *unless* virtual channels are
    /// registered, in which case the worker stays live to keep feeding them
    /// across layout changes.
    pub(super) fn on_layout_changed(&mut self, prev: LayoutMode, next: LayoutMode) {
        let entering = !matches!(prev, LayoutMode::Transfer)
            && matches!(next, LayoutMode::Transfer);
        let leaving = matches!(prev, LayoutMode::Transfer)
            && !matches!(next, LayoutMode::Transfer);
        if entering {
            // Start from the first meas on fresh entry so the user doesn't
            // inherit stale Tab state from a previous Transfer session.
            self.active_meas_idx = 0;
            if self.transfer_active_meas().is_some() {
                self.restart_transfer_stream();
            } else if self.virtual_channels.is_empty() {
                self.notify("transfer: pick ≥ 2 channels (last = REF)");
            } else {
                // Worker already live serving virtual channels — nothing to
                // restart, the layout just has no legacy pair to display.
            }
        } else if leaving {
            // Virtual channels keep the worker alive across layout changes;
            // only fully stop if there's nothing left to stream.
            if self.virtual_channels.is_empty() {
                self.send_transfer_stream_stop();
            } else {
                // Drop the L-layout pair from the worker's set.
                self.restart_transfer_stream();
            }
            // Resume spectrum publishing that was paused when we entered
            // Transfer. No-op if it's already running (e.g. the user never
            // had a valid pair so we never actually stopped it).
            self.send_monitor_spectrum_start();
        }
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

    /// Union of every pair the worker needs to service: every registered
    /// virtual channel plus, if the user is currently in the legacy
    /// L-transfer layout, the (active_meas, ref) pair that view points at —
    /// dedup'd so the worker doesn't compute the same H1 twice.
    fn collect_transfer_pairs(&self) -> Vec<TransferPair> {
        let mut pairs = self.virtual_channels.pairs();
        if matches!(self.config.layout, LayoutMode::Transfer) {
            if let (Some(meas), Some(refc)) =
                (self.transfer_active_meas(), self.transfer_ref_channel())
            {
                let layout_pair = TransferPair { meas: meas as u32, ref_ch: refc as u32 };
                if !pairs.iter().any(|p| *p == layout_pair) {
                    pairs.push(layout_pair);
                }
            }
        }
        pairs
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
