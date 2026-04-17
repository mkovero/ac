"""ac.ui — real-time measurement GUI entry point.

Usage:
    python -m ac.ui --mode spectrum --host localhost --port 5557
    python -m ac.ui --mode sweep_frequency --host localhost --port 5557
    python -m ac.ui --mode sweep_level     --host localhost --port 5557
"""
import argparse
import json
import sys

# ---------------------------------------------------------------------------
# Color palette
# ---------------------------------------------------------------------------
BG      = "#0e1117"
PANEL   = "#161b22"
TEXT    = "#aaaaaa"
BLUE    = "#4a9eff"
ORANGE  = "#e67e22"
PURPLE  = "#a29bfe"
RED     = "#e74c3c"
AMBER   = "#ffb43c"

_SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"


def _hz_label(val):
    """Format a frequency in Hz as a human-readable string."""
    if val >= 1000:
        v = val / 1000
        return f"{v:.0f}k" if v == int(v) else f"{v:.1f}k"
    return f"{val:.0f}"


def FreqAxis(orientation="bottom"):
    """Factory: returns a pyqtgraph AxisItem with human-readable Hz ticks."""
    import pyqtgraph as pg
    import numpy as np

    _MAJOR = [20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000]

    class _FreqAxis(pg.AxisItem):
        def tickStrings(self, values, scale, spacing):
            return [_hz_label(10 ** v) for v in values]

        def tickValues(self, minVal, maxVal, size):
            major = [np.log10(f) for f in _MAJOR
                     if minVal <= np.log10(f) <= maxVal]
            return [(0, major)]

    return _FreqAxis(orientation=orientation)


def mono_font(size=9):
    """Shared monospace font for axis labels."""
    from pyqtgraph.Qt import QtGui
    f = QtGui.QFont("Monospace")
    f.setStyleHint(QtGui.QFont.StyleHint.TypeWriter)
    f.setPointSize(size)
    return f


def styled_plot(glw, title, ylabel, log_freq=False):
    """Create a styled PlotItem with consistent look across all views."""
    import pyqtgraph as pg
    axisItems = {}
    if log_freq:
        axisItems["bottom"] = FreqAxis(orientation="bottom")
    p = glw.addPlot(title=title, axisItems=axisItems)
    p.setLabel("left", ylabel, color=TEXT)
    p.showGrid(x=True, y=True, alpha=0.3)
    p.getAxis("left").setStyle(tickFont=mono_font())
    p.getAxis("bottom").setStyle(tickFont=mono_font())
    if log_freq:
        p.getAxis("bottom").setLabel("Frequency (Hz)", color=TEXT)
    return p


def add_harmonic_markers(plot, f0, sr, max_harmonics=10):
    """Add amber dashed vertical lines at harmonic frequencies. Returns list of items."""
    import pyqtgraph as pg
    from pyqtgraph.Qt import QtCore
    import numpy as np
    lines = []
    if not f0:
        return lines
    f_hi = min(sr / 2, 24000)
    for i in range(1, max_harmonics + 1):
        hf = f0 * (i + 1)
        if hf > f_hi:
            break
        ln = pg.InfiniteLine(
            pos=np.log10(hf), angle=90,
            pen=pg.mkPen(AMBER, width=1, style=QtCore.Qt.PenStyle.DashLine),
            label=f"H{i+1}",
            labelOpts={"color": AMBER, "position": 0.90,
                        "movable": False, "fill": None},
        )
        plot.addItem(ln)
        lines.append(ln)
    return lines


def status_label():
    """Create a styled status label widget."""
    from pyqtgraph.Qt import QtWidgets
    lbl = QtWidgets.QLabel("")
    lbl.setStyleSheet(
        f"color: white; background: {PANEL}; padding: 4px 8px; "
        "font-family: monospace; font-size: 13px;"
    )
    return lbl


def readout_label():
    """Create a styled readout label widget."""
    from pyqtgraph.Qt import QtWidgets
    lbl = QtWidgets.QLabel("")
    lbl.setStyleSheet(
        f"color: {TEXT}; background: {PANEL}; padding: 3px 8px; "
        "font-family: monospace; font-size: 12px;"
    )
    return lbl


