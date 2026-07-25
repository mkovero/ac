//! The settings overlay (M4c, #182, `G` in the transfer view).
//!
//! Pure editable state + apply/cancel logic, so the two acceptance
//! criteria that matter — **last-writer-wins persistence** and
//! **cancel is side-effect-free** — are unit tests, not click-throughs.
//! The app draws the rows and routes ↑↓/←→/Enter/Esc to it while open;
//! this module owns what those keys mean and what Enter writes.
//!
//! Persistence is D8: last writer wins in `~/.config/ac/config.json`,
//! the same fields `ac setup` writes (`input_channel`,
//! `reference_channel`, `output_channel`). No separate UI prefs file —
//! [`SettingsOverlay::apply`] merges into the existing config via
//! `ac_core::config::save`, so a concurrent `ac setup` write to a field
//! this overlay didn't touch survives.

use ac_core::config::Config;

/// The editable rows, in display/nav order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Meas,
    Ref,
    Out,
    Level,
}

impl Row {
    const ORDER: [Row; 4] = [Row::Meas, Row::Ref, Row::Out, Row::Level];

    pub fn label(self) -> &'static str {
        match self {
            Row::Meas => "measurement channel",
            Row::Ref => "reference channel",
            Row::Out => "stimulus output channel",
            Row::Level => "start level (dBFS)",
        }
    }
}

/// What [`SettingsOverlay::apply`] hands back so the app can relaunch the
/// session on the new channels and reseed the stimulus start level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Applied {
    pub meas_channel: u32,
    pub ref_channel: u32,
    pub start_level_dbfs: f64,
}

/// Editable snapshot of the settings, seeded from config when opened.
pub struct SettingsOverlay {
    selected: usize,
    meas_channel: u32,
    ref_channel: u32,
    out_channel: u32,
    start_level_dbfs: f64,
    /// The ceiling the level is clamped to on edit (config `drive_max_dbfs`).
    drive_max_dbfs: f64,
}

impl SettingsOverlay {
    /// Open, seeded from `cfg`. A missing `reference_channel` seeds 1
    /// (a placeholder the operator edits) — the fatal-if-missing rule is
    /// enforced at launch, not here; the overlay is where you *set* it.
    pub fn from_config(cfg: &Config, start_level_dbfs: f64) -> Self {
        Self {
            selected: 0,
            meas_channel: cfg.input_channel,
            ref_channel: cfg.reference_channel.unwrap_or(1),
            out_channel: cfg.output_channel,
            start_level_dbfs: start_level_dbfs.min(cfg.drive_max_dbfs),
            drive_max_dbfs: cfg.drive_max_dbfs,
        }
    }

    pub fn selected_row(&self) -> Row {
        Row::ORDER[self.selected]
    }

    /// The current value of each row, formatted for display.
    pub fn rows(&self) -> [(Row, String); 4] {
        [
            (Row::Meas, self.meas_channel.to_string()),
            (Row::Ref, self.ref_channel.to_string()),
            (Row::Out, self.out_channel.to_string()),
            (Row::Level, format!("{:+.1}", self.start_level_dbfs)),
        ]
    }

    /// ↑/↓: move the selected row, wrapping.
    pub fn move_row(&mut self, down: bool) {
        let n = Row::ORDER.len();
        self.selected = if down {
            (self.selected + 1) % n
        } else {
            (self.selected + n - 1) % n
        };
    }

    /// ←/→: change the selected row's value. Channels step by 1 (floored
    /// at 0); the level steps by 1 dB, clamped to `drive_max_dbfs` — the
    /// overlay is one of the level clamp's entry points (drive-path AC).
    pub fn adjust_value(&mut self, increase: bool) {
        match self.selected_row() {
            Row::Meas => self.meas_channel = step_channel(self.meas_channel, increase),
            Row::Ref => self.ref_channel = step_channel(self.ref_channel, increase),
            Row::Out => self.out_channel = step_channel(self.out_channel, increase),
            Row::Level => {
                let d = if increase { 1.0 } else { -1.0 };
                self.start_level_dbfs =
                    (self.start_level_dbfs + d).clamp(-80.0, self.drive_max_dbfs);
            }
        }
    }

