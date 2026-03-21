# ui/ — pyqtgraph live measurement views

## Architecture

Separate process spawned by `ac.py` via `python -m ac.ui --mode <mode> --host <host> --port <port>`. Subscribes to the ZMQ PUB socket and renders live data.

Entry point: `app.py:main()` (also called from `__main__.py`).

## File responsibilities

| File | Purpose |
|------|---------|
| `app.py` | `main()`, dark palette, `ZmqReceiver` thread, view dispatch |
| `spectrum.py` | `SpectrumView` — live FFT with peak hold and harmonic markers |
| `thd.py` | `ThdView` — rolling THD%/THD+N% strip chart |
| `sweep.py` | `SweepView` — 2×2 sweep grid with point-click spectrum detail |

## Color palette (defined in `app.py`)

```python
BG="#0e1117"  PANEL="#161b22"  GRID="#222222"  TEXT="#aaaaaa"
BLUE="#4a9eff"  ORANGE="#e67e22"  PURPLE="#a29bfe"
RED="#e74c3c"   AMBER="#ffb43c"   GREEN="#2ecc71"  YELLOW="#f1c40f"
```

## ZmqReceiver thread

`ZmqReceiver.create()` returns a `QThread` subclass with a `frame_received(str, object)` signal. It subscribes to all topics, polls with 100 ms timeout, decodes `b"<topic> <json>"` frames, and emits them to the view's `on_frame()` slot.

## View specs

- **SpectrumView**: log-x FFT, smoothing mirrors `tui.py` (`_FALL_ALPHA=0.20`, `_PEAK_HOLD=6`, `_PEAK_DECAY=1.5`), harmonic `InfiniteLine` markers, mouse crosshair readout
- **ThdView**: 120-point rolling buffer, Y-range hysteresis (instant expand, slow contract), clip indicator label
- **SweepView**: 2×2 `GraphicsLayoutWidget`, log-x for freq sweep, clipped points shown as red X scatter, click-to-inspect spectrum in bottom-right panel

## Dependencies

`pyqtgraph>=0.13`, `zmq`, `numpy`
