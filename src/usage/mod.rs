//! Usage limits of the AI providers, read from each source on a background thread.

use std::thread;
use std::time::{Duration, SystemTime};

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
    pub resets_at: Option<SystemTime>,
    /// Tokens used in the current window, when the source counts them.
    pub tokens: Option<u64>,
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

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

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
