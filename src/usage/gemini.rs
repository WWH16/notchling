//! Gemini usage: exact limits per model group from the Antigravity CLI when it is
//! installed, otherwise counted from the Gemini CLI's session logs in `~/.gemini/tmp`.
//!
//! The Gemini CLI's logs hold no quota, so that share is today's request count against
//! the daily limit set in the config, and every such reading is an estimate.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;
use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeDelta, Utc, Weekday};
use serde::Deserialize;

use super::antigravity::Antigravity;
use super::{Provider, Source, Usage};
use crate::config::Config;

/// How often the Antigravity CLI is asked. Each run takes several seconds and starts the
/// CLI's own helpers, so it is asked rarely; its limits change slowly.
const ANTIGRAVITY_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Reads Antigravity's limits every [`ANTIGRAVITY_INTERVAL`], or counts today's Gemini
/// CLI requests across all projects against the daily limit in the config.
#[derive(Debug)]
pub struct Gemini {
    /// `~/.gemini/tmp`, holding one folder per project.
    projects: PathBuf,
    antigravity: Option<Antigravity>,
    /// When Antigravity was last asked, and its answer if that worked.
    antigravity_checked: Option<(Instant, Option<Usage>)>,
}

impl Gemini {
    /// Reads the current Windows user's Antigravity CLI, or Gemini CLI logs.
    ///
    /// # Errors
    ///
    /// Fails when `USERPROFILE` is not set.
    pub fn try_new() -> anyhow::Result<Self> {
        let home = env::var_os("USERPROFILE").context("USERPROFILE is not set")?;
        Ok(Self {
            antigravity: Antigravity::find(),
            ..Self::at(Path::new(&home).join(".gemini").join("tmp"))
        })
    }

    fn at(projects: PathBuf) -> Self {
        Self {
            projects,
            antigravity: None,
            antigravity_checked: None,
        }
    }

    /// Antigravity's latest limits, asking again once [`ANTIGRAVITY_INTERVAL`] has
    /// passed. A failed run counts as asked, so a signed-out CLI is not run every poll.
    fn antigravity(&mut self) -> Option<&Usage> {
        let antigravity = self.antigravity.as_ref()?;
        let due = self
            .antigravity_checked
            .as_ref()
            .is_none_or(|(checked, _)| checked.elapsed() >= ANTIGRAVITY_INTERVAL);
        if due {
            self.antigravity_checked = Some((Instant::now(), antigravity.fetch().ok()));
        }
        self.antigravity_checked.as_ref()?.1.as_ref()
    }

    fn usage(&self, now: DateTime<Utc>, daily_limit: u32) -> anyhow::Result<Usage> {
        let (day_start, next_day) = pacific_day(now);
        let since: SystemTime = day_start.into();
        let projects = fs::read_dir(&self.projects)
            .with_context(|| format!("listing {}", self.projects.display()))?;

        let (mut requests, mut tokens) = (0, 0);
        for project in projects.flatten() {
            let Ok(chats) = fs::read_dir(project.path().join("chats")) else {
                continue;
            };
            for chat in chats.flatten() {
                let name = chat.file_name();
                let name = name.to_string_lossy();
                let changed_today = chat
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .is_ok_and(|modified| modified >= since);
                if !(name.starts_with("session-") && name.ends_with(".jsonl") && changed_today) {
                    continue;
                }
                // A log that cannot be read right now is counted on the next poll.
                if let Ok(text) = fs::read_to_string(chat.path()) {
                    let (file_requests, file_tokens) = count_requests(&text, day_start);
                    requests += file_requests;
                    tokens += file_tokens;
                }
            }
        }
        // Display only; precision loss on huge counts does not matter.
        let used = requests as f32 / daily_limit.max(1) as f32;
        Ok(Usage {
            used: Some(used),
            resets_at: Some(next_day),
            via: Some("Gemini CLI"),
            tokens: Some(tokens),
            estimated: true,
            ..Usage::default()
        })
    }
}

impl Source for Gemini {
    fn provider(&self) -> Provider {
        Provider::Gemini
    }

    fn read(&mut self) -> anyhow::Result<Usage> {
        if let Some(usage) = self.antigravity() {
            return Ok(usage.clone());
        }
        self.usage(Utc::now(), Config::load().gemini_daily_requests())
    }
}

#[derive(Debug, Deserialize)]
struct Line {
    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    tokens: Option<Tokens>,
}

#[derive(Debug, Deserialize)]
struct Tokens {
    total: Option<u64>,
}

