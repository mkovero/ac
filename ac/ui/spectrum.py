"""Spectrum monitor view — live FFT waterfall with peak hold and harmonic markers."""
import numpy as np

from pyqtgraph.Qt import QtCore, QtWidgets
import pyqtgraph as pg

from .app import (BG, PANEL, TEXT, BLUE, AMBER,
                  FreqAxis, mono_font, add_harmonic_markers,
                  status_label, readout_label)


# Smoothing constants (mirrored from tui.py)
_FALL_ALPHA  = 0.20
_PEAK_HOLD   = 6
_PEAK_DECAY  = 1.5


class SpectrumView(QtWidgets.QMainWindow):
    def __init__(self, session_dir=None):
        super().__init__()
        self.setWindowTitle("Spectrum Monitor")
        self._session_dir = session_dir

        # Smoothing state
        self._smooth_db  = None
        self._peak_db    = None
        self._peak_age   = None
        self._last_freqs = None
        self._last_log_f = None
        self._last_smooth = None
        self._y_range_set = False

        # Latest metadata
        self._fundamental_hz = None
        self._harmonic_lines = []   # InfiniteLine items

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

        self._status = status_label()
        self._status.setText("  Waiting for data…")
        layout.addWidget(self._status)

        # --- Plot ---
        freq_axis = FreqAxis(orientation="bottom")
        self._pw = pg.PlotWidget(axisItems={"bottom": freq_axis})
        self._pw.setBackground(PANEL)
        self._pw.setLabel("left",   "Level",     units="dBFS", color=TEXT)
        self._pw.setLabel("bottom", "Frequency (Hz)", color=TEXT)
        self._pw.setXRange(np.log10(20), np.log10(24000), padding=0)
        self._pw.setYRange(-120, 5, padding=0)
        self._pw.showGrid(x=True, y=True, alpha=0.3)
        self._pw.getAxis("left").setStyle(tickFont=mono_font())
        self._pw.getAxis("bottom").setStyle(tickFont=mono_font())
        layout.addWidget(self._pw, stretch=1)

        # Spectrum fill curve
        self._fill_curve = pg.PlotCurveItem(pen=pg.mkPen(BLUE, width=1))
        self._pw.addItem(self._fill_curve)
        self._fill_under = pg.FillBetweenItem(
            self._fill_curve,
            pg.PlotCurveItem([np.log10(20), np.log10(24000)], [-200, -200]),
            brush=pg.mkBrush(74, 158, 255, 40),
        )
        self._pw.addItem(self._fill_under)

        # Peak hold curve
        self._peak_curve = pg.PlotCurveItem(
            pen=pg.mkPen(TEXT, width=1, style=QtCore.Qt.PenStyle.DotLine)
        )
        self._pw.addItem(self._peak_curve)

        # Crosshair
        self._vline = pg.InfiniteLine(angle=90, movable=False,
                                       pen=pg.mkPen("#ffffff", width=1, alpha=80))
        self._hline = pg.InfiniteLine(angle=0,  movable=False,
                                       pen=pg.mkPen("#ffffff", width=1, alpha=80))
        self._pw.addItem(self._vline)
        self._pw.addItem(self._hline)
        self._pw.scene().sigMouseMoved.connect(self._on_mouse_moved)

        self._readout = readout_label()
        layout.addWidget(self._readout)

    # ------------------------------------------------------------------
    # Frame handler (called from ZmqReceiver signal)
    # ------------------------------------------------------------------

    def on_frame(self, topic, frame):
        if topic == "error":
            self._status.setText(f"  Error: {frame.get('message','?')}")
            return
        if topic != "data" or frame.get("type") != "spectrum":
            return

        freqs    = np.array(frame["freqs"],    dtype=float)
        spec_lin = np.array(frame["spectrum"], dtype=float)
        sr       = frame.get("sr", 48000)
        f0       = frame.get("fundamental_hz")
        thd      = frame.get("thd_pct")
        thdn     = frame.get("thdn_pct")
        in_dbu   = frame.get("in_dbu")
        fund_dbfs = frame.get("fundamental_dbfs")

        # Clip to Nyquist / 24 kHz
        f_hi = min(sr / 2, 24000)
        mask = (freqs >= 20) & (freqs <= f_hi)
        freqs    = freqs[mask]
        spec_lin = spec_lin[mask]

        if len(freqs) == 0:
            return

        raw_db = 20.0 * np.log10(np.maximum(spec_lin, 1e-12))
        smooth_db, peak_db = self._update_state(raw_db, len(freqs))

        log_f = np.log10(freqs)

        self._last_log_f = log_f
        self._last_smooth = smooth_db

        self._fill_curve.setData(log_f, smooth_db)
        self._peak_curve.setData(log_f, peak_db)

        # Auto-fit Y range on first frame so noise floor is visible
        if not self._y_range_set:
            self._y_range_set = True
            y_min = max(np.min(smooth_db) - 10, -140)
            y_max = min(np.max(smooth_db) + 10, 10)
            self._pw.setYRange(y_min, y_max, padding=0)

        # Update harmonic markers
        if f0 != self._fundamental_hz:
            self._fundamental_hz = f0
            for line in self._harmonic_lines:
                self._pw.removeItem(line)
            self._harmonic_lines = add_harmonic_markers(self._pw, f0, sr)

        # Status line — show dBFS (matches Y-axis) and dBu if calibrated
        freq_s = f"{f0:.0f} Hz" if f0 else ""
        level_s = ""
        if fund_dbfs is not None:
            level_s = f"  │  {fund_dbfs:.1f} dBFS"
            if in_dbu is not None:
                level_s += f" ({in_dbu:+.2f} dBu)"
        elif in_dbu is not None:
            level_s = f"  │  {in_dbu:+.2f} dBu"
        thd_s  = f"  │  THD: {thd:.4f}%"      if thd     is not None else ""
        thdn_s = f"  │  THD+N: {thdn:.4f}%"   if thdn    is not None else ""
        self._status.setText(f"  {freq_s}{level_s}{thd_s}{thdn_s}")

    # ------------------------------------------------------------------
    # Smoothing (fast attack / slow decay + peak hold — mirrors tui.py)
    # ------------------------------------------------------------------

    def _update_state(self, raw_db, n):
        if self._smooth_db is None or len(self._smooth_db) != n:
            self._smooth_db = raw_db.copy()
            self._peak_db   = raw_db.copy()
            self._peak_age  = np.zeros(n, dtype=int)
            return self._smooth_db.copy(), self._peak_db.copy()

        rise = raw_db >= self._smooth_db
        self._smooth_db[rise]  = raw_db[rise]
        self._smooth_db[~rise] = (self._smooth_db[~rise] * (1.0 - _FALL_ALPHA)
                                  + raw_db[~rise] * _FALL_ALPHA)

        new_peak = raw_db >= self._peak_db
        self._peak_db[new_peak]  = raw_db[new_peak]
        self._peak_age[new_peak] = 0
        self._peak_age[~new_peak] += 1

        falling = self._peak_age > _PEAK_HOLD
        self._peak_db[falling] -= _PEAK_DECAY
        np.clip(self._peak_db, -200, 5, out=self._peak_db)

        return self._smooth_db.copy(), self._peak_db.copy()

    # ------------------------------------------------------------------
    # Mouse crosshair + readout
    # ------------------------------------------------------------------

    def _on_mouse_moved(self, pos):
        if not self._pw.sceneBoundingRect().contains(pos):
            return
        mouse_point = self._pw.getViewBox().mapSceneToView(pos)
        lf = mouse_point.x()   # log10(freq)
        db = mouse_point.y()

        self._vline.setPos(lf)
        self._hline.setPos(db)

        hz = 10 ** lf
        self._readout.setText(f"  \u25b6 {hz:,.1f} Hz   {db:.1f} dBFS")

    # ------------------------------------------------------------------
    # Keyboard shortcuts
    # ------------------------------------------------------------------

    # ------------------------------------------------------------------
    # Save snapshot (S key)
    # ------------------------------------------------------------------

    def _save_snapshot(self):
        import os
        from datetime import datetime
        out_dir = self._session_dir or "."
        os.makedirs(out_dir, exist_ok=True)
        ts = datetime.now().strftime("%Y%m%d_%H%M%S")

        # Save PNG
        from pyqtgraph.exporters import ImageExporter
        exporter = ImageExporter(self._pw.plotItem)
        png_path = os.path.join(out_dir, f"spectrum_{ts}.png")
        exporter.export(png_path)

        # Save CSV
        if self._last_log_f is not None and self._last_smooth is not None:
            csv_path = os.path.join(out_dir, f"spectrum_{ts}.csv")
            freqs_hz = 10 ** self._last_log_f
            with open(csv_path, "w") as f:
                f.write("freq_hz,level_dbfs\n")
                for hz, db in zip(freqs_hz, self._last_smooth):
                    f.write(f"{hz:.2f},{db:.2f}\n")
            self._readout.setText(f"  Saved: {png_path}  +  {csv_path}")
        else:
            self._readout.setText(f"  Saved: {png_path}")

    def _autorange(self):
        """Reset zoom to fit current data (Space key)."""
        self._pw.setXRange(np.log10(20), np.log10(24000), padding=0)
        if self._last_smooth is not None:
            y_min = max(np.min(self._last_smooth) - 10, -140)
            y_max = min(np.max(self._last_smooth) + 10, 10)
            self._pw.setYRange(y_min, y_max, padding=0)
        else:
            self._pw.setYRange(-120, 5, padding=0)

    # ------------------------------------------------------------------
    # Keyboard shortcuts
    # ------------------------------------------------------------------

    def keyPressEvent(self, event):
        key = event.key()
        if key in (QtCore.Qt.Key.Key_Q, QtCore.Qt.Key.Key_Escape):
            self.close()
        elif key == QtCore.Qt.Key.Key_S:
            self._save_snapshot()
        elif key == QtCore.Qt.Key.Key_Space:
            self._autorange()
        elif key == QtCore.Qt.Key.Key_F11:
            if self.isFullScreen():
                self.showNormal()
            else:
                self.showFullScreen()
        else:
            super().keyPressEvent(event)
