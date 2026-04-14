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
}

impl LayoutMode {
    pub fn next(self) -> Self {
        match self {
            LayoutMode::Grid => LayoutMode::Overlay,
            LayoutMode::Overlay => LayoutMode::Single,
            LayoutMode::Single => LayoutMode::Grid,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DisplayConfig {
    pub db_min: f32,
    pub db_max: f32,
    pub freq_min: f32,
    pub freq_max: f32,
    pub peak_hold: bool,
    pub averaging_alpha: f32,
    pub frozen: bool,
    pub layout: LayoutMode,
    pub active_channel: usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            db_min: crate::theme::DEFAULT_DB_MIN,
            db_max: crate::theme::DEFAULT_DB_MAX,
            freq_min: crate::theme::DEFAULT_FREQ_MIN,
            freq_max: crate::theme::DEFAULT_FREQ_MAX,
            peak_hold: false,
            averaging_alpha: 0.20,
            frozen: false,
            layout: LayoutMode::Grid,
            active_channel: 0,
        }
    }
}
