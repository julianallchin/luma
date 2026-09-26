//! Turns that outlive the panel that started them.
//!
//! A turn belongs to its thread, not to whichever conversation the panel is
//! showing. So the task that drives a [`Turn`] lives here, keyed by thread id,
//! and the panel only *attaches* to it: switching score, track or chat lets go
//! of the view and leaves the turn running. Coming back re-attaches, and the
//! live transcript kept here is what gets seated — the backend writes rows at
//! step boundaries, so a thread read back mid-turn is missing the reply that
//! is being written.
//!
//! One entity for the whole app, shared by every panel. Dropping it drops
//! every task, which drops every [`Turn`], which cancels every turn — quitting
//! needs no second path.
//!
//! A subagent's turn is folded here too, from the [`TurnEvent::Child`] events
//! its parent's turn carries, so a reader opened on the child attaches to it
//! exactly as a panel attaches to its own. That entry is passive: it has no
//! task, counts against no limit and cannot be stopped or steered on its own.
//! It ends with the child's turn, or with its parent's.

use std::collections::HashMap;
use std::time::Instant;

use gpui::{Context, EventEmitter, Task};
use luma_lib::agent::{Transcript, TurnEvent, TurnSteer};

use crate::Turn;

/// How many turns may run at once across the app. Each one is a model stream
/// and, often, a Python cell; past a few the reader cannot follow them anyway.
pub const MAX_RUNNING: usize = 4;

/// What the registry tells the panels and the app.
#[derive(Debug)]
pub enum RunningEvent {
    /// One event of `thread`'s turn, already folded into its live transcript.
    /// A panel showing `thread` folds it into its own; the app listens for
    /// [`TurnEvent::DocumentChanged`] whatever the panel shows.
    Event { thread: String, event: TurnEvent },
    /// `thread`'s turn is over: it finished, failed or was stopped.
    Ended { thread: String },
}

/// What a panel attaching to a running thread seats.
pub(crate) struct Live {
    /// The working indicator's timer origin, for whichever panel attaches.
    pub(crate) since: Instant,
    /// The thread's transcript at send time with every event since folded in
    /// — what a re-attaching panel seats instead of a database read.
    pub(crate) transcript: Transcript,
}

/// One running turn. The task, its live state and its steering handle begin
/// and end together, which is why they are one entry rather than three maps.
struct Running {
    /// Drives the [`Turn`]. Dropping it cancels the turn.
    _task: Task<()>,
    steer: TurnSteer,
    live: Live,
}

/// A subagent's turn, seen through its parent's.
struct Child {
    /// The thread whose turn carries this one's events: a running turn or,
    /// for a nested delegation, another child.
    parent: String,
    live: Live,
}

#[derive(Default)]
pub struct RunningTurns {
    turns: HashMap<String, Running>,
    children: HashMap<String, Child>,
}

impl EventEmitter<RunningEvent> for RunningTurns {}

impl RunningTurns {
    /// Start `thread`'s turn from `transcript`, its history at send time.
    ///
    /// `begin` builds the [`Turn`] and runs only once the turn is admitted, so
    /// a refused send never reaches the backend. Refused when [`MAX_RUNNING`]
    /// turns are running, or when this thread's is — a second turn on one
    /// thread is a steer, and inserting it would silently cancel the first.
    /// Hands back the turn's start.
    pub fn start(
        &mut self,
        thread: &str,
        transcript: Transcript,
        begin: impl FnOnce() -> Turn,
        cx: &mut Context<Self>,
    ) -> Result<Instant, String> {
        if self.turns.contains_key(thread) {
            return Err("This chat is already running.".into());
        }
        if self.turns.len() >= MAX_RUNNING {
            return Err(format!("{MAX_RUNNING} chats are running. Stop one first."));
        }
        let mut turn = begin();
        let steer = turn.steering();
        let id = thread.to_owned();
        let task = cx.spawn(async move |this, cx| {
            while let Some(event) = turn.next().await {
                if this
                    .update(cx, |this, cx| this.fold(&id, event, cx))
                    .is_err()
                {
                    return;
                }
            }
            this.update(cx, |this, cx| this.cancel(&id, cx)).ok();
        });
        let since = Instant::now();
        self.turns.insert(
            thread.to_owned(),
            Running {
                _task: task,
                steer,
                live: Live { since, transcript },
            },
        );
        cx.notify();
        Ok(since)
    }

