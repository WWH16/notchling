//! Keeps the PC from sleeping while any Claude Code session is working, and says how
//! long they have been at it.
//!
//! The display may still turn off; only system sleep is blocked. Windows ties the
//! request to the calling thread, so every method must run on the UI thread.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use windows::Win32::System::Power::{ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState};

/// A session that started work but never reported back is dropped after this long.
/// Tool calls that need no permission send no events, so a long autonomous run is
/// silent until it stops; this has to outlast such runs.
pub const SAFETY_RELEASE: Duration = Duration::from_secs(2 * 60 * 60);

/// Sessions currently working, and whether sleep is being blocked for them.
#[derive(Debug, Default)]
pub struct KeepAwake {
    working: HashMap<String, Instant>,
    holding: bool,
}

impl KeepAwake {
    /// A session started working, e.g. the user sent a prompt.
    pub fn started(&mut self, session_id: String, now: Instant) {
        self.working.insert(session_id, now);
        self.apply();
    }

    /// A session stopped working: it finished or is waiting for the user.
    pub fn stopped(&mut self, session_id: &str) {
        self.working.remove(session_id);
        self.apply();
    }

    /// How many sessions are working at `now`, and for how long the longest-running one
    /// has, or `None` while none is.
    #[must_use]
    pub fn activity(&self, now: Instant) -> Option<(usize, Duration)> {
        let longest = self
            .working
            .values()
            .map(|since| now.saturating_duration_since(*since))
            .max()?;
        Some((self.working.len(), longest))
    }

    /// Forgets sessions that have been silent for longer than [`SAFETY_RELEASE`].
    pub fn expire(&mut self, now: Instant) {
        self.working
            .retain(|_, since| now.saturating_duration_since(*since) < SAFETY_RELEASE);
        self.apply();
    }

    fn apply(&mut self) {
        let wanted = !self.working.is_empty();
        if wanted == self.holding {
            return;
        }
        let flags = if wanted {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED
        } else {
            ES_CONTINUOUS
        };
        // SAFETY: SetThreadExecutionState only changes this thread's power request.
        unsafe { SetThreadExecutionState(flags) };
        self.holding = wanted;
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        self.working.clear();
        self.apply();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_while_any_session_works() {
        let now = Instant::now();
        let mut awake = KeepAwake::default();
        awake.started("a".to_owned(), now);
        awake.started("b".to_owned(), now);
        assert!(awake.holding);
        awake.stopped("a");
        assert!(awake.holding, "b is still working");
        awake.stopped("b");
        assert!(!awake.holding);
    }

    #[test]
    fn activity_counts_sessions_and_times_the_longest() {
        let start = Instant::now();
        let mut awake = KeepAwake::default();
        assert_eq!(awake.activity(start), None);
        awake.started("a".to_owned(), start);
        awake.started("b".to_owned(), start + Duration::from_secs(60));
        let later = start + Duration::from_secs(300);
        assert_eq!(awake.activity(later), Some((2, Duration::from_secs(300))));
        awake.stopped("a");
        assert_eq!(awake.activity(later), Some((1, Duration::from_secs(240))));
    }

    #[test]
    fn silent_sessions_are_released_after_the_safety_window() {
        let start = Instant::now();
        let mut awake = KeepAwake::default();
        awake.started("a".to_owned(), start);
        awake.expire(start + SAFETY_RELEASE - Duration::from_secs(1));
        assert!(awake.holding, "still inside the window");
        awake.expire(start + SAFETY_RELEASE);
        assert!(!awake.holding);
    }

    #[test]
    fn stopping_an_unknown_session_is_harmless() {
        let mut awake = KeepAwake::default();
        awake.stopped("never-started");
        assert!(!awake.holding);
    }
}
