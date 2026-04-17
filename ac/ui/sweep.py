"""Sweep results view — frequency sweep: 3 panels; level sweep: single THD panel."""
import os
from datetime import datetime

import numpy as np

from pyqtgraph.Qt import QtCore, QtWidgets
import pyqtgraph as pg

from .app import (PANEL, TEXT, BLUE, ORANGE, PURPLE, RED,
                  _SPINNER, styled_plot, add_harmonic_markers,
                  status_label, readout_label)


def _pct_fmt(val):
    """Format a percentage value with human-readable precision."""
    if val >= 10:
        return f"{val:.0f}%"
    if val >= 1:
        return f"{val:.1f}%"
    if val >= 0.1:
        return f"{val:.2f}%"
    if val >= 0.01:
        return f"{val:.3f}%"
    return f"{val:.4f}%"


_SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"


class SweepView(QtWidgets.QMainWindow):
    def __init__(self, mode="sweep_frequency", session_dir=None):
        super().__init__()
        self._mode   = mode
        self._is_freq = (mode == "sweep_frequency")
        self._points  = []      # list of sweep_point frames
        self._done    = False
        self._selected_idx = None
        self._session_dir = session_dir
        self._spin_idx = 0

        title = "Frequency Sweep" if self._is_freq else "Level Sweep"
        self.setWindowTitle(title)
        self._build_ui()

        # Spinner timer for in-progress indication
        self._spin_timer = QtCore.QTimer()
        self._spin_timer.timeout.connect(self._tick_spinner)
        self._spin_timer.start(100)

        self.showFullScreen()

    # ------------------------------------------------------------------
    # UI construction
    # ------------------------------------------------------------------

    def _build_ui(self):
        central = QtWidgets.QWidget()
        self.setCentralWidget(central)
        layout = QtWidgets.QVBoxLayout(central)
        layout.setSpacing(4)
        layout.setContentsMargins(8, 8, 8, 8)

        self._status = status_label()
        self._status.setText("  Sweep running…")
        layout.addWidget(self._status)

        self._glw = pg.GraphicsLayoutWidget()
        self._glw.setBackground(PANEL)
        layout.addWidget(self._glw, stretch=1)

        use_log = self._is_freq

        # Panel 1: THD + THD+N overlaid
        self._p_thd = styled_plot(self._glw, "THD / THD+N", "Distortion (%)",
                                   log_freq=use_log)
        if not use_log:
            self._p_thd.getAxis("bottom").setLabel("Level (dBu / dBFS)", color=TEXT)
        self._p_thd.getAxis("left").enableAutoSIPrefix(False)
        self._thd_line = self._p_thd.plot(
            pen=pg.mkPen(BLUE, width=2), symbol="o",
            symbolBrush=BLUE, symbolSize=5, symbolPen=None, name="THD")
        self._thdn_line = self._p_thd.plot(
            pen=pg.mkPen(ORANGE, width=2), symbol="o",
            symbolBrush=ORANGE, symbolSize=5, symbolPen=None, name="THD+N")

        # Clipped point scatter (red X markers)
        self._clip_thd = pg.ScatterPlotItem(symbol="x", pen=pg.mkPen(RED, width=2),
                                             brush=None, size=12)
        self._clip_thdn = pg.ScatterPlotItem(symbol="x", pen=pg.mkPen(RED, width=2),
                                              brush=None, size=12)
        self._p_thd.addItem(self._clip_thd)
        self._p_thd.addItem(self._clip_thdn)

        # Selection cursor
        self._sel_line = pg.InfiniteLine(
            angle=90, movable=False,
            pen=pg.mkPen("#ffffff", width=1, alpha=100,
                         style=QtCore.Qt.PenStyle.DotLine))
        self._p_thd.addItem(self._sel_line)
        self._sel_line.hide()

        # Frequency sweep: add gain + spectrum panels
        self._p_gain = None
        self._p_spec = None
        if self._is_freq:
            self._glw.nextRow()

            self._p_gain = styled_plot(self._glw, "Frequency Response", "Gain (dB)",
                                        log_freq=True)
            self._gain_line = self._p_gain.plot(
                pen=pg.mkPen(PURPLE, width=2), symbol="o",
                symbolBrush=PURPLE, symbolSize=5, symbolPen=None)
            self._gain_ref = pg.InfiniteLine(
                pos=0, angle=0,
                pen=pg.mkPen("#444444", width=1, style=QtCore.Qt.PenStyle.DashLine))
            self._p_gain.addItem(self._gain_ref)
            self._p_gain.getAxis("left").enableAutoSIPrefix(False)

            self._glw.nextRow()

            self._p_spec = styled_plot(self._glw, "Spectrum of selected point",
                                        "Level (dBFS)", log_freq=True)
            self._spec_curve = self._p_spec.plot(pen=pg.mkPen(BLUE, width=1))
            self._spec_harmonic_lines = []

            # Link X-axis zoom/pan across all panels
            self._p_gain.setXLink(self._p_thd)
            self._p_spec.setXLink(self._p_thd)

        # Click handler on THD plot for point selection
        self._p_thd.scene().sigMouseClicked.connect(self._on_click)

        self._readout = readout_label()
        layout.addWidget(self._readout)

    # ------------------------------------------------------------------
    # Frame handler
    # ------------------------------------------------------------------

    def _tick_spinner(self):
        if self._done:
            return
        self._spin_idx = (self._spin_idx + 1) % len(_SPINNER)
        n = len(self._points)
        s = _SPINNER[self._spin_idx]
        if n:
            self._status.setText(f"  {s} Measuring… {n} point(s)")
        else:
            self._status.setText(f"  {s} Waiting for data…")

    def on_frame(self, topic, frame):
        if topic == "error":
            self._status.setText(f"  Error: {frame.get('message','?')}")
            self._spin_timer.stop()
            return

        if topic == "data" and frame.get("type") == "sweep_point":
            self._points.append(frame)
            self._refresh_plots()

        elif topic == "done":
            self._done = True
            self._spin_timer.stop()
            n = len(self._points)
            xr = frame.get("xruns", 0)
            xr_s = f"  !! {xr} xrun(s)" if xr else ""
            self._status.setText(
                f"  Sweep complete — {n} points.{xr_s}  Click a point for detail.")
            if self._p_gain:
                self._p_gain.setMouseEnabled(x=True, y=True)
            if self._p_spec:
                self._p_spec.setMouseEnabled(x=True, y=True)

    # ------------------------------------------------------------------
    # Plot refresh
    # ------------------------------------------------------------------

    def _x_vals(self):
        pts = self._points
        if self._is_freq:
            freqs = np.array([p.get("freq_hz", p.get("fundamental_hz", 0)) for p in pts])
            return np.log10(np.maximum(freqs, 1.0))
        if pts and pts[0].get("out_dbu") is not None:
            return np.array([p["out_dbu"] for p in pts])
        return np.array([p.get("drive_db", 0) for p in pts])

    def _refresh_plots(self):
        pts = self._points
        if not pts:
            return

        xs   = self._x_vals()
        thd  = np.array([p["thd_pct"]  for p in pts])
        thdn = np.array([p["thdn_pct"] for p in pts])
        clip = np.array([bool(p.get("clipping")) for p in pts])

        not_clip = np.array([not p.get("clipping") for p in pts])
        show = not_clip if not_clip.any() else np.ones(len(pts), dtype=bool)

        self._thd_line.setData(xs[show], thd[show])
        self._thdn_line.setData(xs[show], thdn[show])

        if self._p_gain:
            gain = np.array([p["gain_db"] if p.get("gain_db") is not None
                             else np.nan for p in pts], dtype=float)
            self._gain_line.setData(xs[show], gain[show])

        if clip.any():
            self._clip_thd.setData(xs[clip], thd[clip])
            self._clip_thdn.setData(xs[clip], thdn[clip])
        else:
            self._clip_thd.setData([], [])
            self._clip_thdn.setData([], [])

        if not self._done:
            # Gain: auto-range with minimum ±0.5 dB span
            if self._p_gain:
                gain = np.array([p["gain_db"] if p.get("gain_db") is not None
                                 else np.nan for p in pts], dtype=float)
                valid_gain = gain[show]
                valid_gain = valid_gain[~np.isnan(valid_gain)]
                if len(valid_gain) > 0:
                    g_min, g_max = np.min(valid_gain), np.max(valid_gain)
                    g_mid = (g_min + g_max) / 2
                    g_half = max((g_max - g_min) / 2 * 1.2, 0.5)
                    self._p_gain.setYRange(g_mid - g_half, g_mid + g_half)

            # Fit THD/THD+N Y-axis tightly to data (avoid defaulting to 0–1)
            all_vals = np.concatenate([thd[show], thdn[show]])
            if len(all_vals) > 0:
                ymin = max(0, np.min(all_vals) * 0.8)
                ymax = np.max(all_vals) * 1.2
                if ymax - ymin < 0.001:
                    ymax = ymin + 0.01
                self._p_thd.setYRange(ymin, ymax)

    # ------------------------------------------------------------------
    # Point selection + spectrum
    # ------------------------------------------------------------------

    def _on_click(self, event):
        if not self._points:
            return
        pos = event.scenePos()
        panels = [self._p_thd]
        if self._p_gain:
            panels.append(self._p_gain)
        for plot in panels:
            if plot.sceneBoundingRect().contains(pos):
                mp = plot.getViewBox().mapSceneToView(pos)
                click_x = mp.x()
                xs = self._x_vals()
                dists = np.abs(xs - click_x)
                idx = int(np.argmin(dists))
                self._selected_idx = idx
                self._show_detail(idx)
                self._sel_line.setPos(xs[idx])
                self._sel_line.show()
                return

    def _show_detail(self, idx):
        pt = self._points[idx]

        # Update spectrum panel (freq sweep only)
        if self._p_spec:
            freqs = pt.get("freqs")
            spec  = pt.get("spectrum")
            if freqs is not None and spec is not None:
                freqs = np.array(freqs, dtype=float)
                spec  = np.array(spec,  dtype=float)
                spec_db = 20.0 * np.log10(np.maximum(spec, 1e-12))
                self._spec_curve.setData(np.log10(np.maximum(freqs, 1.0)), spec_db)

                for ln in self._spec_harmonic_lines:
                    self._p_spec.removeItem(ln)
                self._spec_harmonic_lines.clear()

                f0 = pt.get("fundamental_hz")
                sr = pt.get("sr", 48000)
                if f0:
                    self._spec_harmonic_lines = add_harmonic_markers(self._p_spec, f0, sr)

        # Readout — all numerical values for the selected point
        thd      = pt.get("thd_pct",  0.0)
        thdn     = pt.get("thdn_pct", 0.0)
        gain     = pt.get("gain_db")
        fund_dbfs = pt.get("fundamental_dbfs")
        in_dbu   = pt.get("in_dbu")
        out_dbu  = pt.get("out_dbu")
        noise    = pt.get("noise_floor_dbfs")

        if self._is_freq:
            freq_hz = pt.get("freq_hz", pt.get("fundamental_hz", 0))
            x_label = f"{freq_hz:.0f} Hz"
        else:
            x_label = f"{out_dbu:+.2f} dBu" if out_dbu is not None else f"{pt.get('drive_db',0):.1f} dBFS"

        parts = [f"  {x_label}"]
        parts.append(f"THD: {_pct_fmt(thd)}")
        parts.append(f"THD+N: {_pct_fmt(thdn)}")
        if gain is not None:
            parts.append(f"Gain: {gain:+.3f} dB")
        if fund_dbfs is not None:
            parts.append(f"Fund: {fund_dbfs:.1f} dBFS")
        if in_dbu is not None:
            parts.append(f"In: {in_dbu:+.2f} dBu")
        if out_dbu is not None:
            parts.append(f"Out: {out_dbu:+.2f} dBu")
        if noise is not None:
            parts.append(f"Noise: {noise:.1f} dBFS")
        self._readout.setText("   ".join(parts))

    # ------------------------------------------------------------------
    # Save snapshot (S key)
    # ------------------------------------------------------------------

    def _save_snapshot(self):
        if not self._points:
            self._readout.setText("  Nothing to save yet")
            return
        out_dir = self._session_dir or "."
        os.makedirs(out_dir, exist_ok=True)
        ts = datetime.now().strftime("%Y%m%d_%H%M%S")
        label = "plot" if self._is_freq else "plot_level"

        # Save PNG of the graphics layout
        from pyqtgraph.exporters import ImageExporter
        exporter = ImageExporter(self._glw.scene())
        png_path = os.path.join(out_dir, f"{label}_{ts}.png")
        exporter.export(png_path)

        # Save CSV
        csv_path = os.path.join(out_dir, f"{label}_{ts}.csv")
        with open(csv_path, "w") as f:
            if self._is_freq:
                f.write("freq_hz,thd_pct,thdn_pct,gain_db,fundamental_dbfs,"
                        "out_dbu,in_dbu,noise_floor_dbfs\n")
                for pt in self._points:
                    freq = pt.get("freq_hz", pt.get("fundamental_hz", 0))
                    f.write(f"{freq:.1f},{pt['thd_pct']:.6f},{pt['thdn_pct']:.6f},"
                            f"{pt.get('gain_db','')},{pt.get('fundamental_dbfs','')},"
                            f"{pt.get('out_dbu','')},{pt.get('in_dbu','')},"
                            f"{pt.get('noise_floor_dbfs','')}\n")
            else:
                f.write("drive_dbfs,out_dbu,thd_pct,thdn_pct,gain_db,"
                        "fundamental_dbfs,in_dbu,noise_floor_dbfs\n")
                for pt in self._points:
                    f.write(f"{pt.get('drive_db','')},{pt.get('out_dbu','')},"
                            f"{pt['thd_pct']:.6f},{pt['thdn_pct']:.6f},"
                            f"{pt.get('gain_db','')},{pt.get('fundamental_dbfs','')},"
                            f"{pt.get('in_dbu','')},{pt.get('noise_floor_dbfs','')}\n")

        self._readout.setText(f"  Saved: {png_path}  +  {csv_path}")

    # ------------------------------------------------------------------
    # Keyboard shortcuts
    # ------------------------------------------------------------------

    def keyPressEvent(self, event):
        key = event.key()
        if key in (QtCore.Qt.Key.Key_Q, QtCore.Qt.Key.Key_Escape):
            self.close()
        elif key == QtCore.Qt.Key.Key_S:
            self._save_snapshot()
        elif key == QtCore.Qt.Key.Key_F11:
            if self.isFullScreen():
                self.showNormal()
            else:
                self.showFullScreen()
        else:
            super().keyPressEvent(event)
