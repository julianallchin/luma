//! A provider that reads from a script instead of a network.
//!
//! Compiled only for tests and for hosts that opt into the `scripted-model`
//! feature (the GPUI harness), so the shipped app cannot accidentally hold one.

#![cfg(any(test, feature = "scripted-model"))]

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use futures_util::stream::{self, BoxStream, StreamExt};

use super::{ModelClient, ModelError, ModelEvent, ModelRequest, StopReason, Usage};

/// One scripted turn: a queue of steps, each a list of events to emit.
///
/// A step that does not end with [`ModelEvent::StepEnded`] gets one appended
/// with [`StopReason::EndTurn`] — the loop's contract is that every step ends,
/// and making the script restate it would only invite a hang.
pub struct ScriptedModel {
    steps: Mutex<VecDeque<Vec<ModelEvent>>>,
    seen: Mutex<Vec<ModelRequest>>,
    /// Gap between emitted events. Zero by default, which is what a reducer
    /// test wants — it asserts on the folded result and a delay would only
    /// make it slow.
    cadence: Duration,
}

impl ScriptedModel {
    #[must_use]
    pub fn new(steps: Vec<Vec<ModelEvent>>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
            seen: Mutex::new(Vec::new()),
            cadence: Duration::ZERO,
        }
    }

    /// Emit events this far apart, so a *surface* test can observe a turn
    /// half-finished.
    ///
    /// Without it the whole script is ready in one poll and the transcript goes
    /// from empty to complete inside a single frame — a UI that only ever
    /// renders the final state, which is precisely the state a streaming bug
    /// hides behind. Opt-in rather than a default so the reducer tests, which
    /// assert on the fold and not on any frame, stay instant.
    #[must_use]
    pub fn with_cadence(mut self, cadence: Duration) -> Self {
        self.cadence = cadence;
        self
    }

    /// Every request the loop has made so far, in order — this is how a test
    /// asserts on rehydration without a network.
    #[must_use]
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.seen.lock().expect("poisoned").clone()
    }
}

impl ModelClient for ScriptedModel {
    fn stream(&self, request: ModelRequest) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        self.seen.lock().expect("poisoned").push(request);
        let mut events = self
            .steps
            .lock()
            .expect("poisoned")
            .pop_front()
            .unwrap_or_default();
        if !matches!(events.last(), Some(ModelEvent::StepEnded { .. })) {
            events.push(ModelEvent::StepEnded {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            });
        }
        let cadence = self.cadence;
        stream::iter(events.into_iter().map(Ok))
            .then(move |event| async move {
                if !cadence.is_zero() {
                    tokio::time::sleep(cadence).await;
                }
                event
            })
            .boxed()
    }
}
