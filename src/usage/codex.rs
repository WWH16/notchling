//! ChatGPT usage from the Codex CLI's session logs in `~/.codex/sessions`.
//!
//! After each turn Codex logs the rate limits its server reported, so the share used is
//! exact as of the last Codex turn on this PC.

use std::cmp::Reverse;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Context;
use chrono::{DateTime, Utc};
use ruzstd::decoding::StreamingDecoder;
use serde::Deserialize;

use super::{Limit, Provider, Source, Usage};

const DAY_MINUTES: u64 = 24 * 60;

/// Length of a weekly window, as Codex reports it.
const WEEK_MINUTES: u64 = 7 * DAY_MINUTES;

/// Length of the ChatGPT Go plan's only window, as Codex reports it (FINDINGS V2).
const MONTH_MINUTES: u64 = 30 * DAY_MINUTES;

/// Reads the rate limits from the newest Codex session that logged any.
#[derive(Debug)]
pub struct Codex {
    sessions: PathBuf,
    /// The newest session file when last read, and the limits found then, so an
    /// unchanged file is not decompressed again.
    latest: Option<(PathBuf, SystemTime, RateLimits)>,
}

impl Codex {
    /// Reads the current Windows user's Codex sessions.
    ///
    /// # Errors
    ///
    /// Fails when `USERPROFILE` is not set.
    pub fn try_new() -> anyhow::Result<Self> {
        let home = env::var_os("USERPROFILE").context("USERPROFILE is not set")?;
        Ok(Self::at(Path::new(&home).join(".codex").join("sessions")))
    }

    fn at(sessions: PathBuf) -> Self {
        Self {
            sessions,
            latest: None,
        }
    }

    /// The limits from the newest session that logged any, reading files only when the
    /// newest one changed since the last call.
    fn limits(&mut self) -> anyhow::Result<&RateLimits> {
        let mut files = Vec::new();
        find_sessions(&self.sessions, &mut files)
            .with_context(|| format!("listing {}", self.sessions.display()))?;
        files.sort_by_key(|(_, modified)| Reverse(*modified));
        let (newest, modified) = files.first().context("Codex has no sessions yet")?;

        let unchanged = self
            .latest
            .as_ref()
            .is_some_and(|(path, at, _)| path == newest && at == modified);
        if !unchanged {
            // A session that has not finished a turn yet has no limits; look further back.
            let limits = files
                .iter()
                .find_map(|(path, _)| read_limits(path).ok().flatten())
                .context("no Codex session has logged rate limits")?;
            self.latest = Some((newest.clone(), *modified, limits));
        }
        let (_, _, limits) = self.latest.as_ref().expect("set just above");
        Ok(limits)
    }
}

impl Source for Codex {
    fn provider(&self) -> Provider {
        Provider::ChatGpt
    }

    fn read(&mut self) -> anyhow::Result<Usage> {
        let now = Utc::now();
        Ok(usage(self.limits()?, now))
    }
}

/// Collects every session log under `dir` with its last change time. Folders that
/// cannot be listed are skipped; only an unreadable `dir` itself is an error.
fn find_sessions(dir: &Path, found: &mut Vec<(PathBuf, SystemTime)>) -> io::Result<()> {
    for entry in fs::read_dir(dir)?.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if meta.is_dir() {
            let _ = find_sessions(&path, found);
        } else if (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"))
            && let Ok(modified) = meta.modified()
        {
            found.push((path, modified));
        }
    }
    Ok(())
}

/// The last rate limits logged in one session file, decompressing `.zst` files.
fn read_limits(path: &Path) -> anyhow::Result<Option<RateLimits>> {
    let mut file = BufReader::new(File::open(path)?);
    let mut text = String::new();
    if path.extension().is_some_and(|ext| ext == "zst") {
        StreamingDecoder::new(&mut file)?.read_to_string(&mut text)?;
    } else {
        file.read_to_string(&mut text)?;
    }
    Ok(last_limits(&text))
}

