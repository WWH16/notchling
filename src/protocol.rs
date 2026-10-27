//! Messages between the hook process and the notch: one JSON object per line.

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
    /// Claude is waiting for the user, e.g. idle or asking a question.
    NeedsInput { origin: Origin, message: String },
    /// Claude finished its turn.
    TaskDone { origin: Origin },
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

/// The notch's answer to a [`Event::PermissionRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
    /// Hand the request back to Claude Code's own dialog.
    AskInApp,
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
            project: "ai-notch".to_owned(),
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
            Event::NeedsInput {
                origin: origin(),
                message: "Claude is waiting".to_owned(),
            },
            Event::TaskDone { origin: origin() },
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
    }

    #[test]
    fn oversized_and_malformed_lines_are_errors() {
        // Exactly MAX_LINE_BYTES + 1 bytes without a newline.
        let huge = vec![b'x'; usize::try_from(MAX_LINE_BYTES + 1).expect("fits in usize")];
        assert!(read_line::<Decision>(&mut huge.as_slice()).is_err());
        assert!(read_line::<Decision>(&mut b"{not json}\n".as_slice()).is_err());
    }
}
