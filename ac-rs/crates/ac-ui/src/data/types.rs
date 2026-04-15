use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct SpectrumFrame {
    pub freqs: Vec<f32>,
    pub spectrum: Vec<f32>,
    pub freq_hz: f32,
    pub fundamental_dbfs: f32,
    pub thd_pct: f32,
    pub thdn_pct: f32,
    pub in_dbu: Option<f32>,
    pub sr: u32,
    pub clipping: bool,
    pub xruns: u32,
    #[serde(default)]
    pub channel: Option<u32>,
    #[serde(default)]
    #[allow(dead_code)]
    pub n_channels: Option<u32>,
    /// Set by the producer (receiver / synthetic), monotonically increasing per
    /// channel. Lets the consumer detect fresh frames vs. re-reads of the same
    /// data so the waterfall view can advance one row per real new sample. Zero
    /// means "no frame yet"; producers always emit ≥ 1.
    #[serde(default, skip)]
    pub frame_id: u64,
}

impl Default for SpectrumFrame {
    fn default() -> Self {
        Self {
            freqs: Vec::new(),
            spectrum: Vec::new(),
            freq_hz: 0.0,
            fundamental_dbfs: -140.0,
            thd_pct: 0.0,
            thdn_pct: 0.0,
            in_dbu: None,
            sr: 48000,
            clipping: false,
            xruns: 0,
            channel: None,
            n_channels: None,
            frame_id: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DisplayFrame {
    pub spectrum: Vec<f32>,
    #[allow(dead_code)]
    pub peak_hold: Vec<f32>,
    pub freqs: Vec<f32>,
    pub meta: FrameMeta,
    /// Populated by the store on the first read after a fresh producer frame.
    /// `None` on re-reads of the same frame so the waterfall renderer scrolls
    /// at the rate of incoming data, not at the rate of redraws.
    pub new_row: Option<Vec<f32>>,
}

#[derive(Debug, Clone)]
pub struct FrameMeta {
    pub freq_hz: f32,
    pub fundamental_dbfs: f32,
    pub thd_pct: f32,
    pub thdn_pct: f32,
    pub in_dbu: Option<f32>,
    pub sr: u32,
    pub clipping: bool,
    #[allow(dead_code)]
    pub xruns: u32,
}

impl From<&SpectrumFrame> for FrameMeta {
    fn from(f: &SpectrumFrame) -> Self {
        Self {
            freq_hz: f.freq_hz,
            fundamental_dbfs: f.fundamental_dbfs,
            thd_pct: f.thd_pct,
            thdn_pct: f.thdn_pct,
            in_dbu: f.in_dbu,
            sr: f.sr,
            clipping: f.clipping,
            xruns: f.xruns,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Grid,
    Overlay,
    Single,
    /// Stacks only the user-selected channels in one rect (overlay-style)
    /// with a corner legend. Hidden until the user toggles selections via
    /// Space — the empty case shows a "press Space to select" hint.
    Compare,
}

impl LayoutMode {
    pub fn next(self) -> Self {
        match self {
            LayoutMode::Grid => LayoutMode::Overlay,
            LayoutMode::Overlay => LayoutMode::Single,
            LayoutMode::Single => LayoutMode::Compare,
            LayoutMode::Compare => LayoutMode::Grid,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Spectrum,
    Waterfall,
}

impl ViewMode {
    pub fn next(self) -> Self {
        match self {
            ViewMode::Spectrum => ViewMode::Waterfall,
            ViewMode::Waterfall => ViewMode::Spectrum,
        }
    }
}

/// Per-cell zoom/pan state. Split out of `DisplayConfig` so mouse interactions
/// can target the hovered cell independently without broadcasting to the rest.
#[derive(Debug, Clone, Copy)]
pub struct CellView {
    pub freq_min: f32,
    pub freq_max: f32,
    pub db_min:   f32,
    pub db_max:   f32,
    /// Waterfall-only: how many history rows from the newest one are stretched
    /// across the cell height. `ROWS_PER_CHANNEL` = show the whole ring (full
    /// time depth); smaller = zoom into the recent past. Ignored in Spectrum.
    pub rows_visible: u32,
}

impl Default for CellView {
    fn default() -> Self {
        Self {
            freq_min: crate::theme::DEFAULT_FREQ_MIN,
            freq_max: crate::theme::DEFAULT_FREQ_MAX,
            db_min:   crate::theme::DEFAULT_DB_MIN,
            db_max:   crate::theme::DEFAULT_DB_MAX,
            rows_visible: crate::render::waterfall::ROWS_PER_CHANNEL,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DisplayConfig {
    pub peak_hold: bool,
    pub averaging_alpha: f32,
    pub frozen: bool,
    pub layout: LayoutMode,
    pub view_mode: ViewMode,
    pub active_channel: usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            peak_hold: false,
            averaging_alpha: 0.20,
            frozen: false,
            layout: LayoutMode::Grid,
            view_mode: ViewMode::Spectrum,
            active_channel: 0,
        }
    }
}
