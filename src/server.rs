//! The notch side of the pipe: accepts hook connections and routes their events.

use std::fs::File;
use std::io::BufReader;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::pipe::{self, Listener};
use crate::protocol::{self, Event};
use crate::requests::{Request, Requests};

/// How often a waiting request checks whether its hook process is still there.
const DISCONNECT_CHECK: Duration = Duration::from_millis(250);

/// Pause after a failed accept, so a persistent error cannot spin a core.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// Something the UI should react to.
#[derive(Debug)]
pub enum Update {
    /// The request queue changed; redraw the request card.
    Requests,
    /// A session event other than a permission request.
    Session(Event),
}

/// Serves hook connections on background threads until the process exits.
///
/// `notify` runs on those threads, so it should hand the update to the UI thread.
pub fn spawn(
    mut listener: Listener,
    requests: Arc<Mutex<Requests>>,
    notify: impl Fn(Update) + Send + Clone + 'static,
) {
    thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok(connection) => {
                    let requests = Arc::clone(&requests);
                    let notify = notify.clone();
                    thread::spawn(move || handle(&connection, &requests, &notify));
                }
                Err(_) => thread::sleep(ACCEPT_RETRY),
            }
        }
    });
}

fn handle(connection: &File, requests: &Mutex<Requests>, notify: &impl Fn(Update)) {
    let Ok(Some(event)) = protocol::read_line::<Event>(&mut BufReader::new(connection)) else {
        return;
    };
    match event {
        Event::PermissionRequest {
            origin,
            tool,
            description,
            detail,
        } => {
            let (reply, decision) = mpsc::channel();
            let request = Request {
                session_id: origin.session_id,
                project: origin.project,
                tool,
                description,
                detail,
            };
            let id = lock(requests).push(request, reply);
            notify(Update::Requests);
            loop {
                match decision.recv_timeout(DISCONNECT_CHECK) {
                    Ok(decision) => {
                        // A write failure means the hook already gave up; nothing to do.
                        let _ = protocol::write_line(&mut &*connection, &decision);
                        return;
                    }
                    Err(RecvTimeoutError::Timeout) if pipe::peer_closed(connection) => {
                        // The hook timed out or Claude Code cancelled it.
                        lock(requests).remove(id);
                        notify(Update::Requests);
                        return;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        }
        Event::ToolSettled {
            origin,
            tool,
            detail,
        } => {
            if lock(requests).settle(&origin.session_id, &tool, &detail) > 0 {
                notify(Update::Requests);
            }
        }
        Event::TaskDone { ref origin } => {
            // The turn is over, so nothing from it can still be waiting for an answer.
            if lock(requests).settle_session(&origin.session_id) > 0 {
                notify(Update::Requests);
            }
            notify(Update::Session(event));
        }
        other => notify(Update::Session(other)),
    }
}

fn lock(requests: &Mutex<Requests>) -> std::sync::MutexGuard<'_, Requests> {
    requests.lock().expect("requests lock poisoned")
}
