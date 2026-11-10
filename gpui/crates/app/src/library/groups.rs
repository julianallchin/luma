//! A venue's fixture groups and the selections written over them: group
//! edits, and resolving a selection to heads.

use std::future::Future;

use luma_lib::dispatch::handlers;
use luma_lib::models::groups::FixtureGroup;
use luma_lib::models::selection::Selection;
use luma_lib::models::universe::UniverseState;

use super::{Library, LibraryError};

impl Library {
    pub fn delete_venue_group(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let id = id.to_owned();
        self.call("delete_group", |services| async move {
            handlers::groups::delete_group(&services, id).await
        })
    }
    pub fn generate_venue_groups(
        &self,
        venue: &str,
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let venue_id = venue.to_owned();
        self.call("generate_venue_groups", |services| async move {
            handlers::groups::generate_venue_groups(&services, venue_id).await
        })
    }
    /// Commit a venue group name and membership changes atomically.
    pub fn save_venue_group(
        &self,
        venue: &str,
        id: Option<&str>,
        label: &str,
        added: &[String],
        removed: &[String],
    ) -> impl Future<Output = Result<(), LibraryError>> + use<> {
        let (venue_id, group_id, label) =
            (venue.to_owned(), id.map(str::to_owned), label.to_owned());
        let (added, removed) = (added.to_vec(), removed.to_vec());
        self.call("save_venue_group", |services| async move {
            handlers::groups::save_venue_group(&services, venue_id, group_id, label, added, removed)
                .await
        })
    }

    /// A venue's fixture groups — what a selection chooses from, by id, and
    /// the names the strip's field completes.
    pub fn venue_groups(
        &self,
        venue_id: &str,
    ) -> impl Future<Output = Result<Vec<FixtureGroup>, LibraryError>> + use<> {
        let venue_id = venue_id.to_owned();
        self.call("list_groups", |services| async move {
            handlers::groups::list_groups(&services, venue_id).await
        })
    }

    /// The heads `selection` resolves to in the venue, with their places;
    /// `elements` is a laser clip's elements per projector.
    pub(crate) fn selection_cells(
        &self,
        venue_id: &str,
        selection: &Selection,
        elements: Option<usize>,
    ) -> impl Future<Output = Result<Vec<luma_patterns::Cell>, LibraryError>> + use<> {
        let (venue_id, selection) = (venue_id.to_owned(), selection.clone());
        self.call("selection_cells", move |services| async move {
            handlers::composable_patterns::selection_cells(&services, venue_id, selection, elements)
                .await
        })
    }

    /// The frame that answers "which heads does this selection light?" — every
    /// matched head open and white, the rest of the rig dark.
    ///
    /// The picker draws this over the venue's own geometry, so the picture a
    /// person points at and the fixtures a clip will drive are resolved by one
    /// evaluator, head by head.
    pub fn highlight_selection(
        &self,
        venue_id: &str,
        selection: &Selection,
    ) -> impl Future<Output = Result<UniverseState, LibraryError>> + use<> {
        let (venue_id, selection) = (venue_id.to_owned(), selection.clone());
        self.call("highlight_selection", |services| async move {
            handlers::groups::highlight_selection(&services, venue_id, selection).await
        })
    }
}
