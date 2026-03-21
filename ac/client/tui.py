"""Terminal spectrum analyzer — no extra dependencies."""
import shutil
import numpy as np


class SpectrumRenderer:
    BLOCKS     = " ▁▂▃▄▅▆▇█"
    _PEAK_CHAR = "▄"

    # Color stops: (dBFS threshold, R, G, B)
    _COLOR_STOPS = [
        (-100, 0x1a, 0x3a, 0x5c),
        ( -60, 0x00, 0xaa, 0xcc),
        ( -30, 0x00, 0xcc, 0x66),
        ( -10, 0xff, 0xcc, 0x00),
        (   0, 0xe7, 0x4c, 0x3c),
    ]

    # Fast attack, slow decay: each frame pulls smoothed value this far toward raw
    _FALL_ALPHA  = 0.20   # 0 = freeze, 1 = no smoothing
    # Peak hold
    _PEAK_HOLD   = 6      # frames before peak starts falling
    _PEAK_DECAY  = 1.5    # dB per frame after hold expires

    def __init__(self, db_min=-100, db_max=0, start_freq=20.0, end_freq=24000.0):
        self.db_min     = db_min
        self.db_max     = db_max
        self.f_lo       = start_freq
        self.f_hi       = end_freq
        self._smooth_db = None   # (N,) smoothed bar levels, dB
        self._peak_db   = None   # (N,) peak hold levels, dB
        self._peak_age  = None   # (N,) frames since each peak was set

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def render(self, freqs, spectrum_linear, thd_pct, thdn_pct,
               in_dbu, fundamental_hz, harmonic_freqs, sr=48000) -> str:
        cols, rows = shutil.get_terminal_size((80, 24))

        left_margin = 6
        bar_cols    = max(cols - left_margin - 1, 4)
        bar_rows    = max(rows - 3, 2)

        f_lo         = self.f_lo
        f_hi         = min(sr / 2, self.f_hi)
        raw_lin      = self._log_bin(freqs, spectrum_linear, bar_cols, f_lo=f_lo, f_hi=f_hi)
        raw_db       = 20.0 * np.log10(np.maximum(raw_lin, 1e-12))
        harmonic_set = self._harmonic_columns(harmonic_freqs, bar_cols, f_lo=f_lo, f_hi=f_hi)

        smooth_db, peak_db = self._update_state(raw_db, bar_cols)

        status = self._status_line(thd_pct, thdn_pct, in_dbu, fundamental_hz, cols)
        bars   = self._bar_block(smooth_db, peak_db, bar_cols, bar_rows, harmonic_set)
        freq_l = self._freq_labels(bar_cols, left_margin, f_hi, f_lo=f_lo)

        lines = [status, ""]
        lines += bars
        lines.append(freq_l)
        return "\033[0m" + "\n".join(lines) + "\033[0m"

    # ------------------------------------------------------------------
    # State: fast attack / slow decay + peak hold
    # ------------------------------------------------------------------

    def _update_state(self, raw_db, n):
        # Reinitialize on first call or after terminal resize
        if self._smooth_db is None or len(self._smooth_db) != n:
            self._smooth_db = raw_db.copy()
            self._peak_db   = raw_db.copy()
            self._peak_age  = np.zeros(n, dtype=int)
            return self._smooth_db.copy(), self._peak_db.copy()

        # Bars: instant rise, exponential fall toward raw value
        rise = raw_db >= self._smooth_db
        self._smooth_db[rise]  = raw_db[rise]
        self._smooth_db[~rise] = (self._smooth_db[~rise] * (1.0 - self._FALL_ALPHA)
                                  + raw_db[~rise] * self._FALL_ALPHA)

        # Peak hold: reset on new peak, age otherwise
        new_peak = raw_db >= self._peak_db
        self._peak_db[new_peak]  = raw_db[new_peak]
        self._peak_age[new_peak] = 0
        self._peak_age[~new_peak] += 1

        # After hold period, peak falls at _PEAK_DECAY dB/frame
        falling = self._peak_age > self._PEAK_HOLD
        self._peak_db[falling] -= self._PEAK_DECAY
        np.clip(self._peak_db, self.db_min - 1, self.db_max, out=self._peak_db)

        return self._smooth_db.copy(), self._peak_db.copy()

    # ------------------------------------------------------------------
    # Internals
    # ------------------------------------------------------------------

    def _log_bin(self, freqs, spectrum_linear, n_bars, f_lo=20.0, f_hi=24000.0):
        """Max linear amplitude per log-spaced bar column.

        When FFT resolution is coarse (low frequencies, short captures), many
        columns contain no bins and would render as silent gaps.  Fill those
        columns with the nearest populated column's value so the display looks
        continuous rather than spikey.
        """
        if len(freqs) == 0:
            return np.full(n_bars, 1e-12)
        edges    = f_lo * (f_hi / f_lo) ** (np.arange(n_bars + 1) / n_bars)
        result   = np.full(n_bars, 1e-12)
        has_data = np.zeros(n_bars, dtype=bool)
        for c in range(n_bars):
            mask = (freqs >= edges[c]) & (freqs < edges[c + 1])
            if mask.any():
                result[c]   = spectrum_linear[mask].max()
                has_data[c] = True
        # Fill empty columns via nearest-neighbour from populated columns
        if has_data.any() and not has_data.all():
            idxs    = np.arange(n_bars)
            filled  = idxs[has_data]
            # For each empty column, index of closest populated column
            nearest = filled[np.argmin(np.abs(filled[:, None] - idxs[~has_data]), axis=0)]
            result[~has_data] = result[nearest]
        return result

    def _harmonic_columns(self, harmonic_freqs, n_bars, f_lo=20.0, f_hi=24000.0):
        """Set of bar column indices that are close to a harmonic frequency."""
        cols = set()
        for hf in harmonic_freqs:
            if hf < f_lo or hf > f_hi:
                continue
            c = int(np.log(hf / f_lo) / np.log(f_hi / f_lo) * n_bars)
            for dc in (-1, 0, 1):
                cc = c + dc
                if 0 <= cc < n_bars:
                    cols.add(cc)
        return cols

    def _amplitude_color(self, db: float) -> str:
        stops = self._COLOR_STOPS
        if db <= stops[0][0]:
            r, g, b = stops[0][1], stops[0][2], stops[0][3]
        elif db >= stops[-1][0]:
            r, g, b = stops[-1][1], stops[-1][2], stops[-1][3]
        else:
            for i in range(len(stops) - 1):
                d0, r0, g0, b0 = stops[i]
                d1, r1, g1, b1 = stops[i + 1]
                if d0 <= db <= d1:
                    t  = (db - d0) / (d1 - d0)
                    r  = int(r0 + t * (r1 - r0))
                    g  = int(g0 + t * (g1 - g0))
                    b  = int(b0 + t * (b1 - b0))
                    break
            else:
                r, g, b = 0xaa, 0xaa, 0xaa
        return f"\033[38;2;{r};{g};{b}m"

    _AMBER      = "\033[38;2;255;180;60m"
    _PEAK_COLOR = "\033[1;37m"    # bright white peak markers
    _RST        = "\033[0m"

    def _bar_block(self, smooth_db, peak_db, n_bars, n_rows, harmonic_set):
        """Build list of text lines (top → bottom) for the bar chart."""
        db_range    = self.db_max - self.db_min
        cells_total = n_rows * 8

        heights = np.clip(
            (smooth_db - self.db_min) / db_range * cells_total,
            0, cells_total
        ).astype(int)

        # Peak row index: row 0 = top = db_max
        peak_rows = np.clip(
            np.floor((self.db_max - peak_db) / db_range * n_rows).astype(int),
            0, n_rows - 1
        )

        # dB tick labels
        n_ticks  = min(6, n_rows)
        tick_dbs = [self.db_min + i * db_range / (n_ticks - 1)
                    for i in range(n_ticks)]
        def db_to_row(db):
            return int((self.db_max - db) / db_range * n_rows)
        tick_rows = {db_to_row(d): f"{d:+.0f}" for d in tick_dbs}

        BLOCKS = self.BLOCKS
        lines  = []
        for row in range(n_rows):
            db_at_mid = self.db_max - (row + 0.5) * db_range / n_rows
            margin    = f"{tick_rows.get(row, ''):>5} "

            row_chars = []
            for c in range(n_bars):
                h                  = heights[c]
                row_bottom_subcell = (n_rows - row - 1) * 8
                row_top_subcell    = row_bottom_subcell + 8
                is_harmonic        = c in harmonic_set
                is_peak_row        = peak_rows[c] == row

                if h >= row_top_subcell:
                    # Bar fully fills this row
                    ch    = BLOCKS[8]
                    color = self._AMBER if is_harmonic else self._amplitude_color(db_at_mid)
                elif h > row_bottom_subcell:
                    # Partial bar (fractional top cell)
                    ch    = BLOCKS[h - row_bottom_subcell]
                    color = self._AMBER if is_harmonic else self._amplitude_color(db_at_mid)
                elif is_peak_row and peak_db[c] > self.db_min:
                    # Peak marker floating above bar
                    ch    = self._PEAK_CHAR
                    color = self._AMBER if is_harmonic else self._PEAK_COLOR
                else:
                    ch    = " "
                    color = ""

                row_chars.append(color + ch + self._RST if color else ch)

            lines.append(margin + "".join(row_chars))

        return lines

    def _freq_labels(self, n_bars, left_margin, f_hi=24000.0, f_lo=20.0):
        """Bottom frequency-axis label row."""
        labels = [
            (20,    "20"),
            (50,    "50"),
            (100,   "100"),
            (200,   "200"),
            (500,   "500"),
            (1000,  "1k"),
            (2000,  "2k"),
            (5000,  "5k"),
            (10000, "10k"),
            (20000, "20k"),
        ]
        row = [" "] * n_bars
        for freq, text in labels:
            if freq > f_hi:
                continue
            c = int(np.log(freq / f_lo) / np.log(f_hi / f_lo) * n_bars)
            c = max(0, min(n_bars - len(text), c))
            for i, ch in enumerate(text):
                if c + i < n_bars:
                    row[c + i] = ch
        return " " * left_margin + "".join(row)

    def _status_line(self, thd_pct, thdn_pct, in_dbu, fundamental_hz, width):
        freq_s = f"{fundamental_hz:.0f} Hz" if fundamental_hz else ""
        dbu_s  = f"  |  {in_dbu:+.2f} dBu"   if in_dbu  is not None else ""
        thd_s  = f"  |  THD: {thd_pct:.4f}%"  if thd_pct  is not None else ""
        thdn_s = f"  |  THD+N: {thdn_pct:.4f}%" if thdn_pct is not None else ""
        line   = f"  {freq_s}{dbu_s}{thd_s}{thdn_s}"
        return f"\033[1;37m{line:<{width}}\033[0m"
