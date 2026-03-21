"""Sweep results view — 2×2 live-accumulating grid with point-click spectrum detail."""
import numpy as np

from pyqtgraph.Qt import QtCore, QtWidgets, QtGui
import pyqtgraph as pg

from .app import BG, PANEL, GRID, TEXT, BLUE, ORANGE, PURPLE, RED, AMBER, GREEN


class SweepView(QtWidgets.QMainWindow):
    def __init__(self, mode="sweep_frequency"):
        super().__init__()
        self._mode   = mode
        self._is_freq = (mode == "sweep_frequency")
        self._points  = []      # list of sweep_point frames
        self._done    = False
        self._selected_idx = None

        title = "Frequency Sweep" if self._is_freq else "Level Sweep"
        self.setWindowTitle(title)
        self._build_ui()
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

        # Status
        self._status_label = QtWidgets.QLabel("  Sweep running…")
        self._status_label.setStyleSheet(
            f"color: white; background: {PANEL}; padding: 4px 8px; "
            "font-family: monospace; font-size: 13px;"
        )
        layout.addWidget(self._status_label)

        # 2×2 grid via GraphicsLayoutWidget
        glw = pg.GraphicsLayoutWidget()
        glw.setBackground(PANEL)
        layout.addWidget(glw, stretch=1)

        def _make_plot(title, ylabel):
            p = glw.addPlot(title=title)
            p.setLabel("left",   ylabel, color=TEXT)
            p.showGrid(x=True, y=True, alpha=0.3)
            p.getAxis("left").setStyle(tickFont=_mono_font())
            p.getAxis("bottom").setStyle(tickFont=_mono_font())
            if self._is_freq:
                p.setLogMode(x=True, y=False)
                p.getAxis("bottom").setLabel("Frequency (Hz)", color=TEXT)
            else:
                p.getAxis("bottom").setLabel("Level (dBu / dBFS)", color=TEXT)
            return p

        self._p_thd  = _make_plot("THD% vs " + ("Freq" if self._is_freq else "Level"),
                                   "THD (%)")
        self._p_gain = _make_plot("Gain / Response",
                                   "Gain (dB)")
        glw.nextRow()
        self._p_thdn = _make_plot("THD+N% vs " + ("Freq" if self._is_freq else "Level"),
                                   "THD+N (%)")
        self._p_spec  = _make_plot("Spectrum of selected point",
                                    "Level (dBFS)")
        self._p_spec.setLogMode(x=True, y=False)
        self._p_spec.getAxis("bottom").setLabel("Frequency (Hz)", color=TEXT)

        # Curves for top-3 plots
        self._thd_line  = self._p_thd.plot(
            pen=pg.mkPen(BLUE, width=2), symbol="o",
            symbolBrush=BLUE, symbolSize=6, symbolPen=None)
        self._thdn_line = self._p_thdn.plot(
            pen=pg.mkPen(ORANGE, width=2), symbol="o",
            symbolBrush=ORANGE, symbolSize=6, symbolPen=None)
        self._gain_line = self._p_gain.plot(
            pen=pg.mkPen(PURPLE, width=2), symbol="o",
            symbolBrush=PURPLE, symbolSize=6, symbolPen=None)
        self._gain_ref  = pg.InfiniteLine(pos=0, angle=0,
                                           pen=pg.mkPen("#444444", width=1,
                                                        style=QtCore.Qt.PenStyle.DashLine))
        self._p_gain.addItem(self._gain_ref)

        # Clipped point scatter (red X markers)
        self._clip_thd  = pg.ScatterPlotItem(symbol="x", pen=pg.mkPen(RED, width=2),
                                              brush=None, size=12)
        self._clip_thdn = pg.ScatterPlotItem(symbol="x", pen=pg.mkPen(RED, width=2),
                                              brush=None, size=12)
        self._p_thd.addItem(self._clip_thd)
        self._p_thdn.addItem(self._clip_thdn)

        # Selection cursor on THD plot
        self._sel_line = pg.InfiniteLine(
            angle=90, movable=False,
            pen=pg.mkPen("#ffffff", width=1, alpha=100,
                         style=QtCore.Qt.PenStyle.DotLine))
        self._p_thd.addItem(self._sel_line)
        self._sel_line.hide()

        # Spectrum curves
        self._spec_curve    = self._p_spec.plot(pen=pg.mkPen(BLUE, width=1))
        self._spec_harmonic_lines = []

        # Click handler on THD plot for point selection
        self._p_thd.scene().sigMouseClicked.connect(self._on_thd_click)

        # Readout
        self._readout_label = QtWidgets.QLabel("")
        self._readout_label.setStyleSheet(
            f"color: {TEXT}; background: {PANEL}; padding: 3px 8px; "
            "font-family: monospace; font-size: 12px;"
        )
        layout.addWidget(self._readout_label)

    # ------------------------------------------------------------------
    # Frame handler
    # ------------------------------------------------------------------

    def on_frame(self, topic, frame):
        if topic == "error":
            self._status_label.setText(f"  Error: {frame.get('message','?')}")
            return

        if topic == "data" and frame.get("type") == "sweep_point":
            self._points.append(frame)
            self._refresh_plots()
            n = len(self._points)
            self._status_label.setText(f"  Sweep running… {n} point(s) collected")

        elif topic == "done":
            self._done = True
            n = len(self._points)
            xr = frame.get("xruns", 0)
            xr_s = f"  !! {xr} xrun(s)" if xr else ""
            self._status_label.setText(
                f"  Sweep complete — {n} points.{xr_s}  Click a point to inspect spectrum.")
            # Unlock interactive zoom/pan on gain and spectrum only;
            # _p_thd/_p_thdn stay click-only so sigMouseClicked keeps firing
            for p in [self._p_gain, self._p_spec]:
                p.setMouseEnabled(x=True, y=True)

    # ------------------------------------------------------------------
    # Plot refresh
    # ------------------------------------------------------------------

    def _x_vals(self):
        """X-axis values: freq (Hz) for freq sweep, level (dBu or dBFS) for level."""
        pts = self._points
        if self._is_freq:
            return np.array([p.get("freq_hz", p.get("fundamental_hz", 0)) for p in pts])
        # Level sweep: prefer dBu if calibrated
        if pts and pts[0].get("out_dbu") is not None:
            return np.array([p["out_dbu"] for p in pts])
        return np.array([p.get("drive_db", 0) for p in pts])

    def _refresh_plots(self):
        pts  = self._points
        if not pts:
            return

        xs   = self._x_vals()
        thd  = np.array([p["thd_pct"]  for p in pts])
        thdn = np.array([p["thdn_pct"] for p in pts])
        gain = np.array([p.get("gain_db", np.nan) for p in pts])
        clip = np.array([bool(p.get("clipping")) for p in pts])

        # Filter clipped/ac_coupled points from main lines (match matplotlib behaviour)
        valid = np.array([not p.get("clipping") and not p.get("ac_coupled")
                          for p in pts])
        if not valid.any():
            valid = np.ones(len(pts), dtype=bool)

        # Pass raw xs — plots with setLogMode(x=True) apply log10 internally
        self._thd_line.setData(xs[valid], thd[valid])
        self._thdn_line.setData(xs[valid], thdn[valid])
        self._gain_line.setData(xs[valid], gain[valid])

        # Clipped scatter: show all clipped points regardless of valid mask
        if clip.any():
            self._clip_thd.setData(xs[clip], thd[clip])
            self._clip_thdn.setData(xs[clip], thdn[clip])
        else:
            self._clip_thd.setData([], [])
            self._clip_thdn.setData([], [])

        # Auto range while sweep is running
        if not self._done:
            for p in [self._p_thd, self._p_thdn, self._p_gain]:
                p.enableAutoRange(enable=True)

    # ------------------------------------------------------------------
    # Point selection + spectrum
    # ------------------------------------------------------------------

    def _on_thd_click(self, event):
        if not self._points:
            return
        pos = event.scenePos()
        if not self._p_thd.sceneBoundingRect().contains(pos):
            return
        mp = self._p_thd.getViewBox().mapSceneToView(pos)
        click_x = mp.x()

        xs = self._x_vals()
        if self._is_freq:
            log_xs = np.log10(np.maximum(xs, 1.0))
        else:
            log_xs = xs

        dists = np.abs(log_xs - click_x)
        idx   = int(np.argmin(dists))
        self._selected_idx = idx
        self._show_spectrum(idx)

        sel_x = log_xs[idx]
        self._sel_line.setPos(sel_x)
        self._sel_line.show()

    def _show_spectrum(self, idx):
        pt = self._points[idx]
        freqs = pt.get("freqs")
        spec  = pt.get("spectrum")
        if freqs is None or spec is None:
            self._readout_label.setText("  (no spectrum data for this point)")
            return

        freqs = np.array(freqs, dtype=float)
        spec  = np.array(spec,  dtype=float)
        spec_db = 20.0 * np.log10(np.maximum(spec, 1e-12))

        # Pass raw freqs — _p_spec has setLogMode(x=True)
        self._spec_curve.setData(freqs, spec_db)

        # Update harmonic markers
        for ln in self._spec_harmonic_lines:
            self._p_spec.removeItem(ln)
        self._spec_harmonic_lines.clear()

        f0 = pt.get("fundamental_hz")
        sr = pt.get("sr", 48000)
        if f0:
            f_hi = min(sr / 2, 24000)
            for i in range(1, 10):
                hf = f0 * (i + 1)
                if hf > f_hi:
                    break
                ln = pg.InfiniteLine(
                    pos=np.log10(hf), angle=90,
                    pen=pg.mkPen(AMBER, width=1,
                                 style=QtCore.Qt.PenStyle.DashLine),
                    label=f"H{i+1}",
                    labelOpts={"color": AMBER, "position": 0.90,
                               "movable": False, "fill": None},
                )
                self._p_spec.addItem(ln)
                self._spec_harmonic_lines.append(ln)

        # Readout
        thd  = pt.get("thd_pct",  0.0)
        thdn = pt.get("thdn_pct", 0.0)
        if self._is_freq:
            freq_hz = pt.get("freq_hz", pt.get("fundamental_hz", 0))
            x_label = f"{freq_hz:.0f} Hz"
        else:
            out_dbu = pt.get("out_dbu")
            x_label = f"{out_dbu:+.2f} dBu" if out_dbu is not None else f"{pt.get('drive_db',0):.1f} dBFS"
        self._readout_label.setText(
            f"  Selected: {x_label}   THD: {thd:.4f}%   THD+N: {thdn:.4f}%"
        )

    # ------------------------------------------------------------------
    # Keyboard shortcuts
    # ------------------------------------------------------------------

    def keyPressEvent(self, event):
        key = event.key()
        if key in (QtCore.Qt.Key.Key_Q, QtCore.Qt.Key.Key_Escape):
            self.close()
        elif key == QtCore.Qt.Key.Key_F11:
            if self.isFullScreen():
                self.showNormal()
            else:
                self.showFullScreen()
        else:
            super().keyPressEvent(event)


# ---------------------------------------------------------------------------

def _mono_font():
    from pyqtgraph.Qt import QtGui
    f = QtGui.QFont("Monospace")
    f.setStyleHint(QtGui.QFont.StyleHint.TypeWriter)
    f.setPointSize(9)
    return f
