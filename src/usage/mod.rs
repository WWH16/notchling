//! Usage limits of the AI providers, read from each source on a background thread.

mod claude;
mod claude_account;
mod claude_local;
mod codex;
mod gemini;
mod thresholds;

pub use claude::Claude;
pub use codex::Codex;
pub use gemini::Gemini;
pub use thresholds::{Crossing, Thresholds};

use std::collections::BTreeMap;
use std::fmt;
use std::thread;
use std::time::Duration;

use chrono::{DateTime, DurationRound, TimeDelta, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// An AI provider the notch can show usage for. Written in the config in lowercase.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Claude,
    ChatGpt,
    Gemini,
}

impl Provider {
    /// Every provider, in menu order.
    pub const ALL: [Self; 3] = [Self::Claude, Self::ChatGpt, Self::Gemini];

    /// The name people know the provider's assistant by.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::ChatGpt => "ChatGPT",
            Self::Gemini => "Gemini",
        }
    }
}

/// How much of a provider's limit is in use. Each source fills in what it knows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    /// Share of the current limit in use, from 0 to 1, when the limit is known.
    pub used: Option<f32>,
    /// When the current limit window resets.
    pub resets_at: Option<DateTime<Utc>>,
    /// Share of the weekly limit in use, from 0 to 1, when the provider has one.
    pub weekly_used: Option<f32>,
    /// When the weekly limit resets.
    pub weekly_resets_at: Option<DateTime<Utc>>,
    /// Tokens used in the current window, when the source counts them.
    pub tokens: Option<u64>,
    /// `tokens` split by model name, when the source knows the models.
    pub tokens_by_model: BTreeMap<String, u64>,
    /// Tokens per minute since the window's first message, when the source counts them.
    pub burn_rate: Option<f32>,
    /// `used` was worked out locally instead of reported by the provider.
    pub estimated: bool,
}

/// Somewhere a provider's usage can be read from, such as its local logs.
pub trait Source: Send + 'static {
    /// The provider this source reports on.
    fn provider(&self) -> Provider;

    /// Reads the current usage. Called once per poll, so a source may keep state
    /// between calls, such as how far into each log file it has read.
    ///
    /// # Errors
    ///
    /// Fails when the data is missing or unreadable. Polling carries on regardless.
    fn read(&mut self) -> anyhow::Result<Usage>;
}

/// The outcome of one poll of one source.
#[derive(Debug)]
pub struct Reading {
    pub provider: Provider,
    pub usage: anyhow::Result<Usage>,
}

/// Reads `source` now and then every `interval` on a background thread, until the
/// process exits. A source that fails or even panics never takes the notch down.
///
/// `notify` runs on that thread, so it should hand the reading to the UI thread.
pub fn spawn(
    mut source: impl Source,
    interval: Duration,
    notify: impl Fn(Reading) + Send + 'static,
) {
    thread::spawn(move || {
        loop {
            notify(Reading {
                provider: source.provider(),
                usage: source.read(),
            });
            thread::sleep(interval);
        }
    });
}

/// Formats a count briefly, e.g. "950", "1.2k", "171k" or "3.4M". Digits are cut,
/// not rounded, so a value never shows as more than it is.
#[must_use]
pub fn compact(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..10_000 => format!("{}.{}k", count / 1_000, count % 1_000 / 100),
        10_000..1_000_000 => format!("{}k", count / 1_000),
        1_000_000..10_000_000 => {
            format!("{}.{}M", count / 1_000_000, count % 1_000_000 / 100_000)
        }
        _ => format!("{}M", count / 1_000_000),
    }
}

/// When a limit resets, in the time zone of `at`: "2:20 PM" today, "Thu 7:00 AM" within
/// the coming week, "Nov 14" after that. Rounded to the nearest minute, since providers
/// report resets a fraction of a second off the minute.
#[must_use]
pub fn reset_time<Tz: TimeZone>(at: DateTime<Tz>, now: &DateTime<Tz>) -> String
where
    Tz::Offset: fmt::Display,
{
    // Only fails for dates centuries away, where the exact minute is moot.
    let at = at
        .clone()
        .duration_round(TimeDelta::minutes(1))
        .unwrap_or(at);
    let days_ahead = (at.date_naive() - now.date_naive()).num_days();
    let format = match days_ahead {
        ..=0 => "%-I:%M %p",
        1..=6 => "%a %-I:%M %p",
        _ => "%b %-d",
    };
    at.format(format).to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn compact_cuts_to_at_most_two_digits_and_a_unit() {
        let cases = [
            (0, "0"),
            (999, "999"),
            (1_000, "1.0k"),
            (9_999, "9.9k"),
            (171_072, "171k"),
            (999_999, "999k"),
            (3_456_789, "3.4M"),
            (42_000_000, "42M"),
        ];
        for (count, want) in cases {
            assert_eq!(compact(count), want, "count {count}");
        }
    }

    #[test]
    fn reset_time_is_a_clock_time_in_the_viewers_zone() {
        let manila = chrono::FixedOffset::east_opt(8 * 3600).expect("valid offset");
        let at = |text: &str| {
            DateTime::parse_from_rfc3339(text)
                .expect("valid test timestamp")
                .with_timezone(&manila)
        };
        let now = at("2026-10-27T11:47:00+08:00");
        let cases = [
            ("2026-10-27T06:19:59.904791Z", "2:20 PM"),
            ("2026-10-27T15:59:00+08:00", "3:59 PM"),
            ("2026-10-29T23:00:00.114010Z", "Fri 7:00 AM"),
            ("2026-10-28T00:30:00+08:00", "Wed 12:30 AM"),
            ("2026-11-14T04:06:25Z", "Nov 14"),
        ];
        for (reset, want) in cases {
            assert_eq!(reset_time(at(reset), &now), want, "reset {reset}");
        }
    }

    /// Fails on the first read, then reports 40% used.
    #[derive(Debug, Default)]
    struct Flaky {
        reads: u32,
    }

    impl Source for Flaky {
        fn provider(&self) -> Provider {
            Provider::Claude
        }

        fn read(&mut self) -> anyhow::Result<Usage> {
            self.reads += 1;
            if self.reads == 1 {
                anyhow::bail!("log folder missing");
            }
            Ok(Usage {
                used: Some(0.4),
                ..Usage::default()
            })
        }
    }

    #[test]
    fn failed_read_is_reported_and_polling_continues() {
        let (tx, rx) = mpsc::channel();
        spawn(
            Flaky::default(),
            Duration::from_millis(10),
            move |reading| {
                // Fails once the test has stopped listening, which is fine.
                let _ = tx.send(reading);
            },
        );
        let next = || {
            rx.recv_timeout(Duration::from_secs(5))
                .expect("the poller sends a reading per interval")
        };

        let first = next();
        assert_eq!(first.provider, Provider::Claude);
        let err = first.usage.expect_err("the first read fails");
        assert_eq!(err.to_string(), "log folder missing");

        let second = next().usage.expect("the second read succeeds");
        assert_eq!(second.used, Some(0.4));
    }
}