def _build_dark_palette(app):
    from pyqtgraph.Qt import QtGui
    pal = QtGui.QPalette()
    def _c(hex_):
        return QtGui.QColor(hex_)
    pal.setColor(QtGui.QPalette.ColorRole.Window,          _c(BG))
    pal.setColor(QtGui.QPalette.ColorRole.WindowText,      _c(TEXT))
    pal.setColor(QtGui.QPalette.ColorRole.Base,            _c(PANEL))
    pal.setColor(QtGui.QPalette.ColorRole.AlternateBase,   _c(BG))
    pal.setColor(QtGui.QPalette.ColorRole.Text,            _c(TEXT))
    pal.setColor(QtGui.QPalette.ColorRole.Button,          _c(PANEL))
    pal.setColor(QtGui.QPalette.ColorRole.ButtonText,      _c(TEXT))
    pal.setColor(QtGui.QPalette.ColorRole.Highlight,       _c(BLUE))
    pal.setColor(QtGui.QPalette.ColorRole.HighlightedText, _c(BG))
    app.setPalette(pal)


# ---------------------------------------------------------------------------
# ZMQ receiver thread
# ---------------------------------------------------------------------------

class ZmqReceiver:
    """QThread that subscribes to ZMQ PUB and emits (topic, frame) signals."""

    def __init__(self, host, port):
        from pyqtgraph.Qt import QtCore
        super_class = QtCore.QThread

        class _Receiver(super_class):
            frame_received = QtCore.Signal(str, object)

            def __init__(self_, host, port):
                super().__init__()
                self_._host = host
                self_._port = port
                self_._running = True

            def stop(self_):
                self_._running = False

            def run(self_):
                try:
                    import zmq
                except ImportError:
                    return
                ctx = zmq.Context()
                sub = ctx.socket(zmq.SUB)
                sub.setsockopt(zmq.SUBSCRIBE, b"")
                sub.setsockopt(zmq.LINGER, 0)
                sub.connect(f"tcp://{self_._host}:{self_._port}")
                while self_._running:
                    if sub.poll(100):
                        try:
                            msg   = sub.recv()
                            space = msg.index(b" ")
                            topic = msg[:space].decode()
                            frame = json.loads(msg[space + 1:])
                            self_.frame_received.emit(topic, frame)
                        except Exception:
                            pass
                sub.close()
                ctx.term()

        self._cls  = _Receiver
        self._host = host
        self._port = port

    def create(self):
        return self._cls(self._host, self._port)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(prog="ac.ui")
    ap.add_argument("--mode",  required=True,
                    choices=["spectrum", "sweep_frequency", "sweep_level", "transfer"])
    ap.add_argument("--host",  default="localhost")
    ap.add_argument("--port",  type=int, default=5557)
    ap.add_argument("--session-dir", default=None)
    ap.add_argument("--ctrl-port", type=int, default=5556)
    ap.add_argument("--level-dbfs", type=float, default=-10.0)
    args = ap.parse_args()

    try:
        import pyqtgraph as pg
    except ImportError:
        print("pyqtgraph not installed — run: pip install pyqtgraph", file=sys.stderr)
        sys.exit(1)

    app = pg.mkQApp("ac measurement")
    _build_dark_palette(app)
    pg.setConfigOptions(antialias=True, background=BG, foreground=TEXT)

    # Build receiver
    factory  = ZmqReceiver(args.host, args.port)
    receiver = factory.create()

    # Build view
    if args.mode == "spectrum":
        from .spectrum import SpectrumView
        view = SpectrumView(session_dir=args.session_dir)
    elif args.mode == "transfer":
        from .transfer import TransferView
        view = TransferView(host=args.host, ctrl_port=args.ctrl_port,
                            level_dbfs=args.level_dbfs,
                            session_dir=args.session_dir)
    else:
        from .sweep import SweepView
        view = SweepView(mode=args.mode, session_dir=args.session_dir)

    receiver.frame_received.connect(view.on_frame)
    receiver.start()

    view.show()
    exit_code = app.exec()

    receiver.stop()
    receiver.wait(1000)
    sys.exit(exit_code)


if __name__ == "__main__":
    main()
