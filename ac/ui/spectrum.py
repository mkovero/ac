"""Spectrum monitor view — live FFT waterfall with peak hold and harmonic markers."""
import numpy as np

from pyqtgraph.Qt import QtCore, QtWidgets
import pyqtgraph as pg

from .app import BG, PANEL, GRID, TEXT, BLUE, AMBER, RED


# Smoothing constants (mirrored from tui.py)
_FALL_ALPHA  = 0.20
_PEAK_HOLD   = 6
_PEAK_DECAY  = 1.5


class SpectrumView(QtWidgets.QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("Spectrum Monitor")

        # Smoothing state
        self._smooth_db  = None
        self._peak_db    = None
        self._peak_age   = None
        self._last_freqs = None

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

        # --- Status bar (top) ---
        self._status_label = QtWidgets.QLabel("  Waiting for data…")
        self._status_label.setStyleSheet(
            f"color: white; background: {PANEL}; padding: 4px 8px; "
            "font-family: monospace; font-size: 13px;"
        )
        layout.addWidget(self._status_label)

        # --- Plot ---
        self._pw = pg.PlotWidget()
        self._pw.setBackground(PANEL)
        self._pw.setLabel("left",   "Level",     units="dBFS", color=TEXT)
        self._pw.setLabel("bottom", "Frequency", units="Hz",   color=TEXT)
        self._pw.setLogMode(x=True, y=False)
        self._pw.setXRange(np.log10(20), np.log10(24000), padding=0)
        self._pw.setYRange(-120, 5, padding=0)
        self._pw.showGrid(x=True, y=True, alpha=0.3)
        self._pw.getAxis("left").setStyle(tickFont=_mono_font())
        self._pw.getAxis("bottom").setStyle(tickFont=_mono_font())
        self._pw.getAxis("bottom").setTicks([
            [(np.log10(f), str(f)) for f in
             [20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000]]
        ])
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

        # --- Readout bar (bottom) ---
        self._readout_label = QtWidgets.QLabel("")
        self._readout_label.setStyleSheet(
            f"color: {TEXT}; background: {PANEL}; padding: 3px 8px; "
            "font-family: monospace; font-size: 12px;"
        )
        layout.addWidget(self._readout_label)

    # ------------------------------------------------------------------
    # Frame handler (called from ZmqReceiver signal)
    # ------------------------------------------------------------------

    def on_frame(self, topic, frame):
        if topic == "error":
            self._status_label.setText(f"  Error: {frame.get('message','?')}")
            return
        if topic != "data" or frame.get("type") != "spectrum":
            return

        freqs   = np.array(frame["freqs"],    dtype=float)
        spec_lin = np.array(frame["spectrum"], dtype=float)
        sr      = frame.get("sr", 48000)
        f0      = frame.get("fundamental_hz")
        thd     = frame.get("thd_pct")
        thdn    = frame.get("thdn_pct")
        in_dbu  = frame.get("in_dbu")

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

        self._fill_curve.setData(log_f, smooth_db)
        self._peak_curve.setData(log_f, peak_db)

        # Update harmonic markers
        if f0 != self._fundamental_hz:
            self._fundamental_hz = f0
            self._update_harmonic_markers(f0, sr)

        # Status line
        freq_s = f"{f0:.0f} Hz" if f0 else ""
        dbu_s  = f"  │  {in_dbu:+.2f} dBu"   if in_dbu  is not None else ""
        thd_s  = f"  │  THD: {thd:.4f}%"      if thd     is not None else ""
        thdn_s = f"  │  THD+N: {thdn:.4f}%"   if thdn    is not None else ""
        self._status_label.setText(f"  {freq_s}{dbu_s}{thd_s}{thdn_s}")

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
    # Harmonic markers
    # ------------------------------------------------------------------

    def _update_harmonic_markers(self, f0, sr):
        for line in self._harmonic_lines:
            self._pw.removeItem(line)
        self._harmonic_lines.clear()

        if not f0:
            return

        f_hi = min(sr / 2, 24000)
        for i in range(1, 11):
            hf = f0 * (i + 1)
            if hf > f_hi:
                break
            line = pg.InfiniteLine(
                pos=np.log10(hf),
                angle=90,
                pen=pg.mkPen(AMBER, width=1, style=QtCore.Qt.PenStyle.DashLine),
                label=f"H{i+1}",
                labelOpts={"color": AMBER, "position": 0.92,
                           "movable": False, "fill": None},
            )
            self._pw.addItem(line)
            self._harmonic_lines.append(line)

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
        self._readout_label.setText(f"  ► {hz:,.1f} Hz   {db:.1f} dBFS")

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
# Helpers
# ---------------------------------------------------------------------------

def _mono_font():
    from pyqtgraph.Qt import QtGui
    f = QtGui.QFont("Monospace")
    f.setStyleHint(QtGui.QFont.StyleHint.TypeWriter)
    f.setPointSize(9)
    return f
