//! Permission requests and questions waiting for a click, shared between pipe threads
//! and the UI.

use std::collections::{BTreeMap, VecDeque};
use std::mem;
use std::sync::mpsc::Sender;

use crate::protocol::{Decision, Question};

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

/// Something a Claude Code session waits on the user for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub session_id: String,
    pub project: String,
    pub ask: Ask,
}

/// What a [`Request`] asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// Claude wants to use a tool, answered with Allow or Deny.
    Permission {
        tool: String,
        /// What Claude says the call does; empty when it gave no description.
        description: String,
        /// The full command, or the full tool input for non-shell tools.
        detail: String,
    },
    /// Claude asks the user to pick among options, answered with
    /// [`Decision::Answered`].
    Questions(Vec<Question>),
    /// Claude finished its turn and the user may reply, with [`Decision::Replied`].
    Reply {
        /// Claude's final reply of the turn.
        message: String,
        /// Process ids above the hook, to bring that Claude window to the front.
        ancestors: Vec<u32>,
    },
}

/// One request shown on the notch.
#[derive(Debug)]
pub struct Pending {
    pub id: RequestId,
    pub request: Request,
    reply: Sender<Decision>,
}

/// Requests in arrival order, except that a session waiting on a permission or a
/// question goes ahead of reply cards that are not on screen yet; the front one is on
/// screen.
#[derive(Debug, Default)]
pub struct Requests {
    queue: VecDeque<Pending>,
    next_id: u64,
    /// Progress through the front request's questions, kept by the UI.
    pub answering: Option<Answering>,
}

impl Requests {
    /// Queues a request; `reply` receives the decision when the user clicks. A blocked
    /// session matters more than a finished one, so a permission or a question goes
    /// ahead of the reply cards behind the one on screen.
    pub fn push(&mut self, request: Request, reply: Sender<Decision>) -> RequestId {
        let id = RequestId(self.next_id);
        self.next_id = (self.next_id + 1) % (1 << 31);
        let blocking = !matches!(request.ask, Ask::Reply { .. });
        let at = self
            .queue
            .iter()
            .skip(1)
            .position(|pending| blocking && matches!(pending.request.ask, Ask::Reply { .. }))
            .map_or(self.queue.len(), |behind_front| behind_front + 1);
        self.queue.insert(at, Pending { id, request, reply });
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

    /// Hands back the requests for a call Claude Code has already settled, with no
    /// decision, so their hooks exit and the cards go. Returns how many there were.
    pub fn settle(&mut self, session_id: &str, tool: &str, detail: &str) -> usize {
        self.release(|request| {
            let same_call = match &request.ask {
                Ask::Permission {
                    tool: asked_tool,
                    detail: asked_detail,
                    ..
                } => asked_tool == tool && asked_detail == detail,
                // Answered before the tool runs, so a settled call is never one waiting here.
                Ask::Questions(_) | Ask::Reply { .. } => false,
            };
            request.session_id == session_id && same_call
        })
    }

    /// Hands back every request from a session whose turn has ended, or that the user
    /// sent a new prompt to in Claude Code. Returns how many there were.
    pub fn settle_session(&mut self, session_id: &str) -> usize {
        self.release(|request| request.session_id == session_id)
    }

    fn release(&mut self, settled: impl Fn(&Request) -> bool) -> usize {
        let before = self.queue.len();
        self.queue.retain(|pending| {
            if !settled(&pending.request) {
                return true;
            }
            // The hook may have given up a moment ago; then nobody needs the answer.
            let _ = pending.reply.send(Decision::AskInApp);
            false
        });
        before - self.queue.len()
    }

    fn take(&mut self, id: RequestId) -> Option<Pending> {
        let index = self.queue.iter().position(|pending| pending.id == id)?;
        self.queue.remove(index)
    }
}

/// Progress through one request's questions on the notch: which question shows, which
/// options of a multiple-choice one are ticked, and the answers so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answering {
    pub id: RequestId,
    /// The question on screen.
    pub index: usize,
    /// Ticks on the options of a multiple-choice question.
    pub picked: Vec<bool>,
    answers: BTreeMap<String, String>,
}

impl Answering {
    #[must_use]
    pub fn new(id: RequestId, questions: &[Question]) -> Self {
        Self {
            id,
            index: 0,
            picked: vec![false; questions.first().map_or(0, |q| q.options.len())],
            answers: BTreeMap::new(),
        }
    }

    /// Picks option `option` of the question on screen: answers a single-choice
    /// question, or ticks or unticks it on a multiple-choice one. Returns every answer
    /// once the last question has one.
    pub fn pick(&mut self, questions: &[Question], option: usize) -> Option<Decision> {
        let question = questions.get(self.index)?;
        let label = &question.options.get(option)?.label;
        if question.multi_select {
            if let Some(ticked) = self.picked.get_mut(option) {
                *ticked = !*ticked;
            }
            return None;
        }
        self.answer(questions, label.clone())
    }

    /// Answers the multiple-choice question on screen with its ticked options, joined
    /// with ", " as Claude Code expects. Nothing happens while none is ticked.
    pub fn confirm(&mut self, questions: &[Question]) -> Option<Decision> {
        let question = questions.get(self.index)?;
        let labels: Vec<&str> = question
            .options
            .iter()
            .zip(&self.picked)
            .filter(|(_, ticked)| **ticked)
            .map(|(option, _)| option.label.as_str())
            .collect();
        if labels.is_empty() {
            return None;
        }
        self.answer(questions, labels.join(", "))
    }

