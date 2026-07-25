//! The stimulus state machine (M4c, #182, handoff §5) — the client half
//! of the drive-path safety contract.
//!
//! Pure and headless: it holds state and timing logic and **emits**
//! [`DriveCmd`]s the app sends via `set_drive`; it never touches ZMQ, so
//! every drive-safety acceptance criterion is a unit test with an
//! injected clock. The app is a thin adapter — keypress → event → send
//! the returned command; per-frame → [`StimulusMachine::tick`] → send the
//! returned keepalive.
//!
//! Safety invariants this type is responsible for (drive-path checklist):
//! - launches Idle; no path reaches Driving without an explicit
//!   arm (Space) then fire (Enter);
//! - panic stop (Space/Enter/Esc) works from **both** Armed and Driving;
//! - Armed auto-disarms after 5 s of no {Enter, ↑, ↓} — no "armed
//!   forever";
//! - every level change is clamped to `drive_max_dbfs`, at every entry
//!   point;
//! - while Driving, the current state is re-sent every 250 ms
//!   (idempotent keepalive), with monotone timestamps.

use std::time::{Duration, Instant};

/// Auto-disarm window: Armed returns to Idle after this with no
/// {Enter, ↑, ↓} keypress (§5).
pub const ARM_TIMEOUT: Duration = Duration::from_secs(5);

/// Keepalive resend interval while Driving (§4.3 / §5). 6× margin under
/// the daemon's 1500 ms dead-man; do not change one without the other.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(250);

/// Level step for ↑/↓; Shift multiplies to the coarse step (D7).
pub const FINE_STEP_DB: f64 = 1.0;
pub const COARSE_STEP_DB: f64 = 3.0;

/// Lower bound on the drive level. The ceiling is `drive_max_dbfs`
/// (authoritative server-side, clamped here too); this floor just keeps
/// the client number sane. −80 dBFS is far below any usable stimulus.
pub const LEVEL_FLOOR_DBFS: f64 = -80.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StimState {
    Idle,
    Armed,
    Driving,
}

/// A `set_drive` the app must send. `level_dbfs` is already clamped — the
/// machine never emits an over-ceiling level, so the app sends it as-is
/// and the server's own clamp is a redundant backstop, not the only one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DriveCmd {
    pub on: bool,
    pub level_dbfs: f64,
}

pub struct StimulusMachine {
    state: StimState,
    level_dbfs: f64,
    drive_max_dbfs: f64,
    /// When Armed, the instant auto-disarm fires unless reset.
    disarm_at: Option<Instant>,
    /// When Driving, the instant of the last emitted `set_drive`, for the
    /// 250 ms keepalive cadence.
    last_send: Option<Instant>,
}

impl StimulusMachine {
    /// `start_level_dbfs` is the operator's configured start level (from
    /// the settings overlay / config); it is clamped to the ceiling on
    /// construction so even a stale config cannot seed an over-ceiling
    /// level.
    pub fn new(drive_max_dbfs: f64, start_level_dbfs: f64) -> Self {
        Self {
            state: StimState::Idle,
            level_dbfs: clamp_level(start_level_dbfs, drive_max_dbfs),
            drive_max_dbfs,
            disarm_at: None,
            last_send: None,
        }
    }

    pub fn state(&self) -> StimState {
        self.state
    }

    pub fn level_dbfs(&self) -> f64 {
        self.level_dbfs
    }

    /// Space: arm from Idle, or **stop** from Armed/Driving.
    pub fn press_space(&mut self, now: Instant) -> Option<DriveCmd> {
        match self.state {
            StimState::Idle => {
                self.state = StimState::Armed;
                self.disarm_at = Some(now + ARM_TIMEOUT);
                None
            }
            StimState::Armed => self.stop(),
            StimState::Driving => self.stop(),
        }
    }

    /// Enter: fire from Armed, or **stop** from Driving.
    pub fn press_enter(&mut self, now: Instant) -> Option<DriveCmd> {
        match self.state {
            StimState::Armed => {
                self.state = StimState::Driving;
                self.last_send = Some(now);
                Some(DriveCmd {
                    on: true,
                    level_dbfs: self.level_dbfs,
                })
            }
            StimState::Driving => self.stop(),
            StimState::Idle => None,
        }
    }

    /// Esc: stop / cancel from any state.
    pub fn press_esc(&mut self, _now: Instant) -> Option<DriveCmd> {
        self.stop()
    }

    /// ↑: raise level. `coarse` is the Shift-modified 3 dB step. In Armed
    /// this only adjusts the pending level and resets the disarm timer
    /// (no output in Armed); in Driving it applies live and re-sends.
    pub fn press_up(&mut self, now: Instant, coarse: bool) -> Option<DriveCmd> {
        self.nudge(step(coarse), now)
    }