    /// Redirect `thread`'s turn. A thread with no turn running ignores it,
    /// as a turn that has just ended does.
    pub fn steer(&self, thread: &str, prompt: String) {
        if let Some(running) = self.turns.get(thread) {
            running.steer.send(prompt);
        }
    }

    /// Drop `thread`'s turn, which cancels it.
    ///
    /// Also the *end*-of-turn path: a turn that ran out and one the reader
    /// stopped leave the registry in the same state and say so the same way.
    ///
    /// Only a turn this registry drives can be stopped; a subagent stops with
    /// its parent.
    pub fn cancel(&mut self, thread: &str, cx: &mut Context<Self>) {
        if self.turns.remove(thread).is_some() {
            self.ended(thread, cx);
        }
    }

    /// Say `thread` is over, and end every child its turn was carrying.
    fn ended(&mut self, thread: &str, cx: &mut Context<Self>) {
        let orphans: Vec<String> = self
            .children
            .iter()
            .filter(|(_, child)| child.parent == thread)
            .map(|(id, _)| id.clone())
            .collect();
        for orphan in orphans {
            if self.children.remove(&orphan).is_some() {
                self.ended(&orphan, cx);
            }
        }
        cx.emit(RunningEvent::Ended {
            thread: thread.to_owned(),
        });
        cx.notify();
    }

    /// Whether a turn this registry drives is running on `thread`. A
    /// subagent's thread is not: it is its parent's work.
    #[must_use]
    pub fn is_running(&self, thread: &str) -> bool {
        self.turns.contains_key(thread)
    }

    /// `thread`'s running turn, or the running subagent turn on it, for a
    /// panel attaching to it.
    pub(crate) fn live(&self, thread: &str) -> Option<&Live> {
        self.turns
            .get(thread)
            .map(|running| &running.live)
            .or_else(|| self.children.get(thread).map(|child| &child.live))
    }

    fn fold(&mut self, thread: &str, event: TurnEvent, cx: &mut Context<Self>) {
        if let TurnEvent::Child {
            thread_id: child,
            event,
        } = event
        {
            return self.fold_child(thread, child, *event, cx);
        }
        let live = match self.turns.get_mut(thread) {
            Some(running) => &mut running.live,
            None => match self.children.get_mut(thread) {
                Some(child) => &mut child.live,
                None => return,
            },
        };
        luma_lib::agent::apply(&mut live.transcript, &event);
        cx.emit(RunningEvent::Event {
            thread: thread.to_owned(),
            event,
        });
    }

