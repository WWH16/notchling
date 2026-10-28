//! `--install-hooks` / `--uninstall-hooks`: adds or removes notchling's entries in
//! `~/.claude/settings.json`, leaving every other setting and hook untouched.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use serde_json::{Map, Value, json};

use crate::config::{Config, HOOK_ENTRY_MARGIN_SECS};

/// Hook events the notch listens to.
const EVENTS: [&str; 8] = [
    "PermissionRequest",
    "PreToolUse",
    "Notification",
    "Stop",
    "UserPromptSubmit",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionDenied",
];

/// Events that only tell the notch a tool call was settled. They fire on every tool
/// call, so Claude Code runs them in the background instead of waiting for them.
const BACKGROUND_EVENTS: [&str; 3] = ["PostToolUse", "PostToolUseFailure", "PermissionDenied"];

/// Events that wait for the user to answer on the notch, so they get the permission
/// wait as their timeout. Stop waits for a reply to Claude, or only a few seconds when
/// the user leaves the card alone.
const WAITING_EVENTS: [&str; 3] = ["PermissionRequest", "PreToolUse", "Stop"];

/// The only tool the notch hooks before it runs: Claude's multiple-choice questions,
/// which the notch can answer. Every other tool call passes PreToolUse untouched.
const PRE_TOOL_USE_MATCHER: &str = "AskUserQuestion";

/// Claude Code timeout for events that do not wait for the user; the hook returns at once.
const QUICK_EVENT_TIMEOUT_SECS: u64 = 10;

/// What a successful install or uninstall did, for the confirmation message.
#[derive(Debug)]
pub struct Outcome {
    pub settings: PathBuf,
    /// `None` when there was no settings file to back up.
    pub backup: Option<PathBuf>,
}

/// Adds the notch's hooks, pointing at the running exe.
///
/// # Errors
///
/// Fails when the settings file is not valid JSON, or cannot be backed up or written.
pub fn install_hooks() -> anyhow::Result<Outcome> {
    let exe = env::current_exe().context("locating notchling.exe")?;
    let command = hook_command(&exe);
    let permission_timeout = Config::load().hook_timeout().as_secs() + HOOK_ENTRY_MARGIN_SECS;
    update_settings(|settings| install(settings, &command, permission_timeout))
}

/// Whether every notch hook in `settings.json` runs the running exe. `false` when the
/// hooks are missing, or name another copy, such as one that has since been moved.
///
/// # Errors
///
/// Fails when the settings file cannot be read or is not valid JSON.
pub fn hooks_point_here() -> anyhow::Result<bool> {
    let exe = env::current_exe().context("locating notchling.exe")?;
    let path = settings_path()?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    let settings: Value = serde_json::from_str(&text)
        .with_context(|| format!("{} is not valid JSON", path.display()))?;
    Ok(runs_everywhere(&settings, &hook_command(&exe)))
}

/// Removes the notch's hooks.
///
/// # Errors
///
/// Fails when the settings file is not valid JSON, or cannot be backed up or written.
pub fn uninstall_hooks() -> anyhow::Result<Outcome> {
    update_settings(uninstall)
}

fn update_settings(
    change: impl FnOnce(&mut Value) -> anyhow::Result<()>,
) -> anyhow::Result<Outcome> {
    let path = settings_path()?;
    let (mut settings, backup) = match fs::read_to_string(&path) {
        Ok(text) => {
            let settings: Value = serde_json::from_str(&text)
                .with_context(|| format!("{} is not valid JSON; fix it first", path.display()))?;
            (settings, Some(back_up(&path, &text)?))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => (json!({}), None),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    change(&mut settings)?;

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut text = serde_json::to_string_pretty(&settings)?;
    text.push('\n');
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(Outcome {
        settings: path,
        backup,
    })
}

/// `%USERPROFILE%\.claude\settings.json`.
fn settings_path() -> anyhow::Result<PathBuf> {
    let home = env::var_os("USERPROFILE").context("USERPROFILE is not set")?;
    Ok(Path::new(&home).join(".claude").join("settings.json"))
}

/// Copies the settings next to themselves as `settings.json.notchling-backup-<unix time>`.
fn back_up(path: &Path, text: &str) -> anyhow::Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".notchling-backup-{stamp}"));
    let backup = path.with_file_name(name);
    fs::write(&backup, text).with_context(|| format!("writing backup {}", backup.display()))?;
    Ok(backup)
}

