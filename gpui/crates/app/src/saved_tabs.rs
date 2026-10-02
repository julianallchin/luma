//! The open tabs, kept on this device so a launch reopens them
//! (`docs/specs/venue-tabs.md` rule 8).
//!
//! The tab sets themselves stay the one source of truth: what is saved is a
//! [`Snapshot`] taken from them at draw, and what is restored goes back in
//! through the same openers a click uses. Nothing here is synced — it is a
//! session item in this device's local store, beside the last venue.
//!
//! A venue's saved tabs are reopened the first time its catalogue lands in a
//! launch. Until then they wait in [`SavedTabs::unopened`] and are written back
//! unchanged, so visiting one venue does not forget another's tabs.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use gpui::{App, Context};
use serde::{Deserialize, Serialize};

use crate::tabs::Target;
use crate::Luma;

/// The session item the snapshot is stored under.
const KEY: &str = "open-tabs";

/// How long a change waits for the next one before it is written.
const SAVE_WAIT: Duration = Duration::from_millis(500);

/// Every venue's tabs, as stored.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct Snapshot {
    venues: BTreeMap<String, SavedSet>,
}

/// One venue's tabs in strip order, and which was in front.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct SavedSet {
    tabs: Vec<SavedTab>,
    front: Option<usize>,
}

/// One tab, and the chat it had open.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct SavedTab {
    target: Target,
    thread: Option<String>,
}

#[derive(Default)]
pub(crate) struct SavedTabs {
    /// Venues read from the store whose tabs this launch has not reopened.
    unopened: BTreeMap<String, SavedSet>,
    /// What the store holds, as last read or written. A frame whose snapshot
    /// matches it writes nothing.
    stored: Snapshot,
    /// Correlates a debounced write with the change that scheduled it.
    writes: u64,
}

impl SavedTabs {
    /// Take in what the store held at launch. A value that does not parse is
    /// treated as nothing saved: it is a convenience, not a document.
    pub(crate) fn read(stored: Option<&str>) -> Self {
        let snapshot: Snapshot = stored
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();
        Self {
            unopened: snapshot.venues.clone(),
            stored: snapshot,
            writes: 0,
        }
    }
}

impl Luma {
    /// The store's value, for [`SavedTabs::read`].
    pub(crate) fn read_saved_tabs(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<String>, crate::LibraryError>> + use<>
    {
        self.library.get_session_item(KEY)
    }

    /// Every venue's tabs as they stand: the live and parked sets, and the
    /// venues not reopened yet.
    fn tab_snapshot(&self, cx: &App) -> Snapshot {
        let mut venues = self.saved_tabs.unopened.clone();
        for (venue, tabs) in self.parked.sets(&self.workspace) {
            if tabs.is_empty() {
                continue;
            }
            let active = tabs.active();
            venues.insert(
                venue.to_string(),
                SavedSet {
                    tabs: tabs
                        .iter()
                        .map(|tab| SavedTab {
                            target: tab.target.clone(),
                            thread: tab
                                .body
                                .chat()
                                .panel
                                .read(cx)
                                .thread_id()
                                .map(str::to_string),
                        })
                        .collect(),
                    front: tabs.iter().position(|tab| Some(&tab.target) == active),
                },
            );
        }
        Snapshot { venues }
    }

    /// Write the tabs when they changed. Called every frame; a burst of
    /// changes is written once, after it settles.
    pub(crate) fn save_tabs(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.tab_snapshot(cx);
        if snapshot == self.saved_tabs.stored {
            return;
        }
        let Ok(json) = serde_json::to_string(&snapshot) else {
            return;
        };
        self.saved_tabs.stored = snapshot;
        self.saved_tabs.writes += 1;
        let generation = self.saved_tabs.writes;
        let wait = self.library.debounce(SAVE_WAIT);
        cx.spawn(async move |this, cx| {
            wait.await;
            let Ok(Some(write)) = this.update(cx, |this, _| {
                (this.saved_tabs.writes == generation)
                    .then(|| this.library.set_session_item(KEY, &json))
            }) else {
                return;
            };
            let _ = write.await;
        })
        .detach();
    }

    /// Reopen `venue_id`'s saved tabs, in strip order, with the front tab in
    /// front and each tab's chat. Called when the venue's catalogue lands:
    /// a score tab needs its track's row, and a tab whose track or score no
    /// longer exists is dropped without a word.
    pub(crate) fn restore_tabs(&mut self, venue_id: &str, cx: &mut Context<Self>) {
        let Some(set) = self.saved_tabs.unopened.get(venue_id) else {
            return;
        };
        let Some(browser) = &self.sidebar else {
            return;
        };
        let mut tracks: Vec<String> = Vec::new();
        for tab in &set.tabs {
            if let Target::Score { track, .. } = &tab.target {
                if browser.find(track).is_some() && !tracks.contains(track) {
                    tracks.push(track.clone());
                }
            }
        }
        let listings: Vec<_> = tracks
            .iter()
            .map(|track| self.library.scores_across_venues(track))
            .collect();
        let generation = browser.load_generation();
        let user = self.library.user_id();
        let venue = venue_id.to_string();
        cx.spawn(async move |this, cx| {
            let mut scores = HashMap::new();
            for listing in listings {
                let Ok(summaries) = listing.await else {
                    continue;
                };
                for row in crate::tracks::scores::rows(&summaries, user.as_deref()).iter() {
                    if row.venue_id.as_ref() == venue {
                        scores.insert(row.id.to_string(), crate::tracks::scores::open_row(row));
                    }
                }
            }
            this.update(cx, |this, cx| {
                // The reader left the venue while the listings were out: its
                // tabs wait for the next visit.
                let here = this.sidebar.as_ref().is_some_and(|browser| {
                    browser.venue_id() == venue && browser.load_generation() == generation
                });
                if !here {
                    return;
                }
                let Some(set) = this.saved_tabs.unopened.remove(&venue) else {
                    return;
                };
                this.sync_venue_tabs(cx);
                for tab in &set.tabs {
                    let thread = tab.thread.as_deref();
                    match &tab.target {
                        Target::Score { track, score, .. } => {
                            if let Some(found) = scores.get(score) {
                                this.open_score(track, found.clone(), thread, cx);
                            }
                        }
                        Target::Venue { .. } => this.open_venue_tab(thread, cx),
                    }
                }
                if let Some(front) = set.front.and_then(|index| set.tabs.get(index)) {
                    this.workspace.select(&front.target);
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
            SavedSet {
                tabs: vec![
                    SavedTab {
                        target: Target::Score {
                            venue: "club".into(),
                            track: "song".into(),
                            score: "main".into(),
                        },
                        thread: Some("thread".into()),
                    },
                    SavedTab {
                        target: Target::Venue {
                            venue: "club".into(),
                        },
                        thread: None,
                    },
                ],
                front: Some(1),
            },
        );
        let snapshot = Snapshot { venues };
        let json = serde_json::to_string(&snapshot).unwrap();
        let read = SavedTabs::read(Some(&json));
        assert_eq!(read.stored, snapshot);
        assert_eq!(read.unopened, snapshot.venues);
    }

    #[test]
    fn an_unreadable_value_is_nothing_saved() {
        let read = SavedTabs::read(Some("not json"));
        assert_eq!(read.stored, Snapshot::default());
        assert!(read.unopened.is_empty());
    }
}
