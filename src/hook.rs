//! `ai-notch --hook`: the command Claude Code runs for each hook event.
//!
//! It reads the event from stdin, forwards it to the notch, and for permission
//! requests prints the notch's decision. Whenever anything goes wrong (no notch, a
//! timeout, bad input) it prints nothing and exits 0, so Claude Code falls back to
//! its own dialog. It never approves anything on its own.

use std::io::{self, BufReader, Read};
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::Config;
use crate::pipe;
use crate::process;
use crate::protocol::{self, Decision, Event, Origin};

/// Largest stdin payload accepted from Claude Code.
const MAX_INPUT_BYTES: u64 = 4 * 1024 * 1024;

/// Message Claude sees when a request is denied from the notch.
const DENY_MESSAGE: &str = "The user denied this from the ai-notch overlay.";

/// The fields of a Claude Code hook payload that the notch uses.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct HookInput {
    hook_event_name: String,
    session_id: String,
    cwd: String,
    tool_name: String,
    tool_input: Value,
    message: String,
    notification_type: String,
}

/// Runs hook mode to completion. Prints the decision JSON when there is one.
pub fn run() {
    if let Some(output) = handle_stdin() {
        println!("{output}");
    }
}

fn handle_stdin() -> Option<String> {
    let mut text = String::new();
    io::stdin()
        .take(MAX_INPUT_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    let input: HookInput = serde_json::from_str(&text).ok()?;
    let event = to_event(&input, process::ancestors())?;
    let wants_reply = matches!(event, Event::PermissionRequest { .. });

    let connection = pipe::connect(&pipe::pipe_name().ok()?).ok()?;
    protocol::write_line(&mut &connection, &event).ok()?;
    if !wants_reply {
        return None;
    }

    // Reads cannot time out on a synchronous pipe, so read on a thread and stop
    // waiting when the timeout passes; exiting the process ends that thread.
    let reader = connection.try_clone().ok()?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let reply = protocol::read_line::<Decision>(&mut BufReader::new(reader));
        // The receiver is gone only after a timeout, when nobody needs the reply.
        let _ = sender.send(reply);
    });
    let decision = receiver
        .recv_timeout(Config::load().hook_timeout())
        .ok()?
        .ok()??;
    decision_output(decision)
}

/// Turns a hook payload into a notch event, or `None` for events the notch ignores.
fn to_event(input: &HookInput, ancestors: Vec<u32>) -> Option<Event> {
    let origin = Origin {
        session_id: input.session_id.clone(),
        project: project_name(&input.cwd),
        ancestors,
    };
    match input.hook_event_name.as_str() {
        "PermissionRequest" => Some(Event::PermissionRequest {
            origin,
            tool: input.tool_name.clone(),
            detail: tool_detail(&input.tool_input),
        }),
        // `permission_prompt` duplicates the PermissionRequest that came just before.
        "Notification" => match input.notification_type.as_str() {
            "idle_prompt"
            | "agent_needs_input"
            | "elicitation_dialog"
            | "elicitation_url_dialog" => Some(Event::NeedsInput {
                origin,
                message: input.message.clone(),
            }),
            _ => None,
        },
        "Stop" => Some(Event::TaskDone { origin }),
        "UserPromptSubmit" => Some(Event::PromptSubmitted { origin }),
        _ => None,
    }
}

/// The full command for shell tools, otherwise the whole tool input as indented JSON.
fn tool_detail(tool_input: &Value) -> String {
    match tool_input.get("command").and_then(Value::as_str) {
        Some(command) => command.to_owned(),
        None => serde_json::to_string_pretty(tool_input).unwrap_or_default(),
    }
}

/// The last component of the session's working directory, e.g. `ai-notch`.
fn project_name(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The JSON Claude Code expects for a decision, or `None` to show its own dialog.
fn decision_output(decision: Decision) -> Option<String> {
    let decision = match decision {
        Decision::Allow => json!({ "behavior": "allow" }),
        Decision::Deny => json!({ "behavior": "deny", "message": DENY_MESSAGE }),
        Decision::AskInApp => return None,
    };
    let output = json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": decision,
        }
    });
    Some(output.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(json: &str) -> HookInput {
        serde_json::from_str(json).expect("fixture is valid JSON")
    }

    fn event_for(json: &str) -> Option<Event> {
        to_event(&fixture(json), vec![42])
    }

    #[test]
    fn permission_request_carries_the_full_command() {
        let event = event_for(include_str!(
            "../tests/fixtures/hooks/permission_request.json"
        ));
        let Some(Event::PermissionRequest {
            origin,
            tool,
            detail,
        }) = event
        else {
            panic!("expected a permission request, got {event:?}");
        };
        assert_eq!(tool, "Bash");
        assert_eq!(detail, "cargo build --release");
        assert_eq!(origin.project, "example");
        assert_eq!(origin.ancestors, vec![42]);
    }

    #[test]
    fn non_shell_tools_show_their_whole_input() {
        let input = json!({ "file_path": "src/main.rs", "old_string": "a", "new_string": "b" });
        let detail = tool_detail(&input);
        assert!(
            detail.contains("\"file_path\": \"src/main.rs\""),
            "{detail}"
        );
        assert!(detail.contains("\"new_string\": \"b\""), "{detail}");
    }

    #[test]
    fn duplicate_permission_notifications_are_ignored() {
        assert_eq!(
            event_for(include_str!("../tests/fixtures/hooks/notification.json")),
            None
        );
    }

    #[test]
    fn idle_notifications_become_needs_input() {
        let json = r#"{"hook_event_name":"Notification","session_id":"s","cwd":"C:\\w\\p",
            "message":"Claude is waiting for your input","notification_type":"idle_prompt"}"#;
        assert!(matches!(event_for(json), Some(Event::NeedsInput { .. })));
    }

    #[test]
    fn stop_and_prompt_map_to_their_events() {
        let stop = event_for(include_str!("../tests/fixtures/hooks/stop.json"));
        assert!(matches!(stop, Some(Event::TaskDone { .. })), "{stop:?}");
        let prompt = event_for(include_str!(
            "../tests/fixtures/hooks/user_prompt_submit.json"
        ));
        assert!(
            matches!(prompt, Some(Event::PromptSubmitted { .. })),
            "{prompt:?}"
        );
    }

    #[test]
    fn unknown_events_are_ignored() {
        assert_eq!(event_for(r#"{"hook_event_name":"PreCompact"}"#), None);
    }

    #[test]
    fn decisions_match_the_documented_format() {
        let allow: Value = serde_json::from_str(&decision_output(Decision::Allow).expect("allow"))
            .expect("valid JSON");
        assert_eq!(
            allow,
            json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest",
                "decision": {"behavior": "allow"}}})
        );
        let deny: Value = serde_json::from_str(&decision_output(Decision::Deny).expect("deny"))
            .expect("valid JSON");
        assert_eq!(deny["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert_eq!(
            deny["hookSpecificOutput"]["decision"]["message"],
            DENY_MESSAGE
        );
        assert_eq!(decision_output(Decision::AskInApp), None);
    }

    #[test]
    fn project_name_is_the_last_folder() {
        assert_eq!(project_name(r"S:\PERSONAL PROJECTS\ai-notch"), "ai-notch");
        assert_eq!(project_name(""), "");
    }
}
