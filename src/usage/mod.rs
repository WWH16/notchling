//! Usage limits of the AI providers, read from each source on a background thread.

mod antigravity;
mod claude;
mod claude_account;
mod claude_desktop;
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

/// One limit a provider enforces, as the hover panel lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    /// The models the limit covers, such as "Gemini Models", when the provider splits
    /// its limits by model.
    pub group: Option<String>,
    /// Such as "Current session" or "Weekly limit".
    pub name: String,
    /// Share in use, from 0 to 1.
    pub used: f32,
    pub resets_at: Option<DateTime<Utc>>,
}

/// How much of a provider's limit is in use. Each source fills in what it knows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    /// Share of the limit the pill and the alerts follow, from 0 to 1, when it is known.
    pub used: Option<f32>,
    /// When that limit's window resets.
    pub resets_at: Option<DateTime<Utc>>,
    /// Every limit the provider reported, in its own order; empty when the source only
    /// knows `used`.
    pub limits: Vec<Limit>,
    /// Where the numbers come from, such as "Antigravity", when the provider has more
    /// than one source.
    pub via: Option<&'static str>,
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

/// How often a source whose provider is not tracked checks whether it is again, so a
/// provider turned back on shows its numbers within a few seconds.
const UNTRACKED_CHECK: Duration = Duration::from_secs(2);

/// Reads `source` now and then every `interval` on a background thread, until the
/// process exits, but only while `tracked` says its provider is wanted. A source that
/// fails or even panics never takes the notch down.
///
/// `tracked` and `notify` run on that thread, so `notify` should hand the reading to the
/// UI thread.
pub fn spawn(
    mut source: impl Source,
    interval: Duration,
    tracked: impl Fn(Provider) -> bool + Send + 'static,
    notify: impl Fn(Reading) + Send + 'static,
) {
    thread::spawn(move || {
        loop {
            let provider = source.provider();
            if !tracked(provider) {
                thread::sleep(UNTRACKED_CHECK);
                continue;
            }
            notify(Reading {
                provider,
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

/// "Resets at 2:20 PM" today, "Resets Thu 7:00 AM" within the coming week, "Resets
/// Nov 14" after that, in the time zone of `at`.
#[must_use]
pub fn reset_label<Tz: TimeZone>(at: DateTime<Tz>, now: &DateTime<Tz>) -> String
where
    Tz::Offset: fmt::Display,
{
    let today = at.date_naive() == now.date_naive();
    let time = reset_time(at, now);
    if today {
        format!("Resets at {time}")
    } else {
        format!("Resets {time}")
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

    #[test]
    fn reset_label_says_at_only_for_today() {
        let manila = chrono::FixedOffset::east_opt(8 * 3600).expect("valid offset");
        let at = |text: &str| {
            DateTime::parse_from_rfc3339(text)
                .expect("valid test timestamp")
                .with_timezone(&manila)
        };
        let now = at("2026-10-27T11:47:00+08:00");
        assert_eq!(
            reset_label(at("2026-10-27T22:59:00+08:00"), &now),
            "Resets at 10:59 PM"
        );
        assert_eq!(
            reset_label(at("2026-10-30T06:59:00+08:00"), &now),
            "Resets Fri 6:59 AM"
        );
        assert_eq!(
            reset_label(at("2026-11-14T12:00:00+08:00"), &now),
            "Resets Nov 14"
        );
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
            |_| true,
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