/// The shell command Claude Code runs: the quoted exe path with forward slashes, which
/// both Git Bash and PowerShell accept.
fn hook_command(exe: &Path) -> String {
    format!(
        "\"{}\" --hook",
        exe.display().to_string().replace('\\', "/")
    )
}

/// Whether a hook handler is one of ours, including those from before the rename, when
/// the exe was `ai-notch.exe`.
fn is_ours(handler: &Value) -> bool {
    handler
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| {
            (command.contains("notchling") || command.contains("ai-notch"))
                && command.ends_with(" --hook")
        })
}

/// Whether each event the notch listens to has a handler running `command`.
fn runs_everywhere(settings: &Value, command: &str) -> bool {
    EVENTS.iter().all(|event| {
        settings["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|group| group["hooks"].as_array())
            .flatten()
            .any(|handler| handler["command"] == command)
    })
}

/// Adds one notchling entry per event, replacing any earlier ones.
fn install(settings: &mut Value, command: &str, permission_timeout: u64) -> anyhow::Result<()> {
    uninstall(settings)?;
    let hooks = hooks_object(settings)?;
    for event in EVENTS {
        let timeout = if WAITING_EVENTS.contains(&event) {
            permission_timeout
        } else {
            QUICK_EVENT_TIMEOUT_SECS
        };
        let mut handler = json!({ "type": "command", "command": command, "timeout": timeout });
        if BACKGROUND_EVENTS.contains(&event) {
            handler["async"] = true.into();
        }
        let mut entry = json!({ "hooks": [handler] });
        if event == "PreToolUse" {
            entry["matcher"] = PRE_TOOL_USE_MATCHER.into();
        }
        let groups = hooks.entry(event).or_insert_with(|| json!([]));
        let Some(groups) = groups.as_array_mut() else {
            bail!("hooks.{event} in settings.json is not a list");
        };
        groups.push(entry);
    }
    Ok(())
}

/// Removes every notchling handler, and any group or event list left empty by that.
fn uninstall(settings: &mut Value) -> anyhow::Result<()> {
    let hooks = hooks_object(settings)?;
    for event in EVENTS {
        let Some(groups) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                handlers.retain(|handler| !is_ours(handler));
            }
        }
        groups.retain(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|handlers| !handlers.is_empty())
        });
        if groups.is_empty() {
            hooks.remove(event);
        }
    }
    let now_empty = hooks.is_empty();
    if now_empty && let Some(root) = settings.as_object_mut() {
        root.remove("hooks");
    }
    Ok(())
}

