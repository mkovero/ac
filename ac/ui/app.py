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
# Color palette (mirrors plotting.py)
# ---------------------------------------------------------------------------
BG      = "#0e1117"
PANEL   = "#161b22"
GRID    = "#222222"
TEXT    = "#aaaaaa"
BLUE    = "#4a9eff"
ORANGE  = "#e67e22"
PURPLE  = "#a29bfe"
RED     = "#e74c3c"
AMBER   = "#ffb43c"
GREEN   = "#2ecc71"
YELLOW  = "#f1c40f"


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
                    choices=["spectrum", "thd", "sweep_frequency", "sweep_level"])
    ap.add_argument("--host",  default="localhost")
    ap.add_argument("--port",  type=int, default=5557)
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
        view = SpectrumView()
    else:
        from .sweep import SweepView
        view = SweepView(mode=args.mode)

    receiver.frame_received.connect(view.on_frame)
    receiver.start()

    view.show()
    exit_code = app.exec()

    receiver.stop()
    receiver.wait(1000)
    sys.exit(exit_code)


if __name__ == "__main__":
    main()
