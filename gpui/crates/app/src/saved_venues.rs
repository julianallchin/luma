//! Each venue's window state, kept on this device so a launch comes back to
//! it: the mode on screen, the open score, and the chat each mode had open.
//!
//! The live and parked [`crate::workspace::Modes`] stay the one source of
//! truth: what is saved is a [`Snapshot`] taken from them at draw, and what is
//! restored goes back in through the same openers a click uses. A session item
//! in the shared [`crate::session_store`]; nothing here is synced.
//!
//! A venue's state is restored the first time it is opened in a launch: its
//! mode and its Rig and Live bodies at once, its score once the catalogue that
//! holds the score's track has landed. Until then the venue waits in
//! [`SavedVenues::unopened`] and is written back unchanged, so visiting one
//! venue does not forget another's.

use std::collections::{BTreeMap, HashMap};

use gpui::{App, Context};
use serde::{Deserialize, Serialize};

use crate::session_store::Stored;
use crate::workspace::{Mode, Target};
use crate::Luma;

/// The session item the snapshot is stored under.
const KEY: &str = "venue-modes";

/// Every venue's state, as stored.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct Snapshot {
    venues: BTreeMap<String, SavedVenue>,
}

/// One venue: the mode on screen and each open body.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct SavedVenue {
    mode: Mode,
    bodies: Vec<SavedBody>,
}

/// One body, and the chat it had open.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct SavedBody {
    target: Target,
    thread: Option<String>,
}

pub(crate) struct SavedVenues {
    stored: Stored<Snapshot>,
    /// Venues read from the store whose state this launch has not restored.
    unopened: BTreeMap<String, SavedVenue>,
}

impl Default for SavedVenues {
    fn default() -> Self {
        Self {
            stored: Stored::new(KEY),
            unopened: BTreeMap::new(),
        }
    }
}

impl SavedVenues {
    /// Take in what the store held at launch.
    pub(crate) fn read(stored: Option<&str>) -> Self {
        let stored = Stored::<Snapshot>::read(KEY, stored);
        Self {
            unopened: stored.value().venues.clone(),
            stored,
        }
    }

    /// Whether `venue_id`'s saved score is still waiting for its catalogue —
    /// while it is, Arrange must not offer the song list as if nothing were
    /// open.
    pub(crate) fn restoring(&self, venue_id: &str) -> bool {
        self.unopened.contains_key(venue_id)
    }

    /// Drop `venue_id`'s saved score before it is restored: the window is
    /// arriving for another one.
    pub(crate) fn forget_score(&mut self, venue_id: &str) {
        if let Some(saved) = self.unopened.get_mut(venue_id) {
            saved
                .bodies
                .retain(|body| !matches!(body.target, Target::Score { .. }));
        }
    }
}

