//! The track editor's scores: listing, creating and renaming them, reading
//! and saving a score document, and previewing its clips and definitions.

use std::future::Future;

use luma_lib::dispatch::handlers;
use luma_lib::error::CommandError;
use luma_lib::models::beat_grid::BeatGrid;
use luma_lib::models::scores::{Score, ScoreSummary};
use luma_lib::services::graph_scores::ClipStrip;

use super::{refused, Library, LibraryError};

/// Everything the native timeline needs to draw a score: the document itself
/// and the grid its beat positions are read in.
pub(crate) struct ScoreContents {
    pub score: luma_patterns::Score,
    pub beats: Option<BeatGrid>,
}

impl Library {
    pub(crate) fn preview_definition_frames(
        &self,
        request: luma_lib::models::composable_patterns::ComposablePreviewRequest,
    ) -> impl Future<
        Output = Result<luma_lib::models::composable_patterns::ComposablePreview, LibraryError>,
    > + use<> {
        self.call("preview_composable_pattern", |services| async move {
            handlers::composable_patterns::preview_composable_pattern(&services, request).await
        })
    }

    // -- the track editor -----------------------------------------------------

    /// Every score for this track the current admission may see, in any
    /// venue, newest first. What the editor's score rail lists: the venue in
    /// hand is one grouping of this, not a separate read.
    pub fn scores_across_venues(
        &self,
        track_id: &str,
    ) -> impl Future<Output = Result<Vec<ScoreSummary>, LibraryError>> + use<> {
        let track_id = track_id.to_owned();
        self.call("list_scores_across_venues", |services| async move {
            handlers::scores::list_scores_across_venues(&services, track_id).await
        })
    }

    /// Create another score for a track. Multiple scores per track/venue are
    /// intentional; use [`Self::ensure_track_in_venue`] for the Add action.
    pub fn create_score(
        &self,
        track_id: &str,
        venue_id: &str,
        name: Option<&str>,
    ) -> impl Future<Output = Result<Score, LibraryError>> + use<> {
        let (track_id, venue_id) = (track_id.to_owned(), venue_id.to_owned());
        let name = name.map(str::to_owned);
        self.call("create_score", |services| async move {
            handlers::scores::create_score(&services, track_id, venue_id, name).await
        })
    }

    /// Delete a score, and with it every clip and definition row under it.
    pub fn delete_score(&self, id: &str) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let id = id.to_owned();
        self.call("delete_score", |services| async move {
            handlers::scores::delete_score(&services, id).await
        })
    }

    /// Rename a score. The backend trims the name and refuses an empty one.
    pub fn rename_score(
        &self,
        score_id: &str,
        name: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (score_id, name) = (score_id.to_owned(), name.to_owned());
        self.call("rename_score", |services| async move {
            handlers::scores::rename_score(&services, score_id, name).await
        })
    }

    /// Copy a score and its clips into a new score of the same song in the
    /// same venue, named `name`.
    pub fn duplicate_score(
        &self,
        score_id: &str,
        name: &str,
    ) -> impl Future<Output = Result<Score, LibraryError>> + use<> {
        let (score_id, name) = (score_id.to_owned(), name.to_owned());
        self.call("duplicate_score", |services| async move {
            handlers::scores::duplicate_score(&services, score_id, name).await
        })
    }

    /// Atomically add a track to a venue. Repeated Add actions return the
    /// existing score.
    pub fn ensure_track_in_venue(
        &self,
        track_id: &str,
        venue_id: &str,
        name: Option<&str>,
    ) -> impl Future<Output = Result<Score, LibraryError>> + use<> {
        let (track_id, venue_id) = (track_id.to_owned(), venue_id.to_owned());
        let name = name.map(str::to_owned);
        self.call("ensure_venue_score", |services| async move {
            handlers::scores::ensure_venue_score(&services, track_id, venue_id, name).await
        })
    }

    pub(crate) fn score_contents(
        &self,
        score_id: &str,
        track_id: &str,
    ) -> impl Future<Output = Result<ScoreContents, LibraryError>> + use<> {
        #[cfg(feature = "agent")]
        self.navigation_fixture
            .lock()
            .unwrap()
            .score_reads
            .notify_waiters();
        let (score_id, track_id) = (score_id.to_owned(), track_id.to_owned());
        self.task("get_score_document", |services| async move {
            let score = handlers::scores::get_score_document(&services, score_id.clone())
                .await
                .and_then(|score| {
                    score.ok_or_else(|| CommandError::NotFound("score does not exist".into()))
                })
                .map_err(refused("get_score_document"))?;
            let beats = handlers::tracks::get_track_beats(&services, track_id, Some(score_id))
                .await
                .map_err(refused("get_track_beats"))?;
            Ok(ScoreContents { score, beats })
        })
    }

    /// Save the editor's changes against the score it read. A clip the edit
    /// does not name keeps whatever storage holds now, so an agent's write
    /// that landed since the read survives.
    pub(crate) fn edit_score_document(
        &self,
        score_id: &str,
        edit: &luma_patterns::ScoreEdit,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (score_id, edit) = (score_id.to_owned(), edit.clone());
        #[cfg(feature = "agent")]
        let outside = self.outside_score_edit(&score_id);
        self.task("edit_score_document", |services| async move {
            #[cfg(feature = "agent")]
            if let Some(outside) = outside {
                outside.await?;
            }
            handlers::scores::edit_score_document(&services, score_id, edit)
                .await
                .map_err(refused("edit_score_document"))
        })
    }

    /// Each clip's strip, alone on the venue.
    pub(crate) fn preview_score_clips(
        &self,
        score_id: &str,
        clip_ids: Vec<String>,
        score: luma_patterns::Score,
    ) -> impl Future<Output = Result<Vec<ClipStrip>, LibraryError>> + use<> {
        let score_id = score_id.to_owned();
        self.call("preview_score_clips", |services| async move {
            handlers::scores::preview_score_clips(&services, score_id, clip_ids, Some(score)).await
        })
    }

    pub(crate) fn prepare_score_clip_preview(
        &self,
        score_id: &str,
        clip_id: &str,
        score: &luma_patterns::Score,
    ) -> impl Future<Output = Result<luma_lib::services::graph_scores::ClipPreview, LibraryError>> + use<>
    {
        let (score_id, clip_id, score) = (score_id.to_owned(), clip_id.to_owned(), score.clone());
        self.call("prepare_score_clip_preview", |services| async move {
            handlers::scores::prepare_score_clip_preview(&services, score_id, clip_id, score).await
        })
    }
}
