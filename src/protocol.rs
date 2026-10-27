//! Messages between the hook process and the notch: one JSON object per line.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Longest line either side accepts. Commands and file edits fit easily; anything
/// longer is refused instead of filling memory.
pub const MAX_LINE_BYTES: u64 = 1024 * 1024;

/// Where an event came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub session_id: String,
    /// Project folder name, for display.
    pub project: String,
    /// Process ids from the hook's parent upward, used to find the window to focus.
    pub ancestors: Vec<u32>,
}

/// Something that happened in a Claude Code session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Claude wants to use a tool; the hook waits for a [`Decision`].
    PermissionRequest {
        origin: Origin,
        tool: String,
        /// What Claude says the call does, as Claude Code shows it above the command,
        /// such as "Build release binary"; empty when Claude gave none.
        #[serde(default)]
        description: String,
        /// The full command, or the full tool input for non-shell tools.
        detail: String,
    },
    /// Claude asks the user to pick among options, through its `AskUserQuestion` tool;
    /// the hook waits for a [`Decision::Answered`].
    Question {
        origin: Origin,
        questions: Vec<Question>,
    },
    /// Claude is waiting for the user, e.g. idle or asking a question.
    NeedsInput { origin: Origin, message: String },
    /// Claude finished its turn; the hook waits for a [`Decision::Replied`] that keeps
    /// the conversation going, or anything else to let Claude stop.
    TaskDone {
        origin: Origin,
        /// Claude's final reply of the turn; empty when Claude Code sent none.
        #[serde(default)]
        message: String,
    },
    /// The user sent a prompt, so Claude is now working.
    PromptSubmitted { origin: Origin },
    /// Claude Code ran a tool, saw it fail, or denied it. A request for that call was
    /// answered somewhere else, such as Claude Code's own dialog, and is no longer open.
    ToolSettled {
        origin: Origin,
        tool: String,
        /// Built the same way as a request's detail, so the two can be matched.
        detail: String,
    },
}

/// One question of an `AskUserQuestion` call, in the tool input's own shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    /// The question itself; the answer is keyed by this text.
    pub question: String,
    /// A short label for the question, such as "Framework".
    #[serde(default)]
    pub header: String,
    pub options: Vec<QuestionOption>,
    /// Whether more than one option may be picked.
    #[serde(default)]
    pub multi_select: bool,
}

/// One option Claude offers for a [`Question`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

/// The notch's answer to a [`Event::PermissionRequest`] or an [`Event::Question`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
    /// Hand the request back to Claude Code's own dialog.
    AskInApp,
    /// The options picked, keyed by each question's text; several picks for one
    /// question are joined with ", ", as Claude Code expects.
    Answered {
        answers: BTreeMap<String, String>,
    },
    /// The user's reply to a finished turn, which Claude takes as its next message.
    Replied {
        message: String,
    },
}

/// Writes `message` as one JSON line and flushes.
///
/// # Errors
///
/// Fails when the writer fails.
pub fn write_line(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

/// Reads one JSON line. Returns `Ok(None)` when the other side closed the connection.
///
/// # Errors
///
/// Fails on I/O errors, lines over [`MAX_LINE_BYTES`], and malformed JSON.
pub fn read_line<T: DeserializeOwned>(reader: &mut impl BufRead) -> io::Result<Option<T>> {
    let mut line = String::new();
    let read = reader.take(MAX_LINE_BYTES + 1).read_line(&mut line)?;
    if read == 0 {
        return Ok(None);
    }
    // Read::take caps at MAX_LINE_BYTES + 1, so the conversion cannot truncate.
    if read as u64 > MAX_LINE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too long",
        ));
    }
    Ok(Some(serde_json::from_str(&line)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> Origin {
        Origin {
            session_id: "s1".to_owned(),
            project: "notchling".to_owned(),
            ancestors: vec![10, 20],
        }
    }

    #[test]
    fn events_round_trip_through_lines() {
        let events = [
            Event::PermissionRequest {
                origin: origin(),
                tool: "Bash".to_owned(),
                description: "Build release binary".to_owned(),
                detail: "cargo build\n--release".to_owned(),
            },
            Event::Question {
                origin: origin(),
                questions: vec![Question {
                    question: "Which framework?".to_owned(),
                    header: "Framework".to_owned(),
                    options: vec![QuestionOption {
                        label: "React".to_owned(),
                        description: "Component library".to_owned(),
                    }],
                    multi_select: true,
                }],
            },
            Event::NeedsInput {
                origin: origin(),
                message: "Claude is waiting".to_owned(),
            },
            Event::TaskDone {
                origin: origin(),
                message: "Done. All tests pass.".to_owned(),
            },
            Event::PromptSubmitted { origin: origin() },
            Event::ToolSettled {
                origin: origin(),
                tool: "Bash".to_owned(),
                detail: "cargo build".to_owned(),
            },
        ];
        let mut wire = Vec::new();
        for event in &events {
            write_line(&mut wire, event).expect("writing to a Vec succeeds");
        }
        let mut reader = wire.as_slice();
        for event in &events {
            let got: Event = read_line(&mut reader)
                .expect("valid line")
                .expect("not at end");
            assert_eq!(&got, event);
        }
        assert_eq!(read_line::<Event>(&mut reader).expect("clean end"), None);
    }

    #[test]
    fn decisions_use_snake_case_names() {
        let mut wire = Vec::new();
        write_line(&mut wire, &Decision::AskInApp).expect("writing to a Vec succeeds");
        assert_eq!(wire, b"\"ask_in_app\"\n");

        let answered = Decision::Answered {
            answers: BTreeMap::from([("Which framework?".to_owned(), "React".to_owned())]),
        };
        let mut wire = Vec::new();
        write_line(&mut wire, &answered).expect("writing to a Vec succeeds");
        let got: Decision = read_line(&mut wire.as_slice())
            .expect("valid line")
            .expect("not at end");
        assert_eq!(got, answered);
    }

    #[test]
    fn questions_read_claude_codes_camel_case_input() {
        let question: Question = serde_json::from_str(
            r#"{"question":"Which?","header":"Pick","options":[{"label":"A"}],"multiSelect":true}"#,
        )
        .expect("tool input parses");
        assert!(question.multi_select);
        assert_eq!(question.options[0].description, "");
    }

    #[test]
    fn oversized_and_malformed_lines_are_errors() {
        // Exactly MAX_LINE_BYTES + 1 bytes without a newline.
        let huge = vec![b'x'; usize::try_from(MAX_LINE_BYTES + 1).expect("fits in usize")];
        assert!(read_line::<Decision>(&mut huge.as_slice()).is_err());
        assert!(read_line::<Decision>(&mut b"{not json}\n".as_slice()).is_err());
    }
}
