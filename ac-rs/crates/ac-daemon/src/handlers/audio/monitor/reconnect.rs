//! Per-channel reconnect-failure tracking for the multi-channel monitor
//! path (#93): backoff between retries, error-frame rate limiting, and
//! giving up on a sustained outage.

/// Per-channel state for the multi-channel monitor's `eng.reconnect_input()`
/// path. Tracks consecutive failures so the worker can rate-limit error
/// frames, back off between retries, and give up on a sustained outage.
/// (#93 fix — without this, a permanently-disconnected port would
/// re-enter the reconnect path on every tick, flooding both the JACK
/// syscall and the PUB socket.)
pub(super) struct ReconnectState {
    pub(super) consecutive_failures: u32,
    pub(super) first_failure_at: Option<std::time::Instant>,
    pub(super) last_error_pub_at: Option<std::time::Instant>,
}

pub(super) const RECONNECT_GIVE_UP: std::time::Duration = std::time::Duration::from_secs(30);
pub(super) const RECONNECT_ERR_RATE_LIMIT: std::time::Duration = std::time::Duration::from_secs(1);

impl ReconnectState {
    pub(super) fn new() -> Self {
        Self {
            consecutive_failures: 0,
            first_failure_at: None,
            last_error_pub_at: None,
        }
    }

    pub(super) fn note_success(&mut self) {
        self.consecutive_failures = 0;
        self.first_failure_at = None;
        self.last_error_pub_at = None;
    }

    pub(super) fn note_failure(&mut self, now: std::time::Instant) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.first_failure_at.is_none() {
            self.first_failure_at = Some(now);
        }
    }

    /// Back-off before the next retry. 1st failure = no sleep; ramps to a
    /// 1 s cap so a permanently-disconnected channel doesn't busy-loop.
    pub(super) fn backoff(&self) -> std::time::Duration {
        std::time::Duration::from_millis(match self.consecutive_failures {
            0 | 1 => 0,
            2..=4 => 100,
            5..=9 => 500,
            _ => 1000,
        })
    }

    /// True when the first failure was ≥ `RECONNECT_GIVE_UP` ago — caller
    /// should emit a terminal error and `return` from the worker.
    pub(super) fn should_give_up(&self, now: std::time::Instant) -> bool {
        self.first_failure_at
            .is_some_and(|t0| now.duration_since(t0) >= RECONNECT_GIVE_UP)
    }

    /// True when the current error PUB should be emitted (≥ 1 s since the
    /// last one, or first error of this outage). Updates the timestamp as
    /// a side effect.
    pub(super) fn should_emit_error(&mut self, now: std::time::Instant) -> bool {
        let emit = self
            .last_error_pub_at
            .is_none_or(|t| now.duration_since(t) >= RECONNECT_ERR_RATE_LIMIT);
        if emit {
            self.last_error_pub_at = Some(now);
        }
        emit
    }
}

#[cfg(test)]
mod reconnect_state_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn backoff_ramps_then_caps() {
        let mut st = ReconnectState::new();
        let now = Instant::now();

        assert_eq!(st.backoff(), Duration::ZERO);
        st.note_failure(now);
        assert_eq!(st.backoff(), Duration::ZERO, "1st failure: no sleep yet");

        st.note_failure(now);
        assert_eq!(st.backoff(), Duration::from_millis(100), "2nd failure");
        st.note_failure(now);
        st.note_failure(now);
        assert_eq!(st.backoff(), Duration::from_millis(100), "4th failure");

        st.note_failure(now);
        assert_eq!(st.backoff(), Duration::from_millis(500), "5th failure");
        for _ in 0..4 {
            st.note_failure(now);
        }
        assert_eq!(st.backoff(), Duration::from_millis(500), "9th failure");

        st.note_failure(now);
        assert_eq!(
            st.backoff(),
            Duration::from_millis(1000),
            "10th failure caps at 1s"
        );
        for _ in 0..50 {
            st.note_failure(now);
        }
        assert_eq!(st.backoff(), Duration::from_millis(1000), "stays capped");
    }

    #[test]
    fn note_success_resets_state() {
        let mut st = ReconnectState::new();
        let now = Instant::now();
        for _ in 0..7 {
            st.note_failure(now);
        }
        let _ = st.should_emit_error(now);
        assert!(st.first_failure_at.is_some());
        assert!(st.last_error_pub_at.is_some());

        st.note_success();
        assert_eq!(st.consecutive_failures, 0);
        assert!(st.first_failure_at.is_none());
        assert!(st.last_error_pub_at.is_none());
        assert_eq!(st.backoff(), Duration::ZERO);
    }

    #[test]
    fn should_emit_error_rate_limits() {
        let mut st = ReconnectState::new();
        let t0 = Instant::now();

        st.note_failure(t0);
        assert!(st.should_emit_error(t0), "first error always emits");

        let t_half = t0 + Duration::from_millis(500);
        st.note_failure(t_half);
        assert!(!st.should_emit_error(t_half), "0.5 s later: suppressed");

        let t_2 = t0 + Duration::from_millis(1100);
        st.note_failure(t_2);
        assert!(st.should_emit_error(t_2), "1.1 s later: emit again");

        let t_3 = t_2 + Duration::from_millis(900);
        st.note_failure(t_3);
        assert!(
            !st.should_emit_error(t_3),
            "0.9 s after last emit: suppressed"
        );
    }

    #[test]
    fn should_give_up_only_after_30s_of_failures() {
        let mut st = ReconnectState::new();
        let t0 = Instant::now();

        assert!(!st.should_give_up(t0), "no failures yet — never give up");

        st.note_failure(t0);
        assert!(!st.should_give_up(t0));
        assert!(!st.should_give_up(t0 + Duration::from_secs(29)));
        assert!(st.should_give_up(t0 + Duration::from_secs(30)));
        assert!(st.should_give_up(t0 + Duration::from_secs(60)));
    }

    #[test]
    fn first_failure_at_is_sticky_until_success() {
        let mut st = ReconnectState::new();
        let t0 = Instant::now();
        st.note_failure(t0);
        let initial = st.first_failure_at;
        assert!(initial.is_some());

        for n in 1..5 {
            st.note_failure(t0 + Duration::from_millis(n * 200));
            assert_eq!(st.first_failure_at, initial, "anchor fixed across failures");
        }

        st.note_success();
        assert!(st.first_failure_at.is_none());
    }
}
