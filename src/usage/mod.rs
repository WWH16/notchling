//! Usage limits of the AI providers, read from each source on a background thread.

mod claude;
mod claude_account;
mod claude_local;
#[expect(
    dead_code,
    reason = "shown once the provider selector lands in step 5.3"
)]
mod codex;
#[expect(
    dead_code,
    reason = "shown once the provider selector lands in step 5.3"
)]
mod gemini;

pub use claude::Claude;

use std::collections::BTreeMap;
use std::thread;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};

/// An AI provider the notch can show usage for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Claude,
    ChatGpt,
    Gemini,
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

/// Formats the time left until a reset, e.g. "18d 4h", "4h 25m" or "25m". Rounds up
/// to the minute, so a reset never shows as due before it is.
#[must_use]
pub fn countdown(left: TimeDelta) -> String {
    let minutes = (left.num_seconds().max(0) + 59) / 60;
    let (days, hours, minutes) = (minutes / (24 * 60), minutes / 60 % 24, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{minutes}m"),
        (0, hours) => format!("{hours}h {minutes}m"),
        (days, hours) => format!("{days}d {hours}h"),
    }
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
    fn countdown_rounds_up_to_the_minute() {
        let cases = [
            (TimeDelta::seconds(-5), "0m"),
            (TimeDelta::seconds(0), "0m"),
            (TimeDelta::seconds(1), "1m"),
            (TimeDelta::minutes(59), "59m"),
            (TimeDelta::minutes(60), "1h 0m"),
            (TimeDelta::seconds(4 * 3600 + 24 * 60 + 10), "4h 25m"),
            (TimeDelta::hours(24), "1d 0h"),
            (TimeDelta::minutes(18 * 24 * 60 + 4 * 60 + 30), "18d 4h"),
        ];
        for (left, want) in cases {
            assert_eq!(countdown(left), want, "left {left}");
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
