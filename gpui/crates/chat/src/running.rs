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

/// One running turn. The task, its start and its steering handle begin and end
/// together, which is why they are one entry rather than three maps.
pub(crate) struct Running {
    /// Drives the [`Turn`]. Dropping it cancels the turn.
    _task: Task<()>,
    /// The working indicator's timer origin, for whichever panel attaches.
    pub(crate) since: Instant,
    steer: TurnSteer,
    /// The thread's transcript at send time with every event since folded in
    /// — what a re-attaching panel seats instead of a database read.
    pub(crate) transcript: Transcript,
}

#[derive(Default)]
pub struct RunningTurns {
    turns: HashMap<String, Running>,
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
                since,
                steer,
                transcript,
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
    pub fn cancel(&mut self, thread: &str, cx: &mut Context<Self>) {
        if self.turns.remove(thread).is_some() {
            cx.emit(RunningEvent::Ended {
                thread: thread.to_owned(),
            });
            cx.notify();
        }
    }

    #[must_use]
    pub fn is_running(&self, thread: &str) -> bool {
        self.turns.contains_key(thread)
    }

    /// `thread`'s running turn, for a panel attaching to it.
    pub(crate) fn live(&self, thread: &str) -> Option<&Running> {
        self.turns.get(thread)
    }

    fn fold(&mut self, thread: &str, event: TurnEvent, cx: &mut Context<Self>) {
        let Some(running) = self.turns.get_mut(thread) else {
            return;
        };
        luma_lib::agent::apply(&mut running.transcript, &event);
        cx.emit(RunningEvent::Event {
            thread: thread.to_owned(),
            event,
        });
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

    #[gpui::test]
    fn one_turn_per_thread(cx: &mut TestAppContext) {
        let running = cx.new(|_| RunningTurns::default());
        let first = start(&running, "a", cx).unwrap();
        assert!(start(&running, "a", cx).is_err());
        cx.run_until_parked();
        assert!(!first.is_closed(), "a second send cancelled the first turn");
    }
}
