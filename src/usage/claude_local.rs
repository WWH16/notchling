//! Claude usage worked out from Claude Code's own logs in `~/.claude/projects`.
//!
//! The logs hold token counts but not the plan's limit, so this source never knows the
//! share used and every reading is an estimate.

use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use serde::Deserialize;

use super::{Provider, Source, Usage};

/// Length of Claude's usage window. A window opens at the start of the hour of the
/// first message after the previous window closed.
const WINDOW: TimeDelta = TimeDelta::hours(5);

/// How far back messages are kept. Logs untouched for longer are never opened.
// ponytail: windows are chained from the oldest kept message, so after more than a day
// of nonstop use the window start can be off by up to an hour; keep more if it matters.
const HISTORY: TimeDelta = TimeDelta::hours(24);

/// Reads Claude Code's logs incrementally: each poll reads only what was appended.
#[derive(Debug)]
pub struct ClaudeLocal {
    projects: PathBuf,
    /// Bytes of each log file read so far, always ending on a line break.
    offsets: HashMap<PathBuf, u64>,
    /// Assistant messages from the last [`HISTORY`], keyed by message and request id,
    /// because Claude Code writes one line per content block, each repeating the usage.
    messages: HashMap<String, Message>,
}

#[derive(Debug, Clone, PartialEq)]
struct Message {
    at: DateTime<Utc>,
    model: String,
    tokens: u64,
}

impl ClaudeLocal {
    /// Reads the logs of the current Windows user's Claude Code.
    ///
    /// # Errors
    ///
    /// Fails when `USERPROFILE` is not set.
    pub fn try_new() -> anyhow::Result<Self> {
        let home = env::var_os("USERPROFILE").context("USERPROFILE is not set")?;
        Ok(Self::at(Path::new(&home).join(".claude").join("projects")))
    }

    fn at(projects: PathBuf) -> Self {
        Self {
            projects,
            offsets: HashMap::new(),
            messages: HashMap::new(),
        }
    }

    /// Reads the lines appended to recently changed logs since the last scan.
    fn scan(&mut self, now: DateTime<Utc>) -> anyhow::Result<()> {
        let cutoff = now - HISTORY;
        let mut logs = Vec::new();
        find_logs(&self.projects, &mut logs)
            .with_context(|| format!("listing {}", self.projects.display()))?;
        for (path, modified) in logs {
            if modified >= cutoff {
                // A log that cannot be read right now is retried on the next scan.
                let _ = self.read_appended(path, cutoff);
            }
        }
        Ok(())
    }

    /// Reads the complete lines appended to `path` since the last scan, keeping the
    /// messages sent after `cutoff`.
    fn read_appended(&mut self, path: PathBuf, cutoff: DateTime<Utc>) -> io::Result<()> {
        let mut file = File::open(&path)?;
        let len = file.metadata()?.len();
        let offset = match self.offsets.get(&path) {
            // A shorter file was rewritten, so read it again from the start.
            Some(&offset) if offset <= len => offset,
            _ => 0,
        };
        if offset == len {
            return Ok(());
        }
        file.seek(SeekFrom::Start(offset))?;
        // One line at a time, so a large log never sits in memory whole.
        let mut reader = BufReader::new(file.take(len - offset));
        let mut line = Vec::new();
        let mut read = 0;
        loop {
            line.clear();
            let line_len = reader.read_until(b'\n', &mut line)?;
            // The last line may still be being written; it is read once it is complete.
            let Some(text) = line.strip_suffix(b"\n") else {
                break;
            };
            if let Some((key, message)) = parse_line(text).filter(|(_, m)| m.at >= cutoff) {
                self.messages.entry(key).or_insert(message);
            }
            read += u64::try_from(line_len).expect("a line length fits in u64");
        }
        self.offsets.insert(path, offset + read);
        Ok(())
    }

    /// The usage window open at `now`, from the messages read so far.
    fn usage(&mut self, now: DateTime<Utc>) -> Usage {
        self.messages
            .retain(|_, message| now - message.at < HISTORY);
        let mut messages: Vec<&Message> = self.messages.values().collect();
        messages.sort_by_key(|message| message.at);
        current_window(&messages, now)
    }
}

impl Source for ClaudeLocal {
    fn provider(&self) -> Provider {
        Provider::Claude
    }

    fn read(&mut self) -> anyhow::Result<Usage> {
        let now = Utc::now();
        self.scan(now)?;
        Ok(self.usage(now))
    }
}

/// Collects every `.jsonl` file under `dir` with its last change time. Folders that
/// cannot be listed are skipped; only an unreadable `dir` itself is an error.
fn find_logs(dir: &Path, found: &mut Vec<(PathBuf, DateTime<Utc>)>) -> io::Result<()> {
    for entry in fs::read_dir(dir)?.flatten() {
        // Does not follow symlinks, so a link loop cannot recurse forever.
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let path = entry.path();
        if meta.is_dir() {
            let _ = find_logs(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "jsonl")
            && let Ok(modified) = meta.modified()
        {
            found.push((path, modified.into()));
        }
    }
    Ok(())
}

/// The fields of a log line this source reads; everything else is ignored.
#[derive(Debug, Deserialize)]
struct Line {
    timestamp: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    message: Option<LineMessage>,
}

#[derive(Debug, Deserialize)]
struct LineMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<TokenCounts>,
}

/// Cache reads are left out: they re-send context that was counted when it was
/// written, and would dwarf everything else.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TokenCounts {
    input_tokens: u64,
    cache_creation_input_tokens: u64,
    output_tokens: u64,
}

