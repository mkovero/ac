"""Transfer function view — magnitude, phase, coherence in 3 stacked panels."""
import numpy as np

from pyqtgraph.Qt import QtCore, QtWidgets
import pyqtgraph as pg

from .app import (BG, PANEL, TEXT, BLUE, ORANGE, PURPLE, RED,
                  FreqAxis, mono_font, styled_plot, status_label, readout_label)


class TransferView(QtWidgets.QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("Transfer Function")
        self._result = None
        self._build_ui()
        self.showFullScreen()

    def _build_ui(self):
        central = QtWidgets.QWidget()
        self.setCentralWidget(central)
        layout = QtWidgets.QVBoxLayout(central)
        layout.setSpacing(4)
        layout.setContentsMargins(8, 8, 8, 8)

        self._status = status_label()
        self._status.setText("  Capturing… (this takes several seconds)")
        layout.addWidget(self._status)

        glw = pg.GraphicsLayoutWidget()
        glw.setBackground(PANEL)
        layout.addWidget(glw, stretch=1)

        # Magnitude
        self._p_mag = styled_plot(glw, "Magnitude Response", "Magnitude (dB)",
                                  log_freq=True)
        self._mag_line = self._p_mag.plot(pen=pg.mkPen(BLUE, width=1.5))
        self._mag_ref = pg.InfiniteLine(
            pos=0, angle=0,
            pen=pg.mkPen("#444444", width=1, style=QtCore.Qt.PenStyle.DashLine))
        self._p_mag.addItem(self._mag_ref)

        glw.nextRow()

        # Phase
        self._p_phase = styled_plot(glw, "Phase Response", "Phase (\u00b0)",
                                    log_freq=True)
        self._phase_line = self._p_phase.plot(pen=pg.mkPen(PURPLE, width=1.5))
        self._phase_ref = pg.InfiniteLine(
            pos=0, angle=0,
            pen=pg.mkPen("#444444", width=1, style=QtCore.Qt.PenStyle.DashLine))
        self._p_phase.addItem(self._phase_ref)
        self._p_phase.setYRange(-200, 200)
        self._p_phase.getAxis("left").setTicks([
            [(-180, "-180\u00b0"), (-90, "-90\u00b0"), (0, "0\u00b0"),
             (90, "90\u00b0"), (180, "180\u00b0")]
        ])

        glw.nextRow()

        # Coherence
        self._p_coh = styled_plot(glw, "Coherence", "Coherence (\u03b3\u00b2)",
                                  log_freq=True)
        self._coh_line = self._p_coh.plot(pen=pg.mkPen(ORANGE, width=1.5))
        self._coh_ref = pg.InfiniteLine(
            pos=0.95, angle=0,
            pen=pg.mkPen(RED, width=1, style=QtCore.Qt.PenStyle.DashLine))
        self._p_coh.addItem(self._coh_ref)
        self._p_coh.setYRange(-0.05, 1.05)

        self._readout = readout_label()
        layout.addWidget(self._readout)

        # Crosshair on magnitude plot
        self._vline = pg.InfiniteLine(angle=90, movable=False,
                                      pen=pg.mkPen("#ffffff", width=1, alpha=80))
        self._hline = pg.InfiniteLine(angle=0, movable=False,
                                      pen=pg.mkPen("#ffffff", width=1, alpha=80))
        self._p_mag.addItem(self._vline)
        self._p_mag.addItem(self._hline)
        self._p_mag.scene().sigMouseMoved.connect(self._on_mouse_moved)

    def on_frame(self, topic, frame):
        if topic == "error":
            self._status.setText(f"  Error: {frame.get('message', '?')}")
            return

        if topic == "data" and frame.get("type") == "transfer_result":
            self._result = frame
            self._populate(frame)

        elif topic == "done" and frame.get("cmd") == "transfer":
            xr = frame.get("xruns", 0)
            xr_s = f"  !! {xr} xrun(s)" if xr else ""
            if self._result:
                coh = np.array(self._result["coherence"])
                delay = self._result.get("delay_ms", 0.0)
                self._status.setText(
                    f"  Transfer complete.{xr_s}  "
                    f"Delay: {delay:.3f} ms  |  "
                    f"Coherence: mean {np.mean(coh):.3f}  min {np.min(coh):.3f}")
            else:
                self._status.setText(f"  Transfer complete.{xr_s}")

    def _populate(self, result):
        freqs = np.array(result["freqs"], dtype=float)
        mag   = np.array(result["magnitude_db"], dtype=float)
        phase = np.array(result["phase_deg"], dtype=float)
        coh   = np.array(result["coherence"], dtype=float)

        # Skip DC bin
        mask = freqs > 0
        freqs = freqs[mask]
        mag   = mag[mask]
        phase = phase[mask]
        coh   = coh[mask]

        log_f = np.log10(freqs)

        self._mag_line.setData(log_f, mag)
        self._phase_line.setData(log_f, phase)
        self._coh_line.setData(log_f, coh)

        # Center magnitude around 0 dB
        mag_max = max(abs(np.nanmin(mag)), abs(np.nanmax(mag)), 1.0)
        mag_pad = min(mag_max * 1.1, mag_max + 3)
        self._p_mag.setYRange(-mag_pad, mag_pad)

        # Store for crosshair readout
        self._log_f = log_f
        self._mag = mag
        self._phase = phase
        self._coh = coh

        delay = result.get("delay_ms", 0.0)
        self._status.setText(
            f"  Delay: {delay:.3f} ms  |  "
            f"Coherence: mean {np.mean(coh):.3f}  min {np.min(coh):.3f}")

    def _on_mouse_moved(self, pos):
        if not self._p_mag.sceneBoundingRect().contains(pos):
            return
        mp = self._p_mag.getViewBox().mapSceneToView(pos)
        lf = mp.x()
        db = mp.y()
        self._vline.setPos(lf)
        self._hline.setPos(db)

        hz = 10 ** lf
        from .app import _hz_label
        self._readout.setText(
            f"  \u25b6 {_hz_label(hz)} Hz   {db:+.2f} dB")

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