    fn answer(&mut self, questions: &[Question], answer: String) -> Option<Decision> {
        let question = questions.get(self.index)?;
        self.answers.insert(question.question.clone(), answer);
        self.index += 1;
        match questions.get(self.index) {
            Some(next) => {
                self.picked = vec![false; next.options.len()];
                None
            }
            None => Some(Decision::Answered {
                answers: mem::take(&mut self.answers),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::protocol::QuestionOption;

    fn question(text: &str, labels: &[&str], multi_select: bool) -> Question {
        Question {
            question: text.to_owned(),
            header: String::new(),
            options: labels
                .iter()
                .map(|label| QuestionOption {
                    label: (*label).to_owned(),
                    description: String::new(),
                })
                .collect(),
            multi_select,
        }
    }

    #[test]
    fn single_choices_answer_and_move_on() {
        let questions = [
            question("Framework?", &["React", "Vue"], false),
            question("Styling?", &["CSS", "Tailwind"], false),
        ];
        let mut answering = Answering::new(RequestId(1), &questions);
        assert_eq!(answering.pick(&questions, 1), None, "one question is left");
        assert_eq!(answering.index, 1);
        let answers = BTreeMap::from([
            ("Framework?".to_owned(), "Vue".to_owned()),
            ("Styling?".to_owned(), "CSS".to_owned()),
        ]);
        assert_eq!(
            answering.pick(&questions, 0),
            Some(Decision::Answered { answers })
        );
    }

    #[test]
    fn multiple_choices_tick_until_confirmed() {
        let questions = [question("Features?", &["Auth", "Search", "Billing"], true)];
        let mut answering = Answering::new(RequestId(1), &questions);
        assert_eq!(answering.confirm(&questions), None, "nothing ticked yet");
        assert_eq!(answering.pick(&questions, 0), None);
        assert_eq!(answering.pick(&questions, 2), None);
        assert_eq!(answering.pick(&questions, 0), None, "unticked again");
        assert_eq!(answering.picked, [false, false, true]);
        assert_eq!(answering.pick(&questions, 1), None);
        let answers = BTreeMap::from([("Features?".to_owned(), "Search, Billing".to_owned())]);
        assert_eq!(
            answering.confirm(&questions),
            Some(Decision::Answered { answers })
        );
    }

    fn reply(session_id: &str) -> Request {
        Request {
            session_id: session_id.to_owned(),
            project: "proj".to_owned(),
            ask: Ask::Reply {
                message: "Done.".to_owned(),
                ancestors: Vec::new(),
            },
        }
    }

    #[test]
    fn blocked_sessions_go_ahead_of_waiting_replies() {
        let mut requests = Requests::default();
        let mut queue = |request: Request| requests.push(request, mpsc::channel().0);
        let shown = queue(reply("s1"));
        let waiting = queue(reply("s2"));
        let permission = queue(request("s3", "Bash", "cargo test"));
        let later = queue(reply("s4"));
        let order: Vec<RequestId> = requests.queue.iter().map(|pending| pending.id).collect();
        assert_eq!(
            order,
            [shown, permission, waiting, later],
            "the reply on screen stays"
        );
        assert_eq!(
            requests.settle("s2", "Bash", "cargo test"),
            0,
            "replies never settle"
        );
    }

    #[test]
    fn options_out_of_range_change_nothing() {
        let questions = [question("Framework?", &["React"], false)];
        let mut answering = Answering::new(RequestId(1), &questions);
        assert_eq!(answering.pick(&questions, 5), None);
        assert_eq!(answering.index, 0);
    }

    fn request(session_id: &str, tool: &str, detail: &str) -> Request {
        Request {
            session_id: session_id.to_owned(),
            project: "proj".to_owned(),
            ask: Ask::Permission {
                tool: tool.to_owned(),
                description: String::new(),
                detail: detail.to_owned(),
            },
        }
    }

    fn push(requests: &mut Requests, tool: &str) -> (RequestId, mpsc::Receiver<Decision>) {
        let (sender, receiver) = mpsc::channel();
        let id = requests.push(request("s1", tool, "cmd"), sender);
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
    fn calls_settled_elsewhere_are_handed_back_without_a_decision() {
        let mut requests = Requests::default();
        let mut queue = |session: &str, tool: &str, detail: &str| {
            let (sender, receiver) = mpsc::channel();
            requests.push(request(session, tool, detail), sender);
            receiver
        };
        let build = queue("s1", "Bash", "cargo build");
        let test = queue("s1", "Bash", "cargo test");
        let other_session = queue("s2", "Bash", "cargo build");

        assert_eq!(requests.settle("s1", "Bash", "cargo build"), 1);
        assert_eq!(build.try_recv(), Ok(Decision::AskInApp));
        assert!(test.try_recv().is_err(), "another command stays open");
        assert!(
            other_session.try_recv().is_err(),
            "another session stays open"
        );
        assert_eq!(requests.settle("s1", "Edit", "cargo test"), 0);

        assert_eq!(requests.settle_session("s1"), 1);
        assert_eq!(test.try_recv(), Ok(Decision::AskInApp));
        assert_eq!(
            requests.front().map(|p| p.request.session_id.as_str()),
            Some("s2")
        );
    }

    #[test]
    fn ids_survive_the_trip_through_slint() {
        let mut requests = Requests::default();
        let (id, _r) = push(&mut requests, "Bash");
        assert_eq!(RequestId::from_ui(id.to_ui()), id);
    }
}
