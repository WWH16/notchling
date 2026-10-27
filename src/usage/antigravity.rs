//! Gemini usage from the Antigravity CLI: its `/usage` command prints the share left of
//! each model group's limits, exact as Google reports them.

use std::env;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, bail, ensure};
use chrono::DateTime;

use super::{Limit, Usage};

/// Keeps `agy` from opening a console window each time it runs.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How long `agy` may take to answer; it usually does in about 7 seconds.
const PRINT_TIMEOUT: &str = "60s";

/// Runs the Antigravity CLI's `/usage` command.
#[derive(Debug)]
pub struct Antigravity {
    exe: PathBuf,
    /// An empty folder of notchling's own, so the CLI never runs in a real project.
    workspace: PathBuf,
}

impl Antigravity {
    /// Uses the Antigravity CLI where its installer puts it, `%LOCALAPPDATA%\agy\bin`,
    /// or `None` when it is not installed there.
    #[must_use]
    pub fn find() -> Option<Self> {
        let local = PathBuf::from(env::var_os("LOCALAPPDATA")?);
        let exe = local.join("agy").join("bin").join("agy.exe");
        exe.is_file().then(|| Self {
            exe,
            workspace: local.join("notchling").join("antigravity"),
        })
    }

    /// Asks the CLI for the current limits. Takes several seconds, so call it from a
    /// background thread.
    ///
    /// # Errors
    ///
    /// Fails when the CLI cannot run, exits with an error, or prints no limits, such as
    /// when it is signed out.
    pub fn fetch(&self) -> anyhow::Result<Usage> {
        fs::create_dir_all(&self.workspace)
            .with_context(|| format!("creating {}", self.workspace.display()))?;
        let output = Command::new(&self.exe)
            .args(["--print", "/usage", "--print-timeout", PRINT_TIMEOUT])
            .current_dir(&self.workspace)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .context("running the Antigravity CLI")?;
        ensure!(
            output.status.success(),
            "the Antigravity CLI failed ({})",
            output.status
        );
        parse_usage(&String::from_utf8_lossy(&output.stdout))
    }
}

/// Reads `/usage` output: one limit per line, its group, name, share left and reset
/// time separated by tabs, such as
/// `Gemini Models\tFive Hour Limit Remaining\t80%\t2026-10-27T18:40:20Z`. Groups keep the
/// CLI's order, each listing its shortest window first; the pill follows the tightest
/// limit of all.
fn parse_usage(text: &str) -> anyhow::Result<Usage> {
    let mut limits: Vec<Limit> = text.lines().filter_map(parse_line).collect();
    let mut groups: Vec<Option<String>> = Vec::new();
    for limit in &limits {
        if !groups.contains(&limit.group) {
            groups.push(limit.group.clone());
        }
    }
    limits.sort_by_key(|limit| {
        let group = groups.iter().position(|group| *group == limit.group);
        (group, window_rank(&limit.name))
    });
    let Some(tightest) = limits.iter().max_by(|a, b| a.used.total_cmp(&b.used)) else {
        bail!("the Antigravity CLI reported no limits");
    };
    Ok(Usage {
        used: Some(tightest.used),
        resets_at: tightest.resets_at,
        via: Some("Antigravity"),
        limits,
        ..Usage::default()
    })
}

fn parse_line(line: &str) -> Option<Limit> {
    let mut fields = line.split('\t').map(str::trim);
    let (group, name, left) = (fields.next()?, fields.next()?, fields.next()?);
    let left: f32 = left.strip_suffix('%')?.trim().parse().ok()?;
    let resets_at = fields
        .next()
        .and_then(|reset| DateTime::parse_from_rfc3339(reset).ok())
        .map(|at| at.to_utc());
    Some(Limit {
        group: Some(group.to_owned()),
        name: limit_name(name),
        used: (1.0 - left / 100.0).clamp(0.0, 1.0),
        resets_at,
    })
}

/// "5-hour limit" or "Weekly limit" from the CLI's "Five Hour Limit Remaining" or
/// "Gemini 4 Argon Weekly Limit"; other names as they are, less "Remaining".
fn limit_name(name: &str) -> String {
    let lower = name.to_lowercase();
    if lower.contains("five hour") {
        "5-hour limit".to_owned()
    } else if lower.contains("daily") {
        "Daily limit".to_owned()
    } else if lower.contains("weekly") {
        "Weekly limit".to_owned()
    } else {
        name.trim_end_matches(" Remaining").to_owned()
    }
}

/// Shorter windows sort first within a group.
fn window_rank(name: &str) -> u8 {
    match name {
        "5-hour limit" => 0,
        "Daily limit" => 1,
        "Weekly limit" => 2,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    /// The shape of `agy -p /usage` output, with synthetic shares and times.
    const OUTPUT: &str = "Gemini Models\tWeekly Limit Remaining\t77%\t2026-11-03T11:46:50Z\n\
        Gemini Models\tFive Hour Limit Remaining\t40%\t2026-10-27T18:40:20Z\n\
        Claude and GPT models\tWeekly Limit Remaining\t100%\t2026-11-03T13:40:20Z\n\
        Claude and GPT models\tFive Hour Limit Remaining\t95%\t2026-10-27T18:40:20Z\n\
        Gemini 4 Argon\tGemini 4 Argon Weekly Limit\t100%\t2026-11-03T13:40:20Z\n";

    fn time(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("valid test timestamp")
            .to_utc()
    }

    #[test]
    fn groups_keep_their_order_with_the_shortest_window_first() {
        let usage = parse_usage(OUTPUT).expect("output parses");
        let listed: Vec<(&str, &str)> = usage
            .limits
            .iter()
            .map(|limit| (limit.group.as_deref().unwrap_or(""), limit.name.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                ("Gemini Models", "5-hour limit"),
                ("Gemini Models", "Weekly limit"),
                ("Claude and GPT models", "5-hour limit"),
                ("Claude and GPT models", "Weekly limit"),
                ("Gemini 4 Argon", "Weekly limit"),
            ]
        );
        assert!(
            (usage.limits[0].used - 0.6).abs() < 1e-6,
            "40% left is 60% used"
        );
        assert_eq!(
            usage.limits[1].resets_at,
            Some(time("2026-11-03T11:46:50Z"))
        );
        assert_eq!(usage.via, Some("Antigravity"));
        assert!(!usage.estimated);
    }

    #[test]
    fn the_pill_follows_the_tightest_limit() {
        let usage = parse_usage(OUTPUT).expect("output parses");
        assert!((usage.used.expect("a share") - 0.6).abs() < 1e-6);
        assert_eq!(usage.resets_at, Some(time("2026-10-27T18:40:20Z")));
    }

    #[test]
    fn other_lines_are_skipped_and_none_is_an_error() {
        let usage = parse_usage("Signing in...\nGemini Models\tFive Hour Limit Remaining\t12%\n")
            .expect("one good line is enough");
        assert_eq!(usage.limits.len(), 1);
        assert_eq!(usage.limits[0].resets_at, None);
        assert!(parse_usage("").is_err());
        assert!(parse_usage("Please sign in to Antigravity.\n").is_err());
    }
}
