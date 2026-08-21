//! A provider that reads from a script instead of a network.
//!
//! Compiled only for tests and for hosts that opt into the `scripted-model`
//! feature (the GPUI harness), so the shipped app cannot accidentally hold one.

#![cfg(any(test, feature = "scripted-model"))]

use std::collections::VecDeque;
use std::sync::Mutex;

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
}

impl ScriptedModel {
    #[must_use]
    pub fn new(steps: Vec<Vec<ModelEvent>>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
            seen: Mutex::new(Vec::new()),
        }
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
        stream::iter(events.into_iter().map(Ok)).boxed()
    }
}
