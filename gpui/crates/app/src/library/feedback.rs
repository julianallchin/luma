//! A person's verdicts on what an agent made: one per turn, one per clip.

use std::future::Future;

use luma_lib::database::local::feedback::{Feedback, Target, Verdict};
use luma_lib::dispatch::handlers;

use super::{Library, LibraryError};

impl Library {
    /// Record the signed-in account's verdict on `target`; `None` takes it
    /// back. Queued behind the last write, so two presses in a row are stored
    /// in the order they were made.
    pub fn set_feedback(
        &self,
        target: Target,
        verdict: Option<Verdict>,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        self.ordered_write("set_feedback", move |services| async move {
            handlers::feedback::set_feedback(&services, target, verdict, None).await
        })
    }

    /// The signed-in account's verdicts on a thread's turns.
    pub fn thread_feedback(
        &self,
        thread_id: &str,
    ) -> impl Future<Output = Result<Vec<Feedback>, LibraryError>> + use<> {
        let thread_id = thread_id.to_owned();
        self.call("thread_feedback", |services| async move {
            handlers::feedback::thread_feedback(&services, thread_id).await
        })
    }

    /// The signed-in account's verdicts on a score's clips.
    pub fn score_clip_feedback(
        &self,
        score_id: &str,
    ) -> impl Future<Output = Result<Vec<Feedback>, LibraryError>> + use<> {
        let score_id = score_id.to_owned();
        self.call("score_clip_feedback", |services| async move {
            handlers::feedback::score_clip_feedback(&services, score_id).await
        })
    }
}