    /// ↓: lower level.
    pub fn press_down(&mut self, now: Instant, coarse: bool) -> Option<DriveCmd> {
        self.nudge(-step(coarse), now)
    }

    /// Per-frame tick: drives the 5 s auto-disarm and the 250 ms
    /// keepalive. Must be called every frame while a session is live.
    pub fn tick(&mut self, now: Instant) -> Option<DriveCmd> {
        match self.state {
            StimState::Armed => {
                if self.disarm_at.is_some_and(|d| now >= d) {
                    self.state = StimState::Idle;
                    self.disarm_at = None;
                }
                None
            }
            StimState::Driving => {
                if self
                    .last_send
                    .is_none_or(|t| now.duration_since(t) >= KEEPALIVE_INTERVAL)
                {
                    self.last_send = Some(now);
                    Some(DriveCmd {
                        on: true,
                        level_dbfs: self.level_dbfs,
                    })
                } else {
                    None
                }
            }
            StimState::Idle => None,
        }
    }

    /// A best-effort `off` for clean app exit while driving (§5). Returns
    /// the command to send; the dead-man is the real guarantee if the
    /// process dies uncleanly.
    pub fn on_quit(&mut self) -> Option<DriveCmd> {
        if self.state == StimState::Driving {
            self.stop()
        } else {
            None
        }
    }

    /// Unconditional stop to Idle from any state (auto-stop-on-open, PR
    /// #197). Emits `off` if we were driving. Unlike [`Self::on_quit`],
    /// this also clears Armed — opening a modal must never leave the
    /// machine live.
    pub fn on_stop(&mut self) -> Option<DriveCmd> {
        self.stop()
    }

    fn nudge(&mut self, delta: f64, now: Instant) -> Option<DriveCmd> {
        match self.state {
            StimState::Idle => None,
            StimState::Armed => {
                self.level_dbfs = clamp_level(self.level_dbfs + delta, self.drive_max_dbfs);
                // {↑, ↓} reset the disarm timer (§5).
                self.disarm_at = Some(now + ARM_TIMEOUT);
                None
            }
            StimState::Driving => {
                self.level_dbfs = clamp_level(self.level_dbfs + delta, self.drive_max_dbfs);
                self.last_send = Some(now);
                Some(DriveCmd {
                    on: true,
                    level_dbfs: self.level_dbfs,
                })
            }
        }
    }

    /// Common stop: to Idle, emit `off` only if we were actually driving.
    fn stop(&mut self) -> Option<DriveCmd> {
        let was_driving = self.state == StimState::Driving;
        self.state = StimState::Idle;
        self.disarm_at = None;
        self.last_send = None;
        was_driving.then_some(DriveCmd {
            on: false,
            level_dbfs: self.level_dbfs,
        })
    }
}

fn step(coarse: bool) -> f64 {
    if coarse {
        COARSE_STEP_DB
    } else {
        FINE_STEP_DB
    }
}

