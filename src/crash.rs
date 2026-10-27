//! Makes panics visible. The release exe has no console, so without this a panic would
//! end the notch with no trace at all.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::panic;
use std::path::{Path, PathBuf};
use std::thread;

use chrono::Local;

use crate::window::{self, MessageKind};

/// How a panic is reported besides the crash note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Report {
    /// Only the note. For hook mode, which must never block Claude Code on a dialog.
    NoteOnly,
    /// The note, plus a message box when the panic stops the notch itself.
    NoteAndDialog,
}

/// Records every panic in the crash note, keeping the default report on stderr.
///
/// Panics on background threads only end that thread, so they get the note but no
/// dialog. A panic on the main thread ends the notch.
pub fn install(report: Report) {
    let default = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        default(info);
        let thread = thread::current();
        let thread = thread.name().unwrap_or("unnamed");
        let saved = note_path().filter(|path| append_note(path, thread, &info.to_string()));
        if report == Report::NoteAndDialog && thread == "main" {
            let saved = saved.map_or_else(
                || "The details could not be saved.".to_owned(),
                |path| format!("The details were saved to:\n{}", path.display()),
            );
            let text = format!("notchling stopped because of a bug.\n\n{info}\n\n{saved}");
            window::message_box("notchling", &text, MessageKind::Error);
        }
    }));
}

/// `%LOCALAPPDATA%\notchling\crash.log`, or `None` when `LOCALAPPDATA` is not set.
fn note_path() -> Option<PathBuf> {
    let local = env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join("notchling").join("crash.log"))
}

/// Appends one panic to the crash note. Returns whether it was written; it never
/// panics, since a panic inside the panic hook aborts the process.
fn append_note(path: &Path, thread: &str, panic: &str) -> bool {
    let time = Local::now().format("%Y-%m-%d %H:%M:%S %:z");
    let version = env!("CARGO_PKG_VERSION");
    let note = format!("{time} notchling {version}, thread {thread}: {panic}\n\n");
    path.parent()
        .is_some_and(|dir| fs::create_dir_all(dir).is_ok())
        && OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all(note.as_bytes()))
            .is_ok()
}

#[cfg(test)]
mod tests {
    use std::process;

    use super::*;

    #[test]
    fn notes_are_appended_with_thread_and_message() {
        let dir = env::temp_dir().join(format!("notchling-crash-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("notchling").join("crash.log");

        assert!(append_note(
            &path,
            "main",
            "panicked at src\\main.rs:1:1:\nfirst"
        ));
        assert!(append_note(
            &path,
            "usage",
            "panicked at src\\usage\\mod.rs:2:2:\nsecond"
        ));

        let text = fs::read_to_string(&path).expect("note was written");
        let first = text.find("thread main: panicked").expect("first note");
        let second = text.find("thread usage: panicked").expect("second note");
        assert!(first < second, "{text}");
        assert!(text.contains(concat!("notchling ", env!("CARGO_PKG_VERSION"))));
        let _ = fs::remove_dir_all(dir);
    }
}
