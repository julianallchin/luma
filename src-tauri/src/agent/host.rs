//! The adapter for a host that cannot hold a [`super::TurnStream`].
//!
//! A webview can only receive JSON off an event bus, so *it* pays for the
//! serialization: a turn started through the seam is driven by a task here,
//! which drains the typed stream into `Events::emit("agent-turn", …)`. A host
//! that can hold the stream — GPUI, a CLI, a test — calls
//! [`AgentService::turn`] directly and pays nothing.
//!
//! The registry exists for the same reason: a webview has no handle to drop, so
//! cancellation and steering have to be addressable by thread id. Neither the
//! loop nor the typed path knows this file exists.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use futures_util::StreamExt;
use serde_json::json;

use super::{AgentError, AgentService, TurnSteer, UserPrompt};
use crate::dispatch::AppServices;

/// The event name every turn delta is broadcast under, for the one host that
/// needs a broadcast.
pub const TURN_EVENT: &str = "agent-turn";

struct Running {
    turn_id: String,
    steer: TurnSteer,
    task: tokio::task::JoinHandle<()>,
}

/// Turns started through the dispatch seam, keyed by thread.
///
/// Held by [`AppServices`]; a host that never calls
/// [`AppServices::into_shared`] has no shared handle to spawn against and the
/// seam commands say so rather than silently doing nothing.
#[derive(Default)]
pub struct TurnRegistry {
    services: OnceLock<Weak<AppServices>>,
    running: Mutex<HashMap<String, Running>>,
}

impl TurnRegistry {
    /// Record the shared services handle a spawned turn will run against.
    pub(crate) fn attach(&self, services: &Arc<AppServices>) {
        let _ = self.services.set(Arc::downgrade(services));
    }

    fn shared(&self) -> Result<Arc<AppServices>, AgentError> {
        self.services.get().and_then(Weak::upgrade).ok_or_else(|| {
            AgentError::Invalid(
                "this host has no shared services handle; build it with AppServices::into_shared"
                    .into(),
            )
        })
    }

    /// Start a turn and stream it onto the event bus. Returns the turn id the
    /// emitted events carry.
    ///
    /// # Errors
    ///
    /// [`AgentError::Invalid`] if the host installed no shared handle, or a
    /// turn is already running for this thread — one thread, one turn.
    pub fn start(&self, thread_id: &str, prompt: UserPrompt) -> Result<String, AgentError> {
        let services = self.shared()?;
        let mut running = self.running.lock().expect("poisoned");
        if let Some(existing) = running.get(thread_id) {
            if !existing.task.is_finished() {
                return Err(AgentError::Invalid(format!(
                    "thread '{thread_id}' already has a turn in flight"
                )));
            }
        }

        let turn_id = uuid::Uuid::new_v4().to_string();
        let mut stream = AgentService::new(Arc::clone(&services)).turn(thread_id, prompt);
        let steer = stream.steering();
        let events = services.events().clone();
        let thread = thread_id.to_string();
        let id = turn_id.clone();
        let task = tokio::spawn(async move {
            while let Some(event) = stream.next().await {
                events.emit(
                    TURN_EVENT,
                    json!({ "threadId": thread, "turnId": id, "event": event }),
                );
            }
        });
        running.insert(
            thread_id.to_string(),
            Running {
                turn_id: turn_id.clone(),
                steer,
                task,
            },
        );
        Ok(turn_id)
    }

    /// Cancel the thread's running turn. `false` when there was none.
    ///
    /// Aborting the task drops the stream, which is the same cancellation the
    /// typed path gets for free.
    pub fn cancel(&self, thread_id: &str) -> bool {
        let Some(running) = self.running.lock().expect("poisoned").remove(thread_id) else {
            return false;
        };
        running.task.abort();
        true
    }

    /// Redirect the thread's running turn.
    ///
    /// # Errors
    ///
    /// [`AgentError::Invalid`] if no turn is running for this thread.
    pub fn steer(&self, thread_id: &str, message: String) -> Result<(), AgentError> {
        let running = self.running.lock().expect("poisoned");
        let Some(running) = running.get(thread_id) else {
            return Err(AgentError::Invalid(format!(
                "no turn is running for thread '{thread_id}'"
            )));
        };
        running.steer.send(message);
        Ok(())
    }

    /// The turn id currently attached to a thread, if any.
    #[must_use]
    pub fn turn_id(&self, thread_id: &str) -> Option<String> {
        self.running
            .lock()
            .expect("poisoned")
            .get(thread_id)
            .map(|running| running.turn_id.clone())
    }
}