impl Luma {
    /// The store's value, for [`SavedVenues::read`].
    pub(crate) fn read_saved_venues(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<String>, crate::LibraryError>> + use<>
    {
        self.library.get_session_item(KEY)
    }

    /// Every venue's state as it stands: the live and parked ones, and the
    /// venues not restored yet.
    fn venue_snapshot(&self, cx: &App) -> Snapshot {
        let mut venues = self.saved_venues.unopened.clone();
        for (venue, modes) in self.venues.sets(&self.workspace) {
            // Half restored: what was saved is still the truth until its
            // score is back.
            if self.saved_venues.unopened.contains_key(venue) {
                continue;
            }
            venues.insert(
                venue.to_string(),
                SavedVenue {
                    mode: modes.mode(),
                    bodies: modes
                        .iter()
                        .map(|slot| SavedBody {
                            target: slot.target.clone(),
                            thread: slot
                                .body
                                .chat()
                                .panel
                                .read(cx)
                                .thread_id()
                                .map(str::to_string),
                        })
                        .collect(),
                },
            );
        }
        Snapshot { venues }
    }

    /// Write every venue's state when it changed. Called every frame; a burst
    /// of changes is written once, after it settles.
    pub(crate) fn save_venues(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.venue_snapshot(cx);
        self.store_session(|this| &mut this.saved_venues.stored, snapshot, cx);
    }

    /// Put `venue_id`'s saved mode back on screen and reopen its Rig and Live
    /// bodies, each with its chat. Called as the venue opens; its score waits
    /// for [`Luma::restore_venue_score`].
    pub(crate) fn restore_venue_modes(&mut self, venue_id: &str, cx: &mut Context<Self>) {
        let Some(saved) = self.saved_venues.unopened.get(venue_id).cloned() else {
            return;
        };
        for body in &saved.bodies {
            let thread = body.thread.as_deref();
            match &body.target {
                Target::Venue { .. } => self.open_rig(thread, cx),
                Target::Perform { .. } => self.open_live(thread, cx),
                Target::Score { .. } => {}
            }
        }
        self.workspace.set_mode(saved.mode);
        cx.notify();
    }

    /// Reopen `venue_id`'s saved score with its chat, once the venue's
    /// catalogue has landed: a score needs its track's row, and a score whose
    /// track or score no longer exists is dropped without a word.
    pub(crate) fn restore_venue_score(&mut self, venue_id: &str, cx: &mut Context<Self>) {
        let Some(saved) = self.saved_venues.unopened.get(venue_id) else {
            return;
        };
        let Some(songs) = &self.songs else {
            return;
        };
        let score = saved.bodies.iter().find_map(|body| match &body.target {
            Target::Score { track, score, .. } if songs.find(track).is_some() => {
                Some((track.clone(), score.clone(), body.thread.clone()))
            }
            _ => None,
        });
        let Some((track, score, thread)) = score else {
            self.saved_venues.unopened.remove(venue_id);
            cx.notify();
            return;
        };
        let listing = self.library.scores_across_venues(&track);
        let generation = songs.load_generation();
        let user = self.library.user_id();
        let venue = venue_id.to_string();
        cx.spawn(async move |this, cx| {
            let mut found = HashMap::new();
            if let Ok(summaries) = listing.await {
                for row in crate::tracks::scores::rows(&summaries, user.as_deref()).iter() {
                    if row.venue_id.as_ref() == venue {
                        found.insert(row.id.to_string(), crate::tracks::scores::open_row(row));
                    }
                }
            }
            this.update(cx, |this, cx| {
                // The reader left the venue while the listing was out: its
                // score waits for the next visit.
                let here = this.songs.as_ref().is_some_and(|songs| {
                    songs.venue_id() == venue && songs.load_generation() == generation
                });
                if !here {
                    return;
                }
                if this.saved_venues.unopened.remove(&venue).is_none() {
                    return;
                }
                this.sync_venue_modes(cx);
                if let Some(open) = found.remove(&score) {
                    let mode = this.workspace.mode();
                    this.open_score(&track, open, thread.as_deref(), cx);
                    // Opening a score shows Arrange; the mode on screen wins.
                    this.workspace.set_mode(mode);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_round_trips_through_its_stored_form() {
        let mut venues = BTreeMap::new();
        venues.insert(
            "club".to_string(),
            SavedVenue {
                mode: Mode::Live,
                bodies: vec![
                    SavedBody {
                        target: Target::Score {
                            venue: "club".into(),
                            track: "song".into(),
                            score: "main".into(),
                        },
                        thread: Some("thread".into()),
                    },
                    SavedBody {
                        target: Target::Perform {
                            venue: "club".into(),
                        },
                        thread: None,
                    },
                ],
            },
        );
        let snapshot = Snapshot { venues };
        let json = serde_json::to_string(&snapshot).unwrap();
        let read = SavedVenues::read(Some(&json));
        assert_eq!(read.stored.value(), &snapshot);
        assert_eq!(read.unopened, snapshot.venues);
        assert!(read.restoring("club"));
        assert!(!read.restoring("hall"));
    }
}