    /// One event of `child`'s turn, carried by `parent`'s. The first one
    /// opens the child's entry; its `TurnEnded` closes it.
    fn fold_child(
        &mut self,
        parent: &str,
        child: String,
        event: TurnEvent,
        cx: &mut Context<Self>,
    ) {
        let ends = matches!(event, TurnEvent::TurnEnded { .. });
        if !self.children.contains_key(&child) {
            if ends || self.live(parent).is_none() {
                return;
            }
            self.children.insert(
                child.clone(),
                Child {
                    parent: parent.to_owned(),
                    live: Live {
                        since: Instant::now(),
                        transcript: Transcript::default(),
                    },
                },
            );
            cx.notify();
        }
        self.fold(&child, event, cx);
        if ends && self.children.remove(&child).is_some() {
            self.ended(&child, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, Entity, TestAppContext};
    use luma_lib::agent::Role;
    use tokio::sync::mpsc;

    /// A turn with no backend: the test holds the event sender and the end
    /// the steering arrives at. The sender reports `is_closed` once the
    /// registry has dropped the turn — the backend's cancellation signal.
    fn turn() -> (
        Turn,
        mpsc::UnboundedSender<TurnEvent>,
        mpsc::UnboundedReceiver<String>,
    ) {
        let (events, rx) = mpsc::unbounded_channel();
        let (steer, steered) = TurnSteer::channel();
        (Turn { events: rx, steer }, events, steered)
    }

    fn start(
        running: &Entity<RunningTurns>,
        thread: &str,
        cx: &mut TestAppContext,
    ) -> Result<mpsc::UnboundedSender<TurnEvent>, String> {
        let (turn, events, _) = turn();
        running
            .update(cx, |running, cx| {
                running.start(thread, Transcript::default(), || turn, cx)
            })
            .map(|_| events)
    }

    fn ended(
        running: &Entity<RunningTurns>,
        cx: &mut TestAppContext,
    ) -> std::rc::Rc<std::cell::RefCell<Vec<String>>> {
        let seen = std::rc::Rc::<std::cell::RefCell<Vec<String>>>::default();
        let sink = seen.clone();
        cx.update(|cx| {
            cx.subscribe(running, move |_, event, _| {
                if let RunningEvent::Ended { thread } = event {
                    sink.borrow_mut().push(thread.clone());
                }
            })
            .detach();
        });
        seen
    }

    #[gpui::test]
    fn a_turn_nobody_watches_keeps_running(cx: &mut TestAppContext) {
        let running = cx.new(|_| RunningTurns::default());
        let events = start(&running, "a", cx).unwrap();
        // No panel is attached: the events still fold into the live transcript.
        events
            .send(TurnEvent::MessageStarted {
                id: "m1".into(),
                role: Role::Assistant,
            })
            .unwrap();
        events
            .send(TurnEvent::TextDelta {
                text: "half a reply".into(),
            })
            .unwrap();
        cx.run_until_parked();
        running.read_with(cx, |running, _| {
            assert!(running.is_running("a"));
            let live = running.live("a").unwrap();
            assert_eq!(live.transcript.messages.len(), 1);
        });
        assert!(!events.is_closed(), "a detached turn was cancelled");
    }

    #[gpui::test]
    fn cancel_drops_the_turn(cx: &mut TestAppContext) {
        let running = cx.new(|_| RunningTurns::default());
        let ended = ended(&running, cx);
        let a = start(&running, "a", cx).unwrap();
        let b = start(&running, "b", cx).unwrap();
        running.update(cx, |running, cx| running.cancel("a", cx));
        cx.run_until_parked();
        assert!(a.is_closed(), "the cancelled turn is still being read");
        assert!(!b.is_closed(), "cancelling one turn stopped another");
        running.read_with(cx, |running, _| {
            assert!(!running.is_running("a"));
            assert!(running.is_running("b"));
        });
        assert_eq!(*ended.borrow(), ["a"]);
    }

    #[gpui::test]
    fn a_finished_turn_leaves(cx: &mut TestAppContext) {
        let running = cx.new(|_| RunningTurns::default());
        let ended = ended(&running, cx);
        let events = start(&running, "a", cx).unwrap();
        drop(events);
        cx.run_until_parked();
        running.read_with(cx, |running, _| assert!(!running.is_running("a")));
        assert_eq!(*ended.borrow(), ["a"]);
    }

    #[gpui::test]
    fn steering_reaches_the_running_turn(cx: &mut TestAppContext) {
        let running = cx.new(|_| RunningTurns::default());
        let (turn, _events, mut steered) = turn();
        running
            .update(cx, |running, cx| {
                running.start("a", Transcript::default(), || turn, cx)
            })
            .unwrap();
        running.read_with(cx, |running, _| {
            running.steer("a", "and the lasers".into());
        });
        assert_eq!(steered.try_recv().unwrap(), "and the lasers");
    }

    #[gpui::test]
    fn the_limit_refuses_without_starting(cx: &mut TestAppContext) {
        let running = cx.new(|_| RunningTurns::default());
        let held: Vec<_> = (0..MAX_RUNNING)
            .map(|at| start(&running, &format!("t{at}"), cx).unwrap())
            .collect();
        let refused = running.update(cx, |running, cx| {
            running.start(
                "over",
                Transcript::default(),
                || panic!("a refused turn reached the backend"),
                cx,
            )
        });
        assert!(refused.is_err());
        // Stopping one makes room.
        running.update(cx, |running, cx| running.cancel("t0", cx));
        assert!(start(&running, "over", cx).is_ok());
        drop(held);
    }

    fn child(thread: &str, event: TurnEvent) -> TurnEvent {
        TurnEvent::Child {
            thread_id: thread.into(),
            event: Box::new(event),
        }
    }

    /// Events of `thread` the registry emitted, in order.
    fn heard(
        running: &Entity<RunningTurns>,
        thread: &'static str,
        cx: &mut TestAppContext,
    ) -> std::rc::Rc<std::cell::RefCell<Vec<TurnEvent>>> {
        let seen = std::rc::Rc::<std::cell::RefCell<Vec<TurnEvent>>>::default();
        let sink = seen.clone();
        cx.update(|cx| {
            cx.subscribe(running, move |_, event, _| {
                if let RunningEvent::Event { thread: at, event } = event {
                    if at == thread {
                        sink.borrow_mut().push(event.clone());
                    }
                }
            })
            .detach();
        });
        seen
    }

    /// A subagent's turn, carried inside its parent's, is followed live under
    /// the child's own thread — what a reader on that thread attaches to — and
    /// ends when the child's turn does.
    #[gpui::test]
    fn a_child_turn_is_live_under_its_own_thread(cx: &mut TestAppContext) {
        let running = cx.new(|_| RunningTurns::default());
        let ended = ended(&running, cx);
        let heard = heard(&running, "child", cx);
        let parent = start(&running, "parent", cx).unwrap();
        for event in [
            TurnEvent::MessageStarted {
                id: "c1".into(),
                role: Role::Assistant,
            },
            TurnEvent::TextDelta {
                text: "half a child reply".into(),
            },
        ] {
            parent.send(child("child", event)).unwrap();
        }
        cx.run_until_parked();
        assert_eq!(heard.borrow().len(), 2);
        running.read_with(cx, |running, _| {
            let live = running.live("child").expect("the child is not live");
            assert_eq!(live.transcript.messages[0].text(), "half a child reply");
            assert!(
                running
                    .live("parent")
                    .unwrap()
                    .transcript
                    .messages
                    .is_empty(),
                "a child's events reached its parent's transcript"
            );
            // Its parent's work: not a turn of its own to stop or count.
            assert!(!running.is_running("child"));
        });
        running.update(cx, |running, cx| running.cancel("child", cx));
        running.read_with(cx, |running, _| assert!(running.live("child").is_some()));

        parent
            .send(child(
                "child",
                TurnEvent::TurnEnded {
                    outcome: luma_lib::agent::TurnOutcome::Completed,
                },
            ))
            .unwrap();
        cx.run_until_parked();
        running.read_with(cx, |running, _| {
            assert!(running.live("child").is_none());
            assert!(running.is_running("parent"));
        });
        assert_eq!(*ended.borrow(), ["child"]);
    }

    /// A child cannot outlive the turn that carries it, however deep.
    #[gpui::test]
    fn stopping_the_parent_ends_its_children(cx: &mut TestAppContext) {
        let running = cx.new(|_| RunningTurns::default());
        let ended = ended(&running, cx);
        let parent = start(&running, "parent", cx).unwrap();
        let started = TurnEvent::MessageStarted {
            id: "m".into(),
            role: Role::Assistant,
        };
        parent.send(child("child", started.clone())).unwrap();
        parent
            .send(child("child", child("grandchild", started)))
            .unwrap();
        cx.run_until_parked();
        running.read_with(cx, |running, _| {
            assert!(running.live("grandchild").is_some());
        });
        running.update(cx, |running, cx| running.cancel("parent", cx));
        running.read_with(cx, |running, _| {
            assert!(running.live("child").is_none() && running.live("grandchild").is_none());
        });
        let mut ended = ended.borrow().clone();
        ended.sort();
        assert_eq!(ended, ["child", "grandchild", "parent"]);
    }

    /// Children are not turns the app runs, so they take no room from them.
    #[gpui::test]
    fn children_take_no_room_from_the_limit(cx: &mut TestAppContext) {
        let running = cx.new(|_| RunningTurns::default());
        let parent = start(&running, "t0", cx).unwrap();
        for at in 0..MAX_RUNNING {
            parent
                .send(child(
                    &format!("child{at}"),
                    TurnEvent::MessageStarted {
                        id: "m".into(),
                        role: Role::Assistant,
                    },
                ))
                .unwrap();
        }
        cx.run_until_parked();
        let held: Vec<_> = (1..MAX_RUNNING)
            .map(|at| start(&running, &format!("t{at}"), cx).unwrap())
            .collect();
        drop((parent, held));
    }

    #[gpui::test]
    fn one_turn_per_thread(cx: &mut TestAppContext) {
        let running = cx.new(|_| RunningTurns::default());
        let first = start(&running, "a", cx).unwrap();
        assert!(start(&running, "a", cx).is_err());
        cx.run_until_parked();
        assert!(!first.is_closed(), "a second send cancelled the first turn");
    }
}
