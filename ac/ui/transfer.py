"""Transfer function view — magnitude, phase, coherence in 3 stacked panels."""
import json
import numpy as np

from pyqtgraph.Qt import QtCore, QtGui, QtWidgets
import pyqtgraph as pg

from .app import (BG, PANEL, TEXT, BLUE, ORANGE, PURPLE, RED, AMBER,
                  FreqAxis, mono_font, styled_plot, status_label, readout_label)


class _PulseOverlay(QtWidgets.QWidget):
    """Semi-transparent pulsing ring overlay drawn on top of everything."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setAttribute(QtCore.Qt.WidgetAttribute.WA_TransparentForMouseEvents)
        self.setAttribute(QtCore.Qt.WidgetAttribute.WA_TranslucentBackground)
        self.setStyleSheet("background: transparent;")
        self._phase = 0.0
        self._timer = QtCore.QTimer(self)
        self._timer.timeout.connect(self._tick)

    def start(self):
        self._phase = 0.0
        self.show()
        self.raise_()
        self._timer.start(40)

    def stop(self):
        self._timer.stop()
        self.hide()

    def _tick(self):
        self._phase += 0.04
        self.update()

    def paintEvent(self, event):
        if not self.isVisible():
            return
        p = QtGui.QPainter(self)
        p.setRenderHint(QtGui.QPainter.RenderHint.Antialiasing)

        cx = self.width() / 2
        cy = self.height() / 2
        base_r = min(cx, cy) * 0.85

        t = np.sin(self._phase)
        radius = base_r + base_r * 0.15 * t
        alpha = int(60 + 40 * t)
        width = 3.0 + 1.5 * t

        p.setPen(QtCore.Qt.PenStyle.NoPen)
        p.setBrush(QtGui.QColor(255, 180, 60, int(alpha * 0.15)))
        p.drawEllipse(QtCore.QPointF(cx, cy), radius, radius)

        # Inner dot
        dot_alpha = int(90 + 60 * t)
        p.setPen(QtCore.Qt.PenStyle.NoPen)
        p.setBrush(QtGui.QColor(255, 180, 60, dot_alpha))
        p.drawEllipse(QtCore.QPointF(cx, cy), 6, 6)

        p.end()


class TransferView(QtWidgets.QMainWindow):
    def __init__(self, host="localhost", ctrl_port=5556, level_dbfs=-10.0):
        super().__init__()
        self.setWindowTitle("Transfer Function")
        self._host = host
        self._ctrl_port = ctrl_port
        self._level_dbfs = level_dbfs
        self._result = None
        self._capturing = False
        self._capture_n = 0
        self._build_ui()
        self._start_capturing()
        self.showFullScreen()

    def _build_ui(self):
        central = QtWidgets.QWidget()
        self.setCentralWidget(central)
        layout = QtWidgets.QVBoxLayout(central)
        layout.setSpacing(4)
        layout.setContentsMargins(8, 8, 8, 8)

        self._status = status_label()
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

        # Set initial ranges so empty panels look intentional
        self._p_mag.setXRange(np.log10(20), np.log10(20000), padding=0)
        self._p_mag.setYRange(-5, 5)
        self._p_phase.setXRange(np.log10(20), np.log10(20000), padding=0)
        self._p_coh.setXRange(np.log10(20), np.log10(20000), padding=0)

        # Link X-axis zoom/pan across all panels
        self._p_phase.setXLink(self._p_mag)
        self._p_coh.setXLink(self._p_mag)

        self._readout = readout_label()
        self._readout.setText("  Press Enter to re-capture")
        layout.addWidget(self._readout)

        # Crosshair on magnitude plot
        self._vline = pg.InfiniteLine(angle=90, movable=False,
                                      pen=pg.mkPen("#ffffff", width=1, alpha=80))
        self._hline = pg.InfiniteLine(angle=0, movable=False,
                                      pen=pg.mkPen("#ffffff", width=1, alpha=80))
        self._p_mag.addItem(self._vline)
        self._p_mag.addItem(self._hline)
        self._p_mag.scene().sigMouseMoved.connect(self._on_mouse_moved)

        # Capture pulse overlay — covers entire window
        self._pulse = _PulseOverlay(central)
        self._pulse.hide()

    def resizeEvent(self, event):
        super().resizeEvent(event)
        if hasattr(self, "_pulse"):
            cw = self.centralWidget()
            if cw:
                self._pulse.setGeometry(cw.rect())

    # ------------------------------------------------------------------
    # Capture indicator
    # ------------------------------------------------------------------

    def _start_capturing(self):
        self._capturing = True
        self._pulse.start()
        self._status.setText("  Capturing…")

    def _stop_capturing(self):
        self._capturing = False
        self._pulse.stop()

    # ------------------------------------------------------------------
    # Re-capture via ZMQ REQ
    # ------------------------------------------------------------------

    def _send_transfer_cmd(self):
        if self._capturing:
            return
        self._capture_n += 1
        self._start_capturing()
        import threading

        def _do():
            try:
                import zmq
                ctx = zmq.Context()
                sock = ctx.socket(zmq.REQ)
                sock.setsockopt(zmq.LINGER, 0)
                sock.setsockopt(zmq.RCVTIMEO, 5000)
                sock.connect(f"tcp://{self._host}:{self._ctrl_port}")
                sock.send(json.dumps({
                    "cmd": "transfer",
                    "level_dbfs": self._level_dbfs,
                }).encode())
                sock.recv()  # ack
                sock.close()
                ctx.term()
            except Exception:
                pass

        threading.Thread(target=_do, daemon=True).start()

    # ------------------------------------------------------------------
    # Frame handler
    # ------------------------------------------------------------------

    def on_frame(self, topic, frame):
        if topic == "error":
            self._stop_capturing()
            self._status.setText(f"  Error: {frame.get('message', '?')}")
            return

        if topic == "data" and frame.get("type") == "transfer_result":
            self._result = frame
            self._populate(frame)

        elif topic == "done" and frame.get("cmd") == "transfer":
            self._stop_capturing()
            xr = frame.get("xruns", 0)
            xr_s = f"  !! {xr} xrun(s)" if xr else ""
            if self._result:
                coh = np.array(self._result["coherence"])
                delay = self._result.get("delay_ms", 0.0)
                n_s = f"  #{self._capture_n}" if self._capture_n > 1 else ""
                self._status.setText(
                    f"  Transfer complete.{xr_s}{n_s}  "
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
        elif key in (QtCore.Qt.Key.Key_Return, QtCore.Qt.Key.Key_Enter):
            self._send_transfer_cmd()
        elif key == QtCore.Qt.Key.Key_F11:
            if self.isFullScreen():
                self.showNormal()
            else:
                self.showFullScreen()
        else:
            super().keyPressEvent(event)
