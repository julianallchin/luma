//! The render engine: installing a score as the active scene, sampling it,
//! recording it to video, and render telemetry.

use std::future::Future;

use serde_json::Value;

use luma_lib::dispatch::{handlers, CommandError};
use luma_lib::models::universe::UniverseState;

use super::{Library, LibraryError};

impl Library {
    pub(crate) fn composite_score_document(
        &self,
        score_id: &str,
        score: &luma_patterns::Score,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (score_id, score) = (score_id.to_owned(), score.clone());
        self.call("composite_track", |services| async move {
            handlers::compositor::composite_track(&services, score_id, Some(score)).await
        })
    }

    /// Install one score as the render engine's active scene, so
    /// [`Self::sample_universe`] has something to sample.
    ///
    /// **The score is the subject, not the `(track, venue)` pair.** A pair
    /// carries as many scores as there are people who annotated it, and the rig
    /// shows the one that is open rather than a blend of all of them — which is
    /// why this takes the id the editor holds and does not look one up.
    ///
    /// The score's persisted rows are what gets compiled — the right answer
    /// for a screen that is only *watching* it, which has no working copy to
    /// offer. An editor whose live edits must reach the rig before the
    /// trailing write does sends them with
    /// [`Self::composite_score_document`] instead.
    pub fn composite_score(
        &self,
        score_id: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let score_id = score_id.to_owned();
        self.call("composite_track", |services| async move {
            handlers::compositor::composite_track(&services, score_id, None).await
        })
    }

    /// The installed scene evaluated at one absolute track time, or `None`
    /// when no scene is installed.
    ///
    /// Synchronous, unlike everything else here, and deliberately: this is read
    /// at paint time, where an awaited answer is a frame late and a frame late
    /// is a beam that lags the music. `Scene::render` is pure in `t`, so there
    /// is no ordering or warmup contract to violate by calling it from a
    /// painter — see `eval::scene`.
    pub fn sample_universe(&self, t: f32) -> Option<UniverseState> {
        self.services.render_engine().sample(t)
    }

    /// Render `spec` to a video — the recording `luma-cli record` makes, from the
    /// app. `progress` hears every frame written; setting `cancel` stops at the
    /// next frame.
    pub(crate) fn record<P>(
        &self,
        spec: luma_lib::recording::Recording,
        cancel: luma_lib::recording::CancelFlag,
        progress: P,
    ) -> impl Future<Output = Result<luma_lib::recording::Recorded, LibraryError>> + use<P>
    where
        P: Fn(luma_lib::recording::Progress) + Send + 'static,
    {
        self.call("record", move |services| async move {
            luma_lib::recording::record(&services, spec, cancel, progress)
                .await
                .map_err(|error| CommandError::Internal(error.to_string()))
        })
    }

    /// Append one entry to the render telemetry log, and forget about it.
    ///
    /// Returns nothing, deliberately. This is called from the stage when a
    /// frame took too long, and a report that made the next frame wait on a
    /// file write would be reporting on a hitch it had just widened. The
    /// command is spawned on the Tokio runtime before this returns (see
    /// [`Self::task`]), so dropping the future still runs it — and a
    /// telemetry write that fails is not a fact any screen should act on.
    pub fn record_telemetry(&self, entry: Value) {
        drop(self.call("append_render_telemetry", |services| async move {
            handlers::telemetry::append_render_telemetry(&services, entry).await
        }));
    }
}