/// The `hooks` object, created if missing.
fn hooks_object(settings: &mut Value) -> anyhow::Result<&mut Map<String, Value>> {
    let Some(root) = settings.as_object_mut() else {
        bail!("settings.json is not a JSON object");
    };
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    hooks
        .as_object_mut()
        .context("\"hooks\" in settings.json is not an object")
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMAND: &str = "\"C:/tools/notchling.exe\" --hook";

    #[test]
    fn command_quotes_the_path_with_forward_slashes() {
        let command = hook_command(Path::new(r"S:\PERSONAL PROJECTS\notchling\notchling.exe"));
        assert_eq!(
            command,
            "\"S:/PERSONAL PROJECTS/notchling/notchling.exe\" --hook"
        );
    }

    #[test]
    fn install_adds_all_events_with_their_timeouts() {
        let mut settings = json!({ "model": "opus" });
        install(&mut settings, COMMAND, 130).expect("valid settings");
        for event in EVENTS {
            let handler = &settings["hooks"][event][0]["hooks"][0];
            assert_eq!(handler["command"], COMMAND, "{event}");
        }
        assert_eq!(
            settings["hooks"]["PermissionRequest"][0]["hooks"][0]["timeout"],
            130
        );
        let questions = &settings["hooks"]["PreToolUse"][0];
        assert_eq!(questions["matcher"], "AskUserQuestion");
        assert_eq!(
            questions["hooks"][0]["timeout"], 130,
            "a question waits too"
        );
        assert!(settings["hooks"]["Stop"][0].get("matcher").is_none());
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["timeout"], 130,
            "a reply to Claude waits too"
        );
        assert_eq!(
            settings["hooks"]["Notification"][0]["hooks"][0]["timeout"],
            10
        );
        assert_eq!(
            settings["hooks"]["PostToolUse"][0]["hooks"][0]["async"],
            true
        );
        assert!(
            settings["hooks"]["PermissionRequest"][0]["hooks"][0]
                .get("async")
                .is_none(),
            "a permission request must be waited for"
        );
        assert_eq!(settings["model"], "opus");
    }

    #[test]
    fn install_twice_is_the_same_as_once() {
        let mut once = json!({});
        install(&mut once, COMMAND, 130).expect("valid settings");
        let mut twice = once.clone();
        install(&mut twice, COMMAND, 130).expect("valid settings");
        assert_eq!(once, twice);
    }

    #[test]
    fn other_hooks_survive_install_and_uninstall() {
        let original = json!({
            "hooks": {
                "Stop": [{ "hooks": [{ "type": "command", "command": "say done" }] }],
                "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "lint" }] }]
            },
            "theme": "dark"
        });
        let mut settings = original.clone();
        install(&mut settings, COMMAND, 130).expect("valid settings");
        assert_eq!(settings["hooks"]["Stop"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            settings["hooks"]["PreToolUse"].as_array().map(Vec::len),
            Some(2)
        );
        uninstall(&mut settings).expect("valid settings");
        assert_eq!(settings, original);
    }

    #[test]
    fn uninstall_removes_the_hooks_key_it_created() {
        let original = json!({ "theme": "dark" });
        let mut settings = original.clone();
        install(&mut settings, COMMAND, 130).expect("valid settings");
        uninstall(&mut settings).expect("valid settings");
        assert_eq!(settings, original);
    }

    #[test]
    fn install_replaces_hooks_from_before_the_rename() {
        let mut settings = json!({});
        install(&mut settings, "\"C:/tools/ai-notch.exe\" --hook", 130).expect("valid settings");
        install(&mut settings, COMMAND, 130).expect("valid settings");
        let stop = &settings["hooks"]["Stop"];
        assert_eq!(stop.as_array().map(Vec::len), Some(1), "{stop}");
        assert_eq!(stop[0]["hooks"][0]["command"], COMMAND);
    }

    #[test]
    fn hooks_count_only_when_every_event_runs_this_exe() {
        let mut settings = json!({});
        assert!(!runs_everywhere(&settings, COMMAND), "nothing installed");
        install(&mut settings, "\"D:/old/notchling.exe\" --hook", 130).expect("valid settings");
        assert!(!runs_everywhere(&settings, COMMAND), "another copy");
        install(&mut settings, COMMAND, 130).expect("valid settings");
        assert!(runs_everywhere(&settings, COMMAND));
        settings["hooks"]
            .as_object_mut()
            .expect("hooks object")
            .remove("Stop");
        assert!(!runs_everywhere(&settings, COMMAND), "an event is missing");
    }

    #[test]
    fn malformed_settings_are_refused() {
        assert!(install(&mut json!([]), COMMAND, 130).is_err());
        assert!(install(&mut json!({ "hooks": [] }), COMMAND, 130).is_err());
        assert!(install(&mut json!({ "hooks": { "Stop": {} } }), COMMAND, 130).is_err());
    }
}