/// The message on one log line with its deduplication key, or `None` for lines that
/// carry no token usage.
fn parse_line(line: &[u8]) -> Option<(String, Message)> {
    let line: Line = serde_json::from_slice(line).ok()?;
    let message = line.message?;
    let counts = message.usage?;
    let tokens = counts
        .input_tokens
        .saturating_add(counts.cache_creation_input_tokens)
        .saturating_add(counts.output_tokens);
    // Claude Code's own error notices are logged as messages with no usage.
    if tokens == 0 {
        return None;
    }
    let at = DateTime::parse_from_rfc3339(line.timestamp.as_deref()?)
        .ok()?
        .to_utc();
    let key = format!("{}:{}", message.id?, line.request_id.unwrap_or_default());
    let model = message.model.unwrap_or_default();
    Some((key, Message { at, model, tokens }))
}

/// Claude's usage window open at `now`, from messages sorted by time. With no window
/// open, nothing is in use.
fn current_window(messages: &[&Message], now: DateTime<Utc>) -> Usage {
    let mut start: Option<DateTime<Utc>> = None;
    for message in messages {
        if start.is_none_or(|start| message.at >= start + WINDOW) {
            // Only fails for dates centuries away, where the exact hour is moot.
            start = Some(
                message
                    .at
                    .duration_trunc(TimeDelta::hours(1))
                    .unwrap_or(message.at),
            );
        }
    }
    let Some(start) = start.filter(|&start| now < start + WINDOW) else {
        return Usage {
            tokens: Some(0),
            estimated: true,
            ..Usage::default()
        };
    };

    let in_window: Vec<&&Message> = messages.iter().filter(|m| m.at >= start).collect();
    let mut tokens_by_model = BTreeMap::new();
    for message in &in_window {
        *tokens_by_model.entry(message.model.clone()).or_insert(0) += message.tokens;
    }
    let tokens: u64 = tokens_by_model.values().sum();
    let burn_rate = in_window.first().map(|first| {
        let minutes = ((now - first.at).num_seconds() as f32 / 60.0).max(1.0);
        // Display only; precision loss on huge counts does not matter.
        tokens as f32 / minutes
    });
    Usage {
        used: None,
        resets_at: Some(start + WINDOW),
        weekly_used: None,
        tokens: Some(tokens),
        tokens_by_model,
        burn_rate,
        estimated: true,
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::process;

    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/usage/claude_project.jsonl");

    fn time(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("valid test timestamp")
            .to_utc()
    }

    /// A fresh `projects` folder holding one project folder, unique to `test`.
    fn projects_dir(test: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("ai-notch-{test}-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("project")).expect("temp dir is writable");
        dir
    }

    fn append(path: &Path, text: &str) {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all(text.as_bytes()))
            .expect("temp file is writable");
    }

    fn read_at(source: &mut ClaudeLocal, now: &str) -> Usage {
        let now = time(now);
        source.scan(now).expect("projects folder exists");
        source.usage(now)
    }

    #[test]
    fn counts_each_message_once_in_the_open_window() {
        let dir = projects_dir("window");
        append(&dir.join("project").join("session.jsonl"), FIXTURE);
        let mut source = ClaudeLocal::at(dir.clone());

        let usage = read_at(&mut source, "2026-10-26T08:20:05Z");

        // The 01:10 message opened an earlier window, so 07:20 opened this one at 07:00.
        assert_eq!(usage.resets_at, Some(time("2026-10-26T12:00:00Z")));
        assert_eq!(usage.tokens, Some(2100), "{usage:?}");
        let by_model: Vec<(&str, u64)> = usage
            .tokens_by_model
            .iter()
            .map(|(model, &tokens)| (model.as_str(), tokens))
            .collect();
        assert_eq!(
            by_model,
            [("claude-opus-5-5", 1800), ("claude-sonnet-5-5", 300)]
        );
        // 2100 tokens in the hour since the window's first message.
        assert_eq!(usage.burn_rate, Some(35.0));
        assert!(usage.estimated);
        assert_eq!(usage.used, None);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn nothing_is_in_use_once_the_window_closes() {
        let dir = projects_dir("closed");
        append(&dir.join("project").join("session.jsonl"), FIXTURE);
        let mut source = ClaudeLocal::at(dir.clone());

        let usage = read_at(&mut source, "2026-10-26T12:00:00Z");

        assert_eq!(usage.tokens, Some(0));
        assert_eq!(usage.resets_at, None);
        assert_eq!(usage.burn_rate, None);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn appended_lines_are_read_once_and_only_when_complete() {
        let dir = projects_dir("append");
        let log = dir.join("project").join("session.jsonl");
        let lines: Vec<&str> = FIXTURE.lines().collect();
        let (first_reply, second_reply) = (lines[1], lines[3]);
        let now = "2026-10-26T08:20:05Z";
        let mut source = ClaudeLocal::at(dir.clone());

        append(&log, &format!("{first_reply}\n"));
        assert_eq!(read_at(&mut source, now).tokens, Some(1800));

        let (head, tail) = second_reply.split_at(40);
        append(&log, head);
        assert_eq!(read_at(&mut source, now).tokens, Some(1800));

        append(&log, &format!("{tail}\n"));
        assert_eq!(read_at(&mut source, now).tokens, Some(2100));
        let len = fs::metadata(&log).expect("log exists").len();
        assert_eq!(source.offsets.get(&log), Some(&len));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_projects_folder_is_an_error() {
        let dir = env::temp_dir().join(format!("ai-notch-missing-{}", process::id()));
        assert!(ClaudeLocal::at(dir).read().is_err());
    }
}
