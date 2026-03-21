"""THD strip-chart view — rolling THD% and THD+N% over time."""
import numpy as np

from pyqtgraph.Qt import QtCore, QtWidgets
import pyqtgraph as pg

from .app import BG, PANEL, GRID, TEXT, BLUE, ORANGE, RED, GREEN, YELLOW, AMBER


_WINDOW = 120   # rolling buffer length (points)


class ThdView(QtWidgets.QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("THD Monitor")
        self.resize(900, 500)

        self._times  = np.full(_WINDOW, np.nan)
        self._thd    = np.full(_WINDOW, np.nan)
        self._thdn   = np.full(_WINDOW, np.nan)
        self._t0     = None
        self._idx    = 0
        self._count  = 0

        # Y-range hysteresis
        self._y_max  = 0.0

        self._build_ui()

    # ------------------------------------------------------------------

    def _build_ui(self):
        central = QtWidgets.QWidget()
        self.setCentralWidget(central)
        layout = QtWidgets.QVBoxLayout(central)
        layout.setSpacing(4)
        layout.setContentsMargins(8, 8, 8, 8)

        # Status
        self._status_label = QtWidgets.QLabel("  Waiting for data…")
        self._status_label.setStyleSheet(
            f"color: white; background: {PANEL}; padding: 4px 8px; "
            "font-family: monospace; font-size: 13px;"
        )
        layout.addWidget(self._status_label)

        # Plot
        self._pw = pg.PlotWidget()
        self._pw.setBackground(PANEL)
        self._pw.setLabel("left",   "Distortion (%)", color=TEXT)
        self._pw.setLabel("bottom", "Time (s)",        color=TEXT)
        self._pw.showGrid(x=True, y=True, alpha=0.3)
        self._pw.getAxis("left").setStyle(tickFont=_mono_font())
        self._pw.getAxis("bottom").setStyle(tickFont=_mono_font())
        layout.addWidget(self._pw, stretch=1)

        self._thd_curve  = self._pw.plot(pen=pg.mkPen(BLUE,   width=2), name="THD%")
        self._thdn_curve = self._pw.plot(pen=pg.mkPen(ORANGE, width=2), name="THD+N%")

        legend = self._pw.addLegend(offset=(10, 10))
        legend.addItem(self._thd_curve,  "THD%")
        legend.addItem(self._thdn_curve, "THD+N%")

        # CLIP indicator (hidden until needed)
        self._clip_label = QtWidgets.QLabel("  !! CLIP !!")
        self._clip_label.setStyleSheet(
            f"color: {RED}; background: {PANEL}; padding: 3px 8px; "
            "font-family: monospace; font-weight: bold; font-size: 13px;"
        )
        self._clip_label.hide()
        layout.addWidget(self._clip_label)

        # Readout bar
        self._readout_label = QtWidgets.QLabel("")
        self._readout_label.setStyleSheet(
            f"color: {TEXT}; background: {PANEL}; padding: 3px 8px; "
            "font-family: monospace; font-size: 12px;"
        )
        layout.addWidget(self._readout_label)

    # ------------------------------------------------------------------

    def on_frame(self, topic, frame):
        if topic == "error":
            self._status_label.setText(f"  Error: {frame.get('message','?')}")
            return
        if topic != "data" or frame.get("type") != "thd_point":
            return

        thd    = frame.get("thd_pct",  0.0)
        thdn   = frame.get("thdn_pct", 0.0)
        in_dbu = frame.get("in_dbu")
        out_dbu= frame.get("out_dbu")
        gain   = frame.get("gain_db")
        clipping = frame.get("clipping", False)
        freq   = frame.get("fundamental_hz")

        # Timestamp
        import time
        now = time.monotonic()
        if self._t0 is None:
            self._t0 = now
        t = now - self._t0

        # Rolling buffer
        i = self._idx % _WINDOW
        self._times[i] = t
        self._thd[i]   = thd
        self._thdn[i]  = thdn
        self._idx += 1
        self._count += 1

        # Build ordered view
        if self._count <= _WINDOW:
            tv = self._times[:self._count]
            av = self._thd[:self._count]
            bv = self._thdn[:self._count]
        else:
            pos = self._idx % _WINDOW
            tv = np.concatenate([self._times[pos:], self._times[:pos]])
            av = np.concatenate([self._thd[pos:],   self._thd[:pos]])
            bv = np.concatenate([self._thdn[pos:],  self._thdn[:pos]])

        self._thd_curve.setData(tv, av)
        self._thdn_curve.setData(tv, bv)

        # Y-range: expand instantly, contract slowly (hysteresis)
        cur_max = max(np.nanmax(av), np.nanmax(bv)) if self._count > 0 else 0.0
        if cur_max > self._y_max:
            self._y_max = cur_max
        else:
            self._y_max = self._y_max * 0.99 + cur_max * 0.01
        self._pw.setYRange(0, max(self._y_max * 1.15, 0.01), padding=0)

        # Clip indicator
        if clipping:
            self._clip_label.show()
        else:
            self._clip_label.hide()

        # THD color coding
        if thd < 0.01:   col = GREEN
        elif thd < 0.1:  col = YELLOW
        else:             col = RED

        freq_s = f"{freq:.0f} Hz" if freq else ""
        dbu_s  = f"  │  {in_dbu:+.2f} dBu in" if in_dbu is not None else ""
        gain_s = f"  │  gain {gain:+.2f} dB"   if gain   is not None else ""
        self._status_label.setText(f"  {freq_s}{dbu_s}{gain_s}")

        min_thd = float(np.nanmin(av)) if self._count > 0 else thd
        self._readout_label.setText(
            f"  <span style='color:{col}'>THD: {thd:.4f}%</span>"
            f"   │   THD+N: {thdn:.4f}%"
            f"   │   min: {min_thd:.4f}%"
        )
        self._readout_label.setTextFormat(QtCore.Qt.TextFormat.RichText)


def _mono_font():
    from pyqtgraph.Qt import QtGui
    f = QtGui.QFont("Monospace")
    f.setStyleHint(QtGui.QFont.StyleHint.TypeWriter)
    f.setPointSize(9)
    return f
