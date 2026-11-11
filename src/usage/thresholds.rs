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
    /// Per provider: the window, keyed by its nearest reset minute, and the highest threshold
    /// already alerted in it.
    alerted: HashMap<Provider, (Option<i64>, u32)>,
}

impl Thresholds {
    /// Alerts at each of `percents`; values outside 1..=100 are ignored.
    #[must_use]
    pub fn new(percents: &[u32]) -> Self {
        let mut thresholds = Self {
            percents: Vec::new(),
            alerted: HashMap::new(),
        };
        thresholds.set_percents(percents);
        thresholds
    }

    /// Alerts at each of `percents` from now on. What already alerted in each window
    /// is kept, so saving the settings does not repeat an alert.
    pub fn set_percents(&mut self, percents: &[u32]) {
        self.percents = percents
            .iter()
            .copied()
            .filter(|percent| (1..=100).contains(percent))
            .collect();
        self.percents.sort_unstable();
        self.percents.dedup();
    }

    /// The highest threshold `usage` has reached, if it has not alerted yet in this
    /// window. A new reset time means a new window, so every threshold can fire again.
    pub fn crossed(&mut self, provider: Provider, usage: &Usage) -> Option<Crossing> {
        let used_percent = usage.used? * 100.0;
        // The nearest minute, because the account's reset times jitter by fractions of a
        // second, to either side of the minute.
        let window = usage
            .resets_at
            .map(|at| (at.timestamp_millis() + 30_000).div_euclid(60_000));
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
        // So is a reset reported just short of the minute.
        assert_eq!(
            crossed(&mut thresholds, 0.9, "2026-10-27T04:59:59.904791Z"),
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
    fn changed_percents_keep_what_already_alerted() {
        let mut thresholds = Thresholds::new(&[80, 95]);
        let window = "2026-10-27T05:00:00Z";
        let at_85 = usage(0.85, window);
        assert!(thresholds.crossed(Provider::Claude, &at_85).is_some());
        thresholds.set_percents(&[80, 95]);
        assert_eq!(thresholds.crossed(Provider::Claude, &at_85), None);
        thresholds.set_percents(&[85, 99]);
        let crossed = thresholds.crossed(Provider::Claude, &at_85);
        assert_eq!(crossed.map(|crossing| crossing.percent), Some(85));
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