#[derive(Debug, Deserialize)]
struct Line {
    payload: Option<Payload>,
}

#[derive(Debug, Deserialize)]
struct Payload {
    #[serde(rename = "type")]
    kind: Option<String>,
    rate_limits: Option<RateLimits>,
}

/// The rate limits Codex's server reported, as logged on `token_count` events.
#[derive(Debug, Clone, PartialEq, Deserialize)]
struct RateLimits {
    primary: Option<Window>,
    secondary: Option<Window>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct Window {
    used_percent: f32,
    window_minutes: Option<u64>,
    /// Unix time in seconds.
    resets_at: Option<i64>,
}

/// The rate limits on the last `token_count` event in a session log.
fn last_limits(text: &str) -> Option<RateLimits> {
    text.lines()
        .rev()
        // Cheap check first: most lines are messages and tool output.
        .filter(|line| line.contains("\"rate_limits\""))
        .filter_map(|line| serde_json::from_str::<Line>(line).ok()?.payload)
        .filter(|payload| payload.kind.as_deref() == Some("token_count"))
        .find_map(|payload| {
            payload
                .rate_limits
                .filter(|limits| limits.primary.is_some())
        })
}

/// The share used of each window at `now`, each named after its length; the pill
/// follows the primary window.
fn usage(limits: &RateLimits, now: DateTime<Utc>) -> Usage {
    let primary = limits.primary.as_ref().map(|window| window.at(now));
    let windows = [limits.primary.as_ref(), limits.secondary.as_ref()];
    Usage {
        used: primary.map(|(used, _)| used),
        resets_at: primary.and_then(|(_, resets_at)| resets_at),
        limits: windows
            .into_iter()
            .flatten()
            .map(|window| {
                let (used, resets_at) = window.at(now);
                Limit {
                    group: None,
                    name: window_name(window.window_minutes),
                    used,
                    resets_at,
                }
            })
            .collect(),
        via: Some("Codex CLI"),
        estimated: primary.is_some_and(|(_, resets_at)| resets_at.is_none()),
        ..Usage::default()
    }
}

/// "5-hour limit", "Weekly limit" or "Monthly limit", after how long a window lasts.
fn window_name(minutes: Option<u64>) -> String {
    match minutes {
        Some(WEEK_MINUTES) => "Weekly limit".to_owned(),
        Some(MONTH_MINUTES) => "Monthly limit".to_owned(),
        Some(minutes) if minutes > 0 && minutes % DAY_MINUTES == 0 => {
            format!("{}-day limit", minutes / DAY_MINUTES)
        }
        Some(minutes) if minutes > 0 && minutes % 60 == 0 => {
            format!("{}-hour limit", minutes / 60)
        }
        _ => "Plan limit".to_owned(),
    }
}

impl Window {
    /// The share used at `now` and when the window resets. Once the logged reset has
    /// passed nothing was used since, or a newer turn would have logged fresh limits,
    /// so that reads as 0% with an unknown reset: an estimate, since use on other
    /// devices is not logged here.
    fn at(&self, now: DateTime<Utc>) -> (f32, Option<DateTime<Utc>>) {
        let resets_at = self
            .resets_at
            .and_then(|secs| DateTime::from_timestamp(secs, 0));
        match resets_at {
            Some(at) if at > now => (self.used_percent / 100.0, Some(at)),
            _ => (0.0, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process;

    use ruzstd::encoding::{CompressionLevel, compress_to_vec};

    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/usage/codex_rollout.jsonl");

    fn time(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("valid test timestamp")
            .to_utc()
    }

    fn window(used_percent: f32, window_minutes: u64, resets_at: &str) -> Window {
        Window {
            used_percent,
            window_minutes: Some(window_minutes),
            resets_at: Some(time(resets_at).timestamp()),
        }
    }

    #[test]
    fn fixture_gives_the_last_logged_limits() {
        let limits = last_limits(FIXTURE).expect("fixture logs limits");
        let primary = limits.primary.expect("primary window");
        assert_eq!(primary.used_percent, 10.0);
        assert_eq!(primary.window_minutes, Some(43_200));
        assert_eq!(primary.resets_at, Some(1_794_629_185));
        assert_eq!(limits.secondary, None);
        assert_eq!(last_limits("{\"type\":\"session_meta\"}\nnot json"), None);
    }

    #[test]
    fn limits_hold_until_their_reset_passes() {
        let limits = RateLimits {
            primary: Some(window(41.0, 300, "2026-10-27T05:00:00Z")),
            secondary: Some(window(7.0, WEEK_MINUTES, "2026-10-30T00:00:00Z")),
        };

        let usage_before = usage(&limits, time("2026-10-27T04:00:00Z"));
        assert_eq!(usage_before.used, Some(0.41));
        assert_eq!(usage_before.resets_at, Some(time("2026-10-27T05:00:00Z")));
        let weekly = &usage_before.limits[1];
        assert_eq!((weekly.name.as_str(), weekly.used), ("Weekly limit", 0.07));
        assert_eq!(weekly.resets_at, Some(time("2026-10-30T00:00:00Z")));
        assert!(!usage_before.estimated);

        let usage_after = usage(&limits, time("2026-10-27T05:00:00Z"));
        assert_eq!(usage_after.used, Some(0.0));
        assert_eq!(usage_after.resets_at, None);
        assert_eq!(usage_after.limits[1].used, 0.07);
        assert!(usage_after.estimated);
    }

    #[test]
    fn windows_are_named_after_their_length() {
        let names = |primary: u64, secondary: u64| {
            let limits = RateLimits {
                primary: Some(window(10.0, primary, "2026-11-14T00:00:00Z")),
                secondary: Some(window(3.0, secondary, "2026-11-14T00:00:00Z")),
            };
            usage(&limits, time("2026-10-27T04:00:00Z"))
                .limits
                .into_iter()
                .map(|limit| limit.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(300, WEEK_MINUTES), ["5-hour limit", "Weekly limit"]);
        assert_eq!(names(MONTH_MINUTES, 60), ["Monthly limit", "1-hour limit"]);
        assert_eq!(names(2 * DAY_MINUTES, 0), ["2-day limit", "Plan limit"]);
    }

    #[test]
    fn newest_session_wins_and_compressed_logs_are_read() {
        let dir = env::temp_dir().join(format!("notchling-codex-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        let day = dir.join("2026").join("10").join("15");
        fs::create_dir_all(&day).expect("temp dir is writable");
        let write = |name: &str, bytes: &[u8], age_secs: u64| {
            let path = day.join(name);
            fs::write(&path, bytes).expect("temp file is writable");
            let modified = SystemTime::now() - std::time::Duration::from_secs(age_secs);
            File::options()
                .write(true)
                .open(&path)
                .and_then(|file| file.set_modified(modified))
                .expect("mtime can be set");
        };
        let newer = FIXTURE.replace("\"used_percent\":10.0", "\"used_percent\":55.0");
        write("rollout-old.jsonl", FIXTURE.as_bytes(), 3600);
        let compressed = compress_to_vec(newer.as_bytes(), CompressionLevel::Fastest);
        write("rollout-new.jsonl.zst", &compressed, 60);
        // Newest of all, but it has not finished a turn yet.
        write("rollout-started.jsonl", b"{\"type\":\"session_meta\"}\n", 0);

        let mut source = Codex::at(dir.clone());
        let limits = source.limits().expect("sessions exist");
        assert_eq!(limits.primary.as_ref().map(|w| w.used_percent), Some(55.0));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn no_sessions_is_an_error() {
        let dir = env::temp_dir().join(format!("notchling-codex-none-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir is writable");
        assert!(Codex::at(dir.clone()).read().is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