/// Clamp a level to `[LEVEL_FLOOR_DBFS, drive_max_dbfs]`. The ceiling is
/// the safety bound (D7); the floor keeps the number sane.
fn clamp_level(level: f64, ceiling: f64) -> f64 {
    level.clamp(LEVEL_FLOOR_DBFS, ceiling)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CEILING: f64 = -10.0;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn launches_idle_and_only_arm_then_fire_reaches_driving() {
        let mut m = StimulusMachine::new(CEILING, -30.0);
        assert_eq!(m.state(), StimState::Idle);
        let t = t0();
        // Enter alone from Idle does nothing — no drive without arming.
        assert_eq!(m.press_enter(t), None);
        assert_eq!(m.state(), StimState::Idle);
        // Arm, then fire.
        assert_eq!(m.press_space(t), None); // armed = no output
        assert_eq!(m.state(), StimState::Armed);
        let cmd = m.press_enter(t).expect("fire emits set_drive on");
        assert!(cmd.on);
        assert_eq!(m.state(), StimState::Driving);
    }

    #[test]
    fn panic_stop_works_from_both_armed_and_driving() {
        let t = t0();
        // From Driving, via Space.
        let mut m = StimulusMachine::new(CEILING, -30.0);
        m.press_space(t);
        m.press_enter(t);
        let off = m.press_space(t).expect("stop from driving emits off");
        assert!(!off.on);
        assert_eq!(m.state(), StimState::Idle);

        // From Driving, via Enter and via Esc.
        for stop in [
            StimulusMachine::press_enter as fn(&mut StimulusMachine, Instant) -> Option<DriveCmd>,
            StimulusMachine::press_esc,
        ] {
            let mut m = StimulusMachine::new(CEILING, -30.0);
            m.press_space(t);
            m.press_enter(t);
            let off = stop(&mut m, t).expect("stop from driving emits off");
            assert!(!off.on);
            assert_eq!(m.state(), StimState::Idle);
        }

        // From Armed (no drive was on ⇒ no off command, but state clears).
        for stop in [
            StimulusMachine::press_space as fn(&mut StimulusMachine, Instant) -> Option<DriveCmd>,
            StimulusMachine::press_esc,
        ] {
            let mut m = StimulusMachine::new(CEILING, -30.0);
            m.press_space(t);
            assert_eq!(m.state(), StimState::Armed);
            assert_eq!(stop(&mut m, t), None, "armed stop emits no drive command");
            assert_eq!(m.state(), StimState::Idle);
        }
    }

    #[test]
    fn armed_auto_disarms_after_five_seconds_no_armed_forever() {
        let mut m = StimulusMachine::new(CEILING, -30.0);
        let t = t0();
        m.press_space(t);
        assert_eq!(m.state(), StimState::Armed);
        // Just before the window: still armed.
        assert_eq!(m.tick(t + Duration::from_millis(4_999)), None);
        assert_eq!(m.state(), StimState::Armed);
        // At/after the window: disarmed.
        m.tick(t + ARM_TIMEOUT);
        assert_eq!(m.state(), StimState::Idle);
    }

    #[test]
    fn up_down_reset_the_disarm_timer() {
        let mut m = StimulusMachine::new(CEILING, -30.0);
        let t = t0();
        m.press_space(t);
        // 4 s in, a ↓ resets the 5 s window.
        m.press_down(t + Duration::from_secs(4), false);
        // 8 s total (4 s after the reset) — still armed, would have
        // disarmed at 5 s without the reset.
        assert_eq!(m.tick(t + Duration::from_secs(8)), None);
        assert_eq!(m.state(), StimState::Armed);
        // 9+ s total (>5 s after the reset) — now disarmed.
        m.tick(t + Duration::from_millis(9_001));
        assert_eq!(m.state(), StimState::Idle);
    }

    #[test]
    fn level_is_clamped_to_the_ceiling_at_every_entry_point() {
        let t = t0();
        // Entry point 1: construction from a stale over-ceiling config.
        let m = StimulusMachine::new(CEILING, 6.0);
        assert_eq!(m.level_dbfs(), CEILING);

        // Entry point 2: ↑ while Armed cannot exceed the ceiling.
        let mut m = StimulusMachine::new(CEILING, CEILING - 1.0);
        m.press_space(t);
        m.press_up(t, false); // -11 -> -10 (ceiling)
        m.press_up(t, true); // would be -7, clamped to -10
        assert_eq!(m.level_dbfs(), CEILING);

        // Entry point 3: ↑ while Driving cannot exceed the ceiling, and
        // the emitted command carries the clamped level.
        let mut m = StimulusMachine::new(CEILING, CEILING);
        m.press_space(t);
        m.press_enter(t);
        let cmd = m.press_up(t, true).expect("driving level change emits");
        assert_eq!(cmd.level_dbfs, CEILING);
        assert_eq!(m.level_dbfs(), CEILING);
    }

    #[test]
    fn no_output_while_armed_only_on_fire() {
        let mut m = StimulusMachine::new(CEILING, -30.0);
        let t = t0();
        m.press_space(t);
        // ↑/↓ while armed adjust the level but emit NO drive command.
        assert_eq!(m.press_up(t, false), None);
        assert_eq!(m.press_down(t, false), None);
        assert_eq!(m.state(), StimState::Armed);
    }

    #[test]
    fn keepalive_resends_every_250ms_with_monotone_timestamps() {
        let mut m = StimulusMachine::new(CEILING, -30.0);
        let t = t0();
        m.press_space(t);
        m.press_enter(t); // driving, last_send = t

        // No resend before the interval elapses.
        assert_eq!(m.tick(t + Duration::from_millis(249)), None);
        // Resend at/after the interval; collect the send instants.
        let mut sends = Vec::new();
        for k in 1..=10u32 {
            let now = t + KEEPALIVE_INTERVAL * k + Duration::from_millis(1);
            if m.tick(now).is_some() {
                sends.push(now);
            }
        }
        assert_eq!(sends.len(), 10, "one resend per interval");
        // Monotone strictly increasing.
        for w in sends.windows(2) {
            assert!(w[1] > w[0], "keepalive timestamps must be monotone");
        }
    }

    #[test]
    fn quit_while_driving_emits_off_and_is_a_noop_otherwise() {
        let t = t0();
        let mut m = StimulusMachine::new(CEILING, -30.0);
        m.press_space(t);
        m.press_enter(t);
        let off = m.on_quit().expect("quit while driving stops the drive");
        assert!(!off.on);

        let mut m = StimulusMachine::new(CEILING, -30.0);
        assert_eq!(m.on_quit(), None); // idle
        m.press_space(t);
        assert_eq!(m.on_quit(), None); // armed, nothing driving
    }
}
