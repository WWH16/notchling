//! Usage alerts: each threshold fires once per usage window.

use std::collections::HashMap;

use super::{Provider, Usage};

/// A threshold a provider's usage just crossed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crossing {
    pub percent: u32,
    /// The highest configured threshold, the one that warrants the loudest alert.
    pub last: bool,
}

/// Remembers which thresholds already alerted in each provider's current window.
// ponytail: kept in memory only, so a restart alerts again for the current window;
// store it in the config if that turns out to annoy.
#[derive(Debug)]
pub struct Thresholds {
    /// Ascending, each in 1..=100.
    percents: Vec<u32>,
    /// Per provider: the window, keyed by its reset minute, and the highest threshold
    /// already alerted in it.
    alerted: HashMap<Provider, (Option<i64>, u32)>,
}

impl Thresholds {
    /// Alerts at each of `percents`; values outside 1..=100 are ignored.
    #[must_use]
    pub fn new(percents: &[u32]) -> Self {
        let mut percents: Vec<u32> = percents
            .iter()
            .copied()
            .filter(|percent| (1..=100).contains(percent))
            .collect();
        percents.sort_unstable();
        percents.dedup();
        Self {
            percents,
            alerted: HashMap::new(),
        }
    }

    /// The highest threshold `usage` has reached, if it has not alerted yet in this
    /// window. A new reset time means a new window, so every threshold can fire again.
    pub fn crossed(&mut self, provider: Provider, usage: &Usage) -> Option<Crossing> {
        let used_percent = usage.used? * 100.0;
        // Minutes, because the account's reset times jitter by fractions of a second.
        let window = usage.resets_at.map(|at| at.timestamp() / 60);
        let (alerted_window, alerted) = self.alerted.entry(provider).or_insert((window, 0));
        if *alerted_window != window {
            (*alerted_window, *alerted) = (window, 0);
        }
        // Thresholds are at most 100, so the float conversion is exact.
        let percent = *self
            .percents
            .iter()
            .rev()
            .find(|&&percent| used_percent >= percent as f32)?;
        if percent <= *alerted {
            return None;
        }
        *alerted = percent;
        Some(Crossing {
            percent,
            last: self.percents.last() == Some(&percent),
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::*;

    fn usage(used: f32, resets_at: &str) -> Usage {
        let resets_at: DateTime<Utc> = DateTime::parse_from_rfc3339(resets_at)
            .expect("valid test timestamp")
            .to_utc();
        Usage {
            used: Some(used),
            resets_at: Some(resets_at),
            ..Usage::default()
        }
    }

    #[test]
    fn each_threshold_fires_once_per_window() {
        let mut thresholds = Thresholds::new(&[95, 80, 80, 0, 150]);
        let claude = Provider::Claude;
        let window = "2026-10-27T05:00:00.114010Z";
        let crossed =
            |thresholds: &mut Thresholds, used, at| thresholds.crossed(claude, &usage(used, at));

        assert_eq!(crossed(&mut thresholds, 0.5, window), None);
        let first = Some(Crossing {
            percent: 80,
            last: false,
        });
        assert_eq!(crossed(&mut thresholds, 0.81, window), first);
        assert_eq!(crossed(&mut thresholds, 0.85, window), None);
        // Sub-second jitter in the reset time is the same window.
        assert_eq!(
            crossed(&mut thresholds, 0.9, "2026-10-27T05:00:00.120000Z"),
            None
        );
        let last = Some(Crossing {
            percent: 95,
            last: true,
        });
        assert_eq!(crossed(&mut thresholds, 0.97, window), last);
        assert_eq!(crossed(&mut thresholds, 0.99, window), None);

        // The next window starts over, and a jump straight past both alerts once.
        assert_eq!(crossed(&mut thresholds, 0.96, "2026-10-27T10:00:00Z"), last);
    }

    #[test]
    fn providers_and_unknown_shares_are_separate() {
        let mut thresholds = Thresholds::new(&[80, 95]);
        let window = "2026-10-27T05:00:00Z";
        assert!(
            thresholds
                .crossed(Provider::Claude, &usage(0.9, window))
                .is_some()
        );
        assert!(
            thresholds
                .crossed(Provider::ChatGpt, &usage(0.9, window))
                .is_some()
        );
        assert_eq!(
            thresholds.crossed(Provider::Gemini, &Usage::default()),
            None
        );
    }
}