    /// Enter: persist (last-writer-wins merge into the on-disk config) and
    /// return what the app needs to relaunch. `path` is `None` for the
    /// real `~/.config/ac/config.json`; tests pass a temp path.
    ///
    /// Merge semantics (D8): load current config, set only our three
    /// fields, save. A field this overlay never edits — say a concurrent
    /// `ac setup` wrote `dbu_ref_vrms` — survives, because `config::save`
    /// merges rather than overwrites.
    pub fn apply(&self, path: Option<&std::path::Path>) -> anyhow::Result<Applied> {
        let mut updates = ac_core::config::load(path).unwrap_or_default();
        updates.input_channel = self.meas_channel;
        updates.reference_channel = Some(self.ref_channel);
        updates.output_channel = self.out_channel;
        ac_core::config::save(&updates, path)?;
        Ok(Applied {
            meas_channel: self.meas_channel,
            ref_channel: self.ref_channel,
            start_level_dbfs: self.start_level_dbfs,
        })
    }
}

fn step_channel(v: u32, increase: bool) -> u32 {
    if increase {
        v + 1
    } else {
        v.saturating_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(input: u32, reference: Option<u32>, out: u32) -> Config {
        Config {
            input_channel: input,
            reference_channel: reference,
            output_channel: out,
            ..Config::default()
        }
    }

    #[test]
    fn seeds_from_config_and_navigates_rows() {
        let cfg = cfg_with(2, Some(5), 3);
        let mut o = SettingsOverlay::from_config(&cfg, -30.0);
        assert_eq!(o.selected_row(), Row::Meas);
        o.move_row(true);
        assert_eq!(o.selected_row(), Row::Ref);
        o.move_row(false);
        assert_eq!(o.selected_row(), Row::Meas);
        // Wrap up from the first row to the last.
        o.move_row(false);
        assert_eq!(o.selected_row(), Row::Level);
    }

    #[test]
    fn channel_edits_floor_at_zero_and_level_clamps_to_ceiling() {
        let mut cfg = cfg_with(0, Some(0), 0);
        cfg.drive_max_dbfs = -10.0;
        let mut o = SettingsOverlay::from_config(&cfg, -12.0);
        // Meas at 0, decrease floors at 0.
        o.adjust_value(false);
        assert_eq!(o.rows()[0].1, "0");
        // Level row: raising past the ceiling clamps (an entry point for
        // the drive-path level clamp).
        o.selected = 3;
        for _ in 0..10 {
            o.adjust_value(true);
        }
        assert!(
            o.start_level_dbfs <= -10.0,
            "level {} exceeded ceiling",
            o.start_level_dbfs
        );
    }

    #[test]
    fn apply_persists_last_writer_wins_without_clobbering_other_fields() {
        let dir = std::env::temp_dir().join(format!("ac-settings-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");

        // A prior config with an unrelated field set (as `ac setup` might
        // have written) plus the channel fields.
        let mut prior = cfg_with(0, Some(1), 0);
        prior.dbu_ref_vrms = 1.234;
        ac_core::config::save(&prior, Some(&path)).unwrap();

        // Overlay edits the channels and applies.
        let cfg = cfg_with(0, Some(1), 0);
        let mut o = SettingsOverlay::from_config(&cfg, -30.0);
        o.selected = 0;
        o.adjust_value(true); // meas 0 -> 1
        o.selected = 1;
        o.adjust_value(true); // ref 1 -> 2
        let applied = o.apply(Some(&path)).unwrap();
        assert_eq!(applied.meas_channel, 1);
        assert_eq!(applied.ref_channel, 2);

        // Reload: our fields written, the unrelated one survived (merge,
        // not overwrite).
        let reloaded = ac_core::config::load(Some(&path)).unwrap();
        assert_eq!(reloaded.input_channel, 1);
        assert_eq!(reloaded.reference_channel, Some(2));
        assert!(
            (reloaded.dbu_ref_vrms - 1.234).abs() < 1e-9,
            "unrelated field clobbered"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Cancel side-effect-freeness: dropping the overlay without calling
    // apply() writes nothing. Asserted by construction — apply() is the
    // ONLY method that touches disk; every edit is in-memory on the
    // overlay's own fields. A test that edits then drops and confirms the
    // file is unchanged:
    #[test]
    fn cancel_leaves_the_config_file_untouched() {
        let dir = std::env::temp_dir().join(format!("ac-settings-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let prior = cfg_with(7, Some(8), 9);
        ac_core::config::save(&prior, Some(&path)).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let cfg = cfg_with(7, Some(8), 9);
        let mut o = SettingsOverlay::from_config(&cfg, -30.0);
        o.adjust_value(true); // edit in-memory
        o.move_row(true);
        o.adjust_value(false);
        // "cancel": never call apply(); o falls out of scope, writing nothing.

        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "cancel must not touch the config file");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