/// Model replies sent since `since` in one session log, and their tokens. The CLI
/// appends a reply again each time it updates it, so the last copy of each id counts.
fn count_requests(text: &str, since: DateTime<Utc>) -> (u64, u64) {
    let mut replies: HashMap<String, u64> = HashMap::new();
    for line in text.lines() {
        let Ok(line) = serde_json::from_str::<Line>(line) else {
            continue;
        };
        let (Some("gemini"), Some(id), Some(timestamp)) =
            (line.kind.as_deref(), line.id, line.timestamp)
        else {
            continue;
        };
        let sent_today = DateTime::parse_from_rfc3339(&timestamp).is_ok_and(|at| at >= since);
        if sent_today {
            let tokens = line.tokens.and_then(|tokens| tokens.total).unwrap_or(0);
            replies.insert(id, tokens);
        }
    }
    let requests = u64::try_from(replies.len()).expect("a count fits in u64");
    (requests, replies.values().sum())
}

/// The current Pacific-time day as UTC instants, since Google's daily quotas reset at
/// midnight Pacific.
// ponytail: the next midnight uses today's UTC offset, so on the two days a year the
// clocks change the reset shows an hour off; use a time zone database if that matters.
fn pacific_day(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let offset = TimeDelta::hours(pacific_offset_hours(now));
    let today = (now + offset).date_naive();
    let start = today.and_time(NaiveTime::MIN).and_utc() - offset;
    (start, start + TimeDelta::days(1))
}

/// Pacific time's UTC offset at `at`: daylight time from the second Sunday of March
/// at 02:00 PST to the first Sunday of November at 02:00 PDT.
fn pacific_offset_hours(at: DateTime<Utc>) -> i64 {
    let sunday = |month, nth| {
        NaiveDate::from_weekday_of_month_opt(at.year(), month, Weekday::Sun, nth)
            .expect("March and November always have two Sundays")
    };
    let starts = sunday(3, 2).and_time(NaiveTime::MIN).and_utc() + TimeDelta::hours(10);
    let ends = sunday(11, 1).and_time(NaiveTime::MIN).and_utc() + TimeDelta::hours(9);
    if (starts..ends).contains(&at) { -7 } else { -8 }
}

#[cfg(test)]
mod tests {
    use std::process;

    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/usage/gemini_session.jsonl");

    fn time(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("valid test timestamp")
            .to_utc()
    }

    #[test]
    fn replies_count_once_and_only_from_today() {
        // 2026-06-18 starts at 07:00 UTC in daylight time; m4 was sent the day before.
        let (requests, tokens) = count_requests(FIXTURE, time("2026-06-18T07:00:00Z"));
        assert_eq!(requests, 2);
        assert_eq!(tokens, 13_140 + 115);
    }

    #[test]
    fn pacific_day_follows_daylight_time() {
        let cases = [
            // 18:50 PDT on the 26th.
            ("2026-10-27T01:50:00Z", "2026-10-26T07:00:00Z"),
            // 01:00 PST on December 1st.
            ("2026-12-01T09:00:00Z", "2026-12-01T08:00:00Z"),
            // Daylight time starts at 10:00 UTC on 2026-03-08.
            ("2026-03-08T09:59:59Z", "2026-03-08T08:00:00Z"),
            ("2026-03-08T10:00:00Z", "2026-03-08T07:00:00Z"),
            // It ends at 09:00 UTC on 2026-11-01.
            ("2026-11-01T08:59:59Z", "2026-11-01T07:00:00Z"),
            ("2026-11-01T09:00:00Z", "2026-11-01T08:00:00Z"),
        ];
        for (now, want) in cases {
            let (start, next) = pacific_day(time(now));
            assert_eq!(start, time(want), "now {now}");
            assert_eq!(next - start, TimeDelta::days(1), "now {now}");
        }
    }

    #[test]
    fn usage_is_an_estimate_against_the_daily_limit() {
        let dir = env::temp_dir().join(format!("ai-notch-gemini-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        let chats = dir.join("project").join("chats");
        fs::create_dir_all(&chats).expect("temp dir is writable");
        fs::write(chats.join("session-example.jsonl"), FIXTURE).expect("temp file is writable");
        fs::write(chats.join("notes.jsonl"), FIXTURE).expect("temp file is writable");

        let usage = Gemini::at(dir.clone())
            .usage(time("2026-06-18T12:00:00Z"), 100)
            .expect("projects folder exists");

        assert_eq!(usage.used, Some(0.02));
        assert_eq!(usage.resets_at, Some(time("2026-06-19T07:00:00Z")));
        assert_eq!(usage.tokens, Some(13_255));
        assert!(usage.estimated);
        assert!(Gemini::at(dir.join("missing")).read().is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
