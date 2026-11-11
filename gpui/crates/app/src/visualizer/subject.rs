//! What the stage shows: the room and score resolved from the workspace,
//! and the `Luma` transitions that build, relight or drop the stage.
use super::*;

// -- the screen --------------------------------------------------------------

/// What the stage should be showing, resolved from the shell's current subject.
///
/// A named triple rather than a bare tuple: the two `String`s are the same
/// type as each other, which is exactly when positional returns start getting
/// swapped by accident.
struct StageSubject {
    pub(crate) venue_id: String,
    venue_name: String,
    /// The score that lights the rig, when one does.
    lit: Option<Lit>,
    audition: Option<crate::track_editor::Audition>,
    bass: Option<Bass>,
    /// The page under the stage is the Rig page.
    rigging: bool,
}

/// The score a stage is lit by.
///
/// The id is the answer to *which document*; the ordinal is only how a person
/// names it, carried alongside so stage automation can identify it without a
/// second read. Compared whole rather than by id, so a renumbering — a
/// sibling score deleted out from under this one — refreshes the readout
/// instead of leaving it naming a position the sidebar no longer uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Lit {
    pub(crate) score: String,
    pub(crate) ordinal: i64,
}

/// The 3D view's transitions, kept with the screen the way every other one is
/// (`settings::open_settings`, `track_editor::open_score`), so `lib.rs` stays
/// the list of what exists rather than the list of how each screen is reached.
impl Luma {
    /// The room the stage should be showing this frame, and the score that
    /// should light it.
    ///
    /// **The room comes from the venue on screen, not from the visible tab.**
    /// Every tab in a venue's set is worked on against that room, so switching
    /// tabs leaves the stage exactly where it was. Only the *lighting* asks the
    /// tab, because only a score tab has a score to composite.
    fn stage_subject(&self) -> Option<StageSubject> {
        // A hidden pane drops the stage rather than merely leaving it undrawn,
        // so a collapsed pane does not keep a GPU and a loaded rig alive
        // behind it.
        if self.visualizer_hidden {
            return None;
        }
        // Nothing open below it is nothing to be a view *of*. A venue picked in
        // the sidebar is a browser, not a room being worked on, and raising a
        // stage over it loads that venue's whole rig and builds its scene for a
        // surface nobody asked for — a cost the browser pays on every venue
        // click, in the app as much as in a test.
        if self.workspace.is_empty() {
            return None;
        }
        let venue_id = self.venues.current()?.to_string();
        // The *score*, not the pair it sits on. A `(track, venue)` carries as
        // many scores as there are people who annotated it, and the editor is
        // the one thing that knows which of them is open — so the stage takes
        // that answer rather than looking one up and disagreeing.
        let lit = match self.workspace.active_body() {
            Some(Body::TrackEditor(state)) if state.venue_id() == venue_id => Some(state.lit()),
            Some(Body::TrackEditor(_) | Body::Patch(_) | Body::Perform(_)) | None => None,
        };
        let name = self
            .songs
            .as_ref()
            .filter(|browser| browser.venue_id() == venue_id)
            .map_or_else(
                || venue_id.clone(),
                |browser| browser.venue_name().to_string(),
            );
        let (audition, bass) = match self.workspace.active_body() {
            Some(Body::TrackEditor(state)) if state.venue_id() == venue_id => {
                (state.audition(), self.library.track_audio(state.track_id()))
            }
            _ => (None, None),
        };
        Some(StageSubject {
            venue_id,
            venue_name: name,
            lit,
            audition,
            bass,
            rigging: matches!(self.workspace.active_body(), Some(Body::Patch(_))),
        })
    }

    /// Keep the stage pane pointed at the tab below it: build it when a room
    /// appears, re-light it when only the score changed, drop it when the
    /// workspace has nothing about a room.
    ///
    /// Done at draw rather than at every navigation for the reason
    /// [`Luma::sync_venue_modes`] is: a navigation is a field assignment, and a
    /// gesture that forgot to ask would leave the stage lighting a room
    /// nothing on screen is about. Comparing the venue *and* the score is what
    /// separates the two costs — a different room rebuilds, a different track
    /// only re-composites (see [`Visualizer::relight`]).
    pub(crate) fn sync_visualizer(&mut self, cx: &mut Context<Self>) {
        let Some(StageSubject {
            venue_id,
            venue_name,
            lit: subject,
            audition,
            bass,
            rigging,
        }) = self.stage_subject()
        else {
            // Dropping the state is what un-mounts the viewport, and
            // un-mounting is what stops its continuous redraw — see the
            // rendering note on [`visualizer`].
            if self.visualizer.take().is_some() {
                cx.notify();
            }
            return;
        };
        let exporting = self.exporting_show();
        // Split the borrow: both arms read the library and mutate the stage,
        // and the two fields are disjoint. The same split `shell::active_tab`
        // takes for the same reason.
        let Luma {
            visualizer,
            library,
            saved_cameras,
            ..
        } = self;
        match visualizer {
            Some(state) if state.venue_id == venue_id => {
                if state.subject != subject {
                    state.relight(library, subject, cx);
                    cx.notify();
                }
            }
            _ => {
                let mut opened = Visualizer::open(library, &venue_id, venue_name, subject, cx);
                opened.restore_pose(saved_cameras.value().pose(&venue_id));
                *visualizer = Some(opened);
                cx.notify();
            }
        }
        if let Some(state) = visualizer {
            state.audition = audition;
            state.bass = bass;
            state.exporting = exporting;
            if state.rigging != rigging {
                state.set_rigging(rigging);
                cx.notify();
            }
        }
        self.sync_camera_tour(cx);
    }

    /// Give the stage's room back to the editor under it, or take it again.
    ///
    /// Hiding **drops** the stage rather than merely leaving it undrawn: an
    /// off-screen viewport holding a GPU and a loaded rig is the cost this
    /// pane most needs not to have, and there is nothing to preserve — which
    /// room it shows is re-derived from the workspace the moment it returns,
    /// and the camera opens where it was last left — see
    /// [`crate::saved_cameras`].
    pub(crate) fn toggle_visualizer(&mut self, cx: &mut Context<Self>) {
        self.visualizer_hidden = !self.visualizer_hidden;
        cx.notify();
    }

    /// The stage's state, when one is mounted. Every pointer handler and
    /// toolbar button goes through here, so none of them can act on a stage
    /// that is not on screen.
    pub(crate) fn visualizer_mut(&mut self) -> Option<&mut Visualizer> {
        self.visualizer.as_mut()
    }
}
