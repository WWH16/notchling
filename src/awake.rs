//! Keeps the PC from sleeping while any Claude Code session is working, and says how
//! long they have been at it.
//!
//! The display may still turn off; only system sleep is blocked. Windows ties the
//! request to the calling thread, so every method must run on the UI thread.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use windows::Win32::System::Power::{ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState};

/// A working session that has been silent for this long is dropped. Every tool call
/// reports once it has run, so this has to outlast the longest single call.
pub const SAFETY_RELEASE: Duration = Duration::from_secs(2 * 60 * 60);

/// How long after a session stops a tool call's report is taken as a straggler from the
/// turn that just ended. Those reports run in the background, so one can arrive late.
const STRAGGLER_WINDOW: Duration = Duration::from_secs(5);

/// When a working session started its turn and when it was last heard from.
#[derive(Debug, Clone, Copy)]
struct Work {
    since: Instant,
    heard: Instant,
}

/// Sessions currently working, and whether sleep is being blocked for them.
#[derive(Debug, Default)]
pub struct KeepAwake {
    working: HashMap<String, Work>,
    /// When each session last stopped, kept for [`STRAGGLER_WINDOW`].
    stopped: HashMap<String, Instant>,
    holding: bool,
}

impl KeepAwake {
    /// A session started working, e.g. the user sent a prompt.
    pub fn started(&mut self, session_id: String, now: Instant) {
        self.stopped.remove(&session_id);
        let work = Work {
            since: now,
            heard: now,
        };
        self.working.insert(session_id, work);
        self.apply();
    }

    /// A session ran a tool, so it is working even if no prompt was seen: the notch
    /// started mid-turn, or Claude went on after the user answered it. Its working time
    /// keeps counting from where it began.
    pub fn active(&mut self, session_id: &str, now: Instant) {
        let straggler = self
            .stopped
            .get(session_id)
            .is_some_and(|at| now.saturating_duration_since(*at) < STRAGGLER_WINDOW);
        if straggler {
            return;
        }
        match self.working.get_mut(session_id) {
            Some(work) => work.heard = now,
            None => {
                let work = Work {
                    since: now,
                    heard: now,
                };
                self.working.insert(session_id.to_owned(), work);
            }
        }
        self.apply();
    }

    /// A session stopped working at `now`: it finished or is waiting for the user.
    pub fn stopped(&mut self, session_id: &str, now: Instant) {
        self.working.remove(session_id);
        self.stopped.insert(session_id.to_owned(), now);
        self.apply();
    }

    /// How many sessions are working at `now`, and for how long the longest-running one
    /// has, or `None` while none is.
    #[must_use]
    pub fn activity(&self, now: Instant) -> Option<(usize, Duration)> {
        let longest = self
            .working
            .values()
            .map(|work| now.saturating_duration_since(work.since))
            .max()?;
        Some((self.working.len(), longest))
    }

    /// Forgets sessions not heard from for longer than [`SAFETY_RELEASE`].
    pub fn expire(&mut self, now: Instant) {
        self.working
            .retain(|_, work| now.saturating_duration_since(work.heard) < SAFETY_RELEASE);
        self.stopped
            .retain(|_, at| now.saturating_duration_since(*at) < STRAGGLER_WINDOW);
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
        awake.stopped("a", now);
        assert!(awake.holding, "b is still working");
        awake.stopped("b", now);
        assert!(!awake.holding);
    }

    #[test]
    fn a_tool_call_marks_its_session_working_and_keeps_its_start() {
        let start = Instant::now();
        let mut awake = KeepAwake::default();
        awake.active("a", start);
        assert!(awake.holding, "seen mid-turn, with no prompt");
        let later = start + Duration::from_secs(120);
        awake.active("a", later);
        assert_eq!(awake.activity(later), Some((1, Duration::from_secs(120))));

        // Heard from at `later`, so the safety window runs from there.
        awake.expire(start + SAFETY_RELEASE);
        assert!(awake.holding);
        awake.expire(later + SAFETY_RELEASE);
        assert!(!awake.holding);
    }

    #[test]
    fn a_late_report_from_a_finished_turn_does_not_restart_it() {
        let start = Instant::now();
        let mut awake = KeepAwake::default();
        awake.started("a".to_owned(), start);
        awake.stopped("a", start);
        awake.active("a", start + Duration::from_secs(1));
        assert!(!awake.holding, "a straggler");
        awake.active("a", start + STRAGGLER_WINDOW);
        assert!(awake.holding, "a new turn's tool call");
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
        awake.stopped("a", later);
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
        awake.stopped("never-started", Instant::now());
        assert!(!awake.holding);
    }
}
