//! Permission requests waiting for a click, shared between pipe threads and the UI.

use std::collections::VecDeque;
use std::sync::mpsc::Sender;

use crate::protocol::Decision;

/// Identifies one request for the lifetime of the notch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(u64);

impl RequestId {
    /// The id as Slint sees it. Wraps after 2^31 requests, which never happens in a session.
    #[must_use]
    pub fn to_ui(self) -> i32 {
        // Truncation is intended; see the doc comment.
        self.0 as i32
    }

    #[must_use]
    pub fn from_ui(value: i32) -> Self {
        // Inverse of `to_ui` for every id it produced.
        Self(u64::from(value as u32))
    }
}

/// One request shown on the notch.
#[derive(Debug)]
pub struct Pending {
    pub id: RequestId,
    pub project: String,
    pub tool: String,
    pub detail: String,
    reply: Sender<Decision>,
}

/// Requests in arrival order; the front one is on screen.
#[derive(Debug, Default)]
pub struct Requests {
    queue: VecDeque<Pending>,
    next_id: u64,
}

impl Requests {
    /// Queues a request; `reply` receives the decision when the user clicks.
    pub fn push(
        &mut self,
        project: String,
        tool: String,
        detail: String,
        reply: Sender<Decision>,
    ) -> RequestId {
        let id = RequestId(self.next_id);
        self.next_id = (self.next_id + 1) % (1 << 31);
        self.queue.push_back(Pending {
            id,
            project,
            tool,
            detail,
            reply,
        });
        id
    }

    /// The request currently on screen.
    #[must_use]
    pub fn front(&self) -> Option<&Pending> {
        self.queue.front()
    }

    /// How many requests wait behind the one on screen.
    #[must_use]
    pub fn behind_front(&self) -> usize {
        self.queue.len().saturating_sub(1)
    }

    /// Sends `decision` for request `id` and removes it. Returns `false` when the
    /// request is already gone, e.g. because its hook timed out.
    pub fn decide(&mut self, id: RequestId, decision: Decision) -> bool {
        let Some(pending) = self.take(id) else {
            return false;
        };
        // The hook may have given up a moment ago; then nobody needs the answer.
        let _ = pending.reply.send(decision);
        true
    }

    /// Drops request `id` without answering, e.g. because its hook went away.
    pub fn remove(&mut self, id: RequestId) -> bool {
        self.take(id).is_some()
    }

    fn take(&mut self, id: RequestId) -> Option<Pending> {
        let index = self.queue.iter().position(|pending| pending.id == id)?;
        self.queue.remove(index)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn push(requests: &mut Requests, tool: &str) -> (RequestId, mpsc::Receiver<Decision>) {
        let (sender, receiver) = mpsc::channel();
        let id = requests.push("proj".to_owned(), tool.to_owned(), "cmd".to_owned(), sender);
        (id, receiver)
    }

    #[test]
    fn first_in_is_shown_first() {
        let mut requests = Requests::default();
        let (first, _r1) = push(&mut requests, "Bash");
        let (_second, _r2) = push(&mut requests, "Edit");
        assert_eq!(requests.front().map(|p| p.id), Some(first));
        assert_eq!(requests.behind_front(), 1);
    }

    #[test]
    fn deciding_answers_the_hook_and_shows_the_next() {
        let mut requests = Requests::default();
        let (first, r1) = push(&mut requests, "Bash");
        let (second, _r2) = push(&mut requests, "Edit");
        assert!(requests.decide(first, Decision::Deny));
        assert_eq!(r1.try_recv(), Ok(Decision::Deny));
        assert_eq!(requests.front().map(|p| p.id), Some(second));
    }

    #[test]
    fn gone_requests_cannot_be_decided() {
        let mut requests = Requests::default();
        let (id, receiver) = push(&mut requests, "Bash");
        assert!(requests.remove(id));
        assert!(!requests.decide(id, Decision::Allow));
        assert!(
            receiver.try_recv().is_err(),
            "a removed request is never answered"
        );
        assert!(requests.front().is_none());
    }

    #[test]
    fn ids_survive_the_trip_through_slint() {
        let mut requests = Requests::default();
        let (id, _r) = push(&mut requests, "Bash");
        assert_eq!(RequestId::from_ui(id.to_ui()), id);
    }
}
