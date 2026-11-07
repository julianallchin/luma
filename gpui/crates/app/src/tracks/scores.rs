//! The song list's third column: the selected song's scores.
//!
//! A score is a `(track, venue)` document owned by one principal, and a pair
//! holds as many of them as there are principals who annotated it. One score
//! is open at a time: choosing one opens it in Arrange and closes the list.
//!
//! # Why a column
//!
//! Scores are *navigation*: choosing one is choosing what the timeline is a
//! view of, which is the same kind of act as choosing the track. So they sit
//! one column right of the track, the way a file sits right of its folder.
//!
//! # This venue's scores first; the others under their venue
//!
//! The listing is cross-venue ([`Library::scores_across_venues`]) because "the
//! same track, scored in the warehouse too" is the fact the operator cannot
//! otherwise find. Another venue's score opens the way the stage's score menu
//! opens one: after asking, by switching the window to that venue. Only this
//! venue's scores take the right-click menu and a rename.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use luma_ui::float;

use super::{column_row, Lit, ROW_INSET};
use luma_ui::glass;
use luma_ui::node::{Instrument, Role};
use luma_ui::text_input::{self, TextInput, DRAFT_CONTEXT};

use luma_lib::models::scores::ScoreSummary;
use luma_lib::models::tracks::TrackBrowserRow;

use crate::agent::ChatStatus;
use crate::workspace::Target;
use crate::Luma;

/// seam or the clock anything.
pub(crate) struct ScoreRow {
    pub(crate) id: SharedString,
    /// The seam's display ordinal within `(track, venue)` — `#2`. A handle,
    /// not a key: it is a rank by creation instant and it renumbers when an
    /// earlier score is deleted, which is why nothing here stores it.
    pub(crate) ordinal: i64,
    pub(crate) venue_id: SharedString,
    pub(crate) venue: SharedString,
    /// Whose score it is.
    author: SharedString,
    /// The score's own name, when it was given one. Most are unnamed, which
    /// is why the owner and not this is the row's leading word.
    name: Option<SharedString>,
    clips: i64,
    /// When it was last written, as an age — the same one-unit column the
    /// welcome screen reads in, from the one place it is written down. The
    /// authored timestamp when there is one: the score row's `updated_at`
    /// moves for reasons that are not authorship.
    age: SharedString,
    /// What the agent runs behind this score cost and spent, already
    /// formatted, or empty when nothing recorded a run against it. One field
    /// rather than two: the pair is always shown together and a score with a
    /// token count but no price is a run whose harness did not name one.
    spend: SharedString,
    /// Somebody else's score: openable, not writable. The same flag the editor
    /// computes for the open score, from the same comparison.
    pub(crate) read_only: bool,
}

/// Resolve a listing into rows. Order is the seam's — newest-created first —
/// and is preserved, so "which score is newest" is a property of the list and
/// not something this level re-decides per venue.
pub(crate) fn rows(summaries: &[ScoreSummary], user: Option<&str>) -> Rc<[ScoreRow]> {
    summaries
        .iter()
        .map(|score| {
            let read_only = score.uid.is_some() && score.uid.as_deref() != user;
            let author = if read_only {
                SharedString::from(short_uid(score.uid.as_deref().unwrap_or_default()))
            } else {
                SharedString::from("You")
            };
            ScoreRow {
                id: score.id.clone().into(),
                ordinal: score.ordinal,
                venue_id: score.venue_id.clone().unwrap_or_default().into(),
                venue: score
                    .venue_name
                    .clone()
                    .unwrap_or_else(|| "Unknown venue".into())
                    .into(),
                author,
                name: score.name.clone().map(SharedString::from),
                clips: score.annotation_count,
                age: crate::welcome::relative_age(
                    score
                        .last_authored_at
                        .as_deref()
                        .unwrap_or(&score.updated_at),
                )
                .into(),
                spend: spend(score.cost_usd, score.total_tokens).into(),
                read_only,
            }
        })
        .collect()
}

/// What a score's authoring runs cost, in the subtitle's own idiom: ` · $3.66 ·
/// 1.2M tok`, either half omitted when it is unknown, and the whole thing empty
/// when neither is.
///
/// Money keeps its cents because that is the unit the invoice is in; tokens are
/// abbreviated because nobody reads the last five digits of 1_243_881, and a
/// row that showed them would push the age off the end of the column.
fn spend(cost_usd: Option<f64>, total_tokens: i64) -> String {
    let mut out = String::new();
    if let Some(cost) = cost_usd {
        out.push_str(&format!(" · ${cost:.2}"));
    }
    match total_tokens {
        0 => {}
        n if n < 1_000 => out.push_str(&format!(" · {n} tok")),
        n if n < 1_000_000 => out.push_str(&format!(" · {:.1}k tok", n as f64 / 1e3)),
        n => out.push_str(&format!(" · {:.1}M tok", n as f64 / 1e6)),
    }
    out
}

/// A uid's first segment, which is what distinguishes two principals on a
/// screen with no directory to resolve them against.
fn short_uid(uid: &str) -> String {
    uid.chars().take(6).collect()
}

/// The selected song's scores: the track they are about, and the listing for
/// it.
///
/// The track is a *snapshot*, not an index into the page's rows: the filters
/// behind it can admit a different set while it is selected.
pub(crate) struct Scores {
    pub(crate) track: TrackBrowserRow,
    pub(crate) rows: Rc<[ScoreRow]>,
    /// Whether the listing has come back. Written in the same assignment as
    /// [`Self::rows`] and [`Self::error`], so "still loading" and "no scores"
    /// can never be confused for one another.
    pub(crate) loaded: bool,
    pub(crate) error: Option<String>,
    /// The keyboard's place in the column, an index into [`Self::items`].
    pub(crate) cursor: Option<usize>,
    /// The context menu a right-click raised, or none. Held here and not by
    /// the row: a row is rebuilt every frame from the listing, and the menu
    /// has to outlive that.
    pub(crate) menu: Option<ScoreMenu>,
    /// The score whose name is being typed, or none. One caret is what a
    /// person has.
    rename: Option<Rename>,
}

/// One line of the column the keyboard can stand on: a score, by its index
/// in [`Scores::rows`], or the way to mint another.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Item {
    Score(usize),
    New,
}

impl Scores {
    pub(crate) fn new(track: TrackBrowserRow) -> Self {
        Self {
            track,
            rows: Vec::new().into(),
            loaded: false,
            error: None,
            cursor: None,
            menu: None,
            rename: None,
        }
    }

    /// The column top to bottom, as the keyboard walks it: `venue`'s scores,
    /// New score, then every other venue's, grouped as [`elsewhere`] groups
    /// them. Empty until the listing has come back.
    pub(crate) fn items(&self, venue: &str) -> Vec<Item> {
        if !self.loaded {
            return Vec::new();
        }
        let index = |row: &ScoreRow| {
            self.rows
                .iter()
                .position(|other| other.id == row.id)
                .map(Item::Score)
        };
        self.rows
            .iter()
            .filter(|row| row.venue_id == venue)
            .filter_map(index)
            .chain(std::iter::once(Item::New))
            .chain(
                elsewhere(&self.rows, venue)
                    .flat_map(|(_, rows)| rows)
                    .filter_map(index),
            )
            .collect()
    }
}

/// An inline rename: the score, the live field, and what the name read when
/// the caret arrived. A commit that matches it writes nothing, so Enter and
/// blur can both commit without a second write.
struct Rename {
    score_id: SharedString,
    field: Entity<TextInput>,
    opened_on: String,
    _subscriptions: Vec<Subscription>,
}

/// Which score was right-clicked, where, and the two facts the delete gesture
/// needs about it.
///
/// A snapshot rather than an index into [`Scores::rows`]: the listing is
/// re-read whenever a score is minted or archived, and a menu holding a
/// position would be pointing at a different row by the time it was used.
pub(crate) struct ScoreMenu {
    /// Window space — that is what a right-click hands you, and what
    /// [`luma_ui::menu::ContextMenu`] hangs from.
    at: Point<Pixels>,
    score_id: SharedString,
    ordinal: i64,
    venue_id: SharedString,
    /// What the row said when it was clicked. The confirmation quotes it, so
    /// the sentence the operator reads is the number they were looking at.
    clips: i64,
}

impl ScoreRow {
    /// What the row is called: the score's name, or its owner when it has
    /// none.
    pub(crate) fn title(&self) -> SharedString {
        self.name.clone().unwrap_or_else(|| self.author.clone())
    }

    /// The line beside the title: clips, age and what the agent spent.
    pub(crate) fn detail(&self) -> SharedString {
        format!("{} patterns · {}{}", self.clips, self.age, self.spend).into()
    }
}

impl super::Tracks {
    /// The scores column, while it is `track_id`'s.
    fn scores_of(&self, track_id: &str) -> Option<&Scores> {
        self.scores
            .as_ref()
            .filter(|level| level.track.id == track_id)
    }

    fn scores_of_mut(&mut self, track_id: &str) -> Option<&mut Scores> {
        self.scores
            .as_mut()
            .filter(|level| level.track.id == track_id)
    }
}

// -- navigation and writes ----------------------------------------------------

impl Luma {
    /// Act on one line of the scores column: open a score of this venue on
    /// the spot, ask before taking the window to another venue's — the stage
    /// menu's own question, see [`Luma::ask_to_switch_venue`] — or mint a new
    /// one.
    pub(crate) fn choose_score_item(&mut self, track_id: &str, item: Item, cx: &mut Context<Self>) {
        let Some(songs) = &mut self.songs else {
            return;
        };
        let here = songs.venue_id().to_string();
        let Some(level) = songs.scores_of_mut(track_id) else {
            return;
        };
        let items = level.items(&here);
        level.cursor = items.iter().position(|other| *other == item);
        let row = match item {
            Item::New => return self.create_listed_score(track_id, cx),
            Item::Score(index) => level
                .rows
                .get(index)
                .map(|row| (row.venue_id.clone(), row.venue.clone(), open_row(row))),
        };
        let Some((venue_id, venue, score)) = row else {
            return;
        };
        if venue_id.as_ref() == here {
            self.open_listed_score(track_id.to_string().into(), score, cx);
        } else {
            self.ask_to_switch_venue(venue_id, venue, track_id, score, cx);
        }
    }

    /// Read `track_id`'s listing into the scores column.
    ///
    /// Read fresh on every selection rather than cached with the row: a score
    /// minted in another window is exactly the thing this exists to show.
    pub(crate) fn load_track_scores(&mut self, track_id: &str, cx: &mut Context<Self>) {
        let pending = self.library.scores_across_venues(track_id);
        let user = self.library.user_id();
        let track_id = track_id.to_string();
        cx.spawn(async move |this, cx| {
            let listing = pending.await;
            this.update(cx, |this, cx| {
                this.with_scores(&track_id, cx, |level| {
                    level.loaded = true;
                    match listing {
                        Ok(summaries) => {
                            level.rows = rows(&summaries, user.as_deref());
                            level.error = None;
                        }
                        Err(error) => level.error = Some(error.to_string()),
                    }
                });
            })
            .ok();
        })
        .detach();
    }

    /// Re-read the venue's songs, and the selected song's scores.
    pub(crate) fn refresh_score_listing(&mut self, cx: &mut Context<Self>) {
        let Some(songs) = &self.songs else {
            return;
        };
        let venue = songs.venue_id().to_owned();
        let generation = songs.load_generation();
        let selected = songs.scores.as_ref().map(|scores| scores.track.id.clone());
        let tracks = self.library.tracks(&venue);
        cx.spawn(async move |this, cx| {
            let result = tracks.await;
            this.update(cx, |this, cx| {
                this.with_tracks_for_venue(&venue, generation, cx, |songs| match result {
                    Ok(rows) => songs.replace_rows(rows),
                    Err(error) => songs.error = Some(error.to_string()),
                });
            })
            .ok();
        })
        .detach();
        if let Some(track) = selected {
            self.load_track_scores(&track, cx);
        }
    }

    /// Run `edit` against `track_id`'s scores, while it is still the selected
    /// song — the same admission rule the venue's own reads take.
    fn with_scores(
        &mut self,
        track_id: &str,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Scores),
    ) {
        let Some(songs) = &mut self.songs else {
            return;
        };
        let Some(level) = songs
            .scores
            .as_mut()
            .filter(|level| level.track.id == track_id)
        else {
            return;
        };
        edit(level);
        cx.notify();
    }

    /// Open `score` and close the song list so the timeline has the room.
    ///
    /// The close waits [`DOUBLE_CLICK`]: the click that lands here may be the
    /// first of a rename double-click, and the second click has to find its
    /// row. A rename in that time cancels the close.
    pub(crate) fn open_listed_score(
        &mut self,
        track_id: SharedString,
        score: crate::track_editor::Score,
        cx: &mut Context<Self>,
    ) {
        if self.songs.is_none() {
            return;
        }
        self.open_score(&track_id, score, None, cx);
        let wait = self.library.debounce(DOUBLE_CLICK);
        self.song_list_close = Some(cx.spawn(async move |this, cx| {
            wait.await;
            this.update(cx, |this, cx| {
                this.song_list_close = None;
                this.song_list = false;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Mint `track_id` another score in the open venue and open it.
    ///
    /// `create_score`, not `ensure_track_in_venue`: the row says *new*, and
    /// the idempotent form would silently hand back the score already there.
    pub(crate) fn create_listed_score(&mut self, track_id: &str, cx: &mut Context<Self>) {
        let Some(songs) = &self.songs else {
            return;
        };
        let venue_id = songs.venue_id().to_string();
        let track_id = track_id.to_string();
        let request_id = uuid::Uuid::new_v4().to_string();
        let pending = self
            .library
            .create_score(&request_id, &track_id, &venue_id, None);
        cx.spawn(async move |this, cx| {
            // Sequential on purpose: the listing has to be taken *after* the
            // score exists, or the row about to be opened is not in it yet.
            let created = pending.await;
            let Ok(listing) =
                this.read_with(cx, |this, _| this.library.scores_across_venues(&track_id))
            else {
                return;
            };
            let listing = listing.await;
            this.update(cx, |this, cx| {
                let user = this.library.user_id();
                let mut open = None;
                this.with_scores(&track_id, cx, |level| match (&created, listing) {
                    (Ok(score), Ok(summaries)) => {
                        level.rows = rows(&summaries, user.as_deref());
                        open = level
                            .rows
                            .iter()
                            .find(|row| row.id == score.id)
                            .map(open_row);
                    }
                    (Err(error), _) => level.error = Some(error.to_string()),
                    (_, Err(error)) => level.error = Some(error.to_string()),
                });
                if let Some(score) = open {
                    this.open_listed_score(track_id.clone().into(), score, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Raise the score menu over the row that was right-clicked. Only rows in
    /// the open venue get here — another venue's score is that venue's to
    /// delete or rename.
    fn open_score_menu(&mut self, track_id: &str, menu: ScoreMenu, cx: &mut Context<Self>) {
        let Some(level) = self
            .songs
            .as_mut()
            .and_then(|songs| songs.scores_of_mut(track_id))
        else {
            return;
        };
        level.menu = Some(menu);
        cx.notify();
    }

    /// Close it, reporting whether there was one — which is what puts this on
    /// Escape's ladder without giving the menu a binding of its own.
    pub(crate) fn close_score_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(songs) = &mut self.songs else {
            return false;
        };
        let closed = songs
            .scores
            .as_mut()
            .is_some_and(|level| level.menu.take().is_some());
        if closed {
            cx.notify();
        }
        closed
    }

    /// The delete gesture: straight through when the score holds nothing, and
    /// through [`crate::confirm`] when it holds clips.
    ///
    /// The count decides, not a preference: a confirmation on an empty score
    /// is a dialog that can only ever be answered one way, and one that is
    /// always answered the same way stops being read.
    fn request_delete_score(&mut self, track_id: SharedString, cx: &mut Context<Self>) {
        let Some(menu) = self
            .songs
            .as_ref()
            .and_then(|songs| songs.scores_of(&track_id))
            .and_then(|level| level.menu.as_ref())
        else {
            return;
        };
        let (score_id, venue_id, ordinal, clips) = (
            menu.score_id.clone(),
            menu.venue_id.clone(),
            menu.ordinal,
            menu.clips,
        );
        self.close_score_menu(cx);
        if clips == 0 {
            self.delete_score(&track_id, &score_id, &venue_id, cx);
            return;
        }
        self.ask(
            crate::confirm::Confirm {
                title: format!("Delete score #{ordinal}?").into(),
                body: format!(
                    "It holds {clips} {}. Deleting archives the document — its history is kept, but it leaves this list.",
                    if clips == 1 { "pattern" } else { "patterns" }
                )
                .into(),
                verb: "Delete score".into(),
                action: crate::confirm::Action::DeleteScore {
                    score_id,
                    track_id,
                    venue_id,
                },
            },
            cx,
        );
    }

    /// Archive the score, close it if it is the open one, and re-read the
    /// listing.
    ///
    /// The score closes *first*, before the seam has answered: a timeline
    /// that kept drawing a document that no longer exists for as long as the
    /// round trip takes, and then vanished, would be worse.
    pub(crate) fn delete_score(
        &mut self,
        track_id: &str,
        score_id: &str,
        venue_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.close_score_menu(cx);
        let pending = self.library.delete_score(score_id);
        let doomed = Target::Score {
            venue: venue_id.to_string(),
            track: track_id.to_string(),
            score: score_id.to_string(),
        };
        self.close_bodies_where(|target| target != &doomed, cx);
        let track_id = track_id.to_string();
        cx.spawn(async move |this, cx| {
            let deleted = pending.await;
            let Ok(listing) =
                this.read_with(cx, |this, _| this.library.scores_across_venues(&track_id))
            else {
                return;
            };
            let listing = listing.await;
            this.update(cx, |this, cx| {
                let user = this.library.user_id();
                this.with_scores(&track_id, cx, |level| match (deleted, listing) {
                    (Ok(()), Ok(summaries)) => level.rows = rows(&summaries, user.as_deref()),
                    (Err(error), _) => level.error = Some(error.to_string()),
                    (_, Err(error)) => level.error = Some(error.to_string()),
                });
            })
            .ok();
        })
        .detach();
    }

    /// Put the caret in `score_id`'s name. Only this venue's own scores get
    /// here: another person's score is read only, and another venue's is not
    /// this list's subject.
    fn start_score_rename(
        &mut self,
        track_id: SharedString,
        score_id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self
            .songs
            .as_ref()
            .and_then(|songs| songs.scores_of(&track_id))
            .and_then(|level| level.rows.iter().find(|row| row.id == score_id))
        else {
            return;
        };
        if row.read_only {
            return;
        }
        // The second click of the pair: the list stays for the caret.
        self.song_list_close = None;
        let opened_on = row.name.as_deref().unwrap_or_default().to_string();
        let field = cx.new(|cx| {
            let mut input = TextInput::search("Score name", cx);
            input.set_text(opened_on.clone(), cx);
            input
        });
        let keys = cx.subscribe_in(
            &field,
            window,
            |this: &mut Luma, _, event: &text_input::Event, window, cx| match event {
                text_input::Event::Submitted => this.finish_score_rename(true, Some(window), cx),
                text_input::Event::Cancelled => this.finish_score_rename(false, Some(window), cx),
                // A press elsewhere. Focus goes where the press sends it.
                text_input::Event::Blurred => this.finish_score_rename(true, None, cx),
                _ => cx.notify(),
            },
        );
        // Focus leaving by the keyboard, such as Tab. A press elsewhere is
        // `Blurred` above; whichever comes second finds nothing to save.
        let luma = cx.entity().downgrade();
        let blur = window.on_focus_out(&field.read(cx).focus_handle(cx), cx, move |_, _, cx| {
            luma.update(cx, |this, cx| this.finish_score_rename(true, None, cx))
                .ok();
        });
        let focus = field.read(cx).focus_handle(cx);
        if let Some(level) = self
            .songs
            .as_mut()
            .and_then(|songs| songs.scores_of_mut(&track_id))
        {
            level.menu = None;
            level.rename = Some(Rename {
                score_id,
                field,
                opened_on,
                _subscriptions: vec![keys, blur],
            });
        }
        window.focus(&focus, cx);
        cx.notify();
    }

    /// End the rename. `save` writes the typed name when it is a different,
    /// non-empty name; otherwise nothing is written. `window` is present when
    /// a key ended it, and the keyboard then goes back to the list; on a
    /// blur, focus has already gone where the person sent it.
    fn finish_score_rename(
        &mut self,
        save: bool,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let focus = self.focus.clone();
        let Some(songs) = &mut self.songs else {
            return;
        };
        let Some((track_id, rename)) = songs
            .scores
            .as_mut()
            .and_then(|level| Some((level.track.id.clone(), level.rename.take()?)))
        else {
            return;
        };
        if let Some(window) = window {
            window.focus(&focus, cx);
        }
        cx.notify();
        let name = rename.field.read(cx).text().trim().to_string();
        if !save || name.is_empty() || name == rename.opened_on {
            return;
        }
        let pending = self.library.rename_score(&rename.score_id, &name);
        cx.spawn(async move |this, cx| {
            // The listing is read after the write: before it, the old name is
            // still in it.
            let renamed = pending.await;
            let Ok(listing) =
                this.read_with(cx, |this, _| this.library.scores_across_venues(&track_id))
            else {
                return;
            };
            let listing = listing.await;
            this.update(cx, |this, cx| {
                let user = this.library.user_id();
                this.with_scores(&track_id, cx, |level| match (renamed, listing) {
                    (Ok(()), Ok(summaries)) => level.rows = rows(&summaries, user.as_deref()),
                    (Err(error), _) => level.error = Some(error.to_string()),
                    (_, Err(error)) => level.error = Some(error.to_string()),
                });
            })
            .ok();
        })
        .detach();
    }
}

/// The row, as the editor holds the score it is showing. This list is the one
/// place a row becomes the open score, so the two cannot disagree about which
/// `#n` or which ownership the timeline is under.
pub(crate) fn open_row(row: &ScoreRow) -> crate::track_editor::Score {
    crate::track_editor::Score {
        id: row.id.to_string(),
        ordinal: row.ordinal,
        read_only: row.read_only,
    }
}

// -- rendering ----------------------------------------------------------------

/// How long a click on a score waits before it closes the song list, so the
/// second click of a double-click still finds the row. About the platform's
/// double-click interval: gpui does not publish it.
const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(400);

/// The third column: the selected song's scores in this venue, the way to
/// mint another, then each other venue's under its name. Says what to do when
/// no song is selected.
pub(super) fn column(
    shell: &Luma,
    state: &super::Tracks,
    statuses: &HashMap<Target, ChatStatus>,
    app: &Entity<Luma>,
) -> AnyElement {
    let Some(scores) = &state.scores else {
        return float::empty_row(if state.shown.is_empty() {
            ""
        } else {
            "Select a song to see its scores"
        })
        .into_any_element();
    };
    let venue = state.venue_id();
    let open = shell
        .workspace
        .of_mode(crate::workspace::Mode::Arrange)
        .map(|slot| slot.target.clone());
    let track = &scores.track;
    let items = scores.items(venue);
    let renaming = scores
        .rename
        .as_ref()
        .map(|rename| (rename.score_id.clone(), rename.field.clone()));
    // A row's paint: the cursor's, in the column's tint for whether the
    // keyboard is here.
    let row_state = |item: Item| {
        let at = items.iter().position(|other| *other == item);
        state.path_state(super::Column::Scores, at.is_some() && at == scores.cursor)
    };
    // `run` is the row's place under its venue's heading: the stripes count
    // from each heading.
    let draw = |run: usize, row: &ScoreRow| {
        let index = scores
            .rows
            .iter()
            .position(|other| other.id == row.id)
            .unwrap_or_default();
        let here = row.venue_id == venue;
        let target = Target::Score {
            venue: row.venue_id.to_string(),
            track: track.id.clone(),
            score: row.id.to_string(),
        };
        let rename = renaming
            .as_ref()
            .filter(|(id, _)| here && *id == row.id)
            .map(|(_, field)| field);
        score_row(
            row,
            index,
            run,
            row_state(Item::Score(index)),
            here,
            open.as_ref() == Some(&target),
            statuses.get(&target).copied(),
            app,
            track,
            rename,
        )
    };
    let here: Vec<&ScoreRow> = scores
        .rows
        .iter()
        .filter(|row| row.venue_id == venue)
        .collect();
    div()
        .id("scores-column")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .child(
            float::section_heading(state.venue_name().to_string())
                .px(px(ROW_INSET))
                .pt(px(10.)),
        )
        .children(scores.error.clone().map(float::error_row))
        .when(scores.error.is_none() && !scores.loaded, |el| {
            el.child(float::empty_row("Loading scores…"))
        })
        .children(here.iter().enumerate().map(|(run, row)| draw(run, row)))
        .when(scores.loaded && here.is_empty(), |el| {
            el.child(float::empty_row(format!(
                "No scores in {}",
                state.venue_name()
            )))
        })
        .when(scores.loaded, |el| {
            el.child(new_score(row_state(Item::New), here.len(), app, &track.id))
        })
        .children(elsewhere(&scores.rows, venue).map(|(name, rows)| {
            div()
                .flex()
                .flex_col()
                .pt(px(10.))
                .child(float::section_heading(name).px(px(ROW_INSET)))
                .children(
                    rows.into_iter()
                        .enumerate()
                        .map(|(run, row)| draw(run, row)),
                )
        }))
        .agent_node(
            Role::Card,
            format!("Scores of {}", super::track_name(track)),
        )
        .into_any_element()
}

/// The context menu over a right-clicked score, if one is up.
pub(super) fn menu(state: &super::Tracks, app: &Entity<Luma>) -> Option<AnyElement> {
    let level = state.scores.as_ref()?;
    let open = level.menu.as_ref()?;
    let track_id = SharedString::from(level.track.id.clone());
    let deleted = app.clone();
    let dismissed = app.clone();
    Some(
        luma_ui::menu::ContextMenu::new("score-menu", open.at)
            .destructive("Delete score", move |_, cx| {
                let track_id = track_id.clone();
                deleted.update(cx, |this, cx| this.request_delete_score(track_id, cx));
            })
            .render(move |_, cx| {
                dismissed.update(cx, |this, cx| {
                    this.close_score_menu(cx);
                });
            }),
    )
}

/// Every other venue's rows, grouped under the venue's name, in the order the
/// venues first appear — which is newest-created first, so the room with the
/// newest score leads. The stage's score menu groups them the same way.
pub(crate) fn elsewhere<'a>(
    rows: &'a [ScoreRow],
    venue: &str,
) -> impl Iterator<Item = (SharedString, Vec<&'a ScoreRow>)> {
    let mut order: Vec<SharedString> = Vec::new();
    let mut groups: HashMap<SharedString, Vec<&ScoreRow>> = HashMap::new();
    for row in rows.iter().filter(|row| row.venue_id != venue) {
        if !groups.contains_key(&row.venue_id) {
            order.push(row.venue_id.clone());
        }
        groups.entry(row.venue_id.clone()).or_default().push(row);
    }
    order.into_iter().filter_map(move |id| {
        let rows = groups.remove(&id)?;
        Some((rows.first()?.venue.clone(), rows))
    })
}

/// One score. A press opens it — here at once, in another venue after asking
/// (see [`Luma::choose_score_item`]). `here` is whether it is this venue's:
/// only those take the right-click menu and the rename. The open score wears a
/// check.
///
/// `rename` is the live field when this row's name is being typed, and
/// `status` the dot of the score's chat.
#[allow(clippy::too_many_arguments)]
fn score_row(
    row: &ScoreRow,
    index: usize,
    run: usize,
    state: Lit,
    here: bool,
    open: bool,
    status: Option<ChatStatus>,
    app: &Entity<Luma>,
    track: &TrackBrowserRow,
    rename: Option<&Entity<TextInput>>,
) -> AnyElement {
    let renamer = (here && !row.read_only).then(|| {
        (
            app.clone(),
            SharedString::from(track.id.clone()),
            row.id.clone(),
        )
    });
    let id = SharedString::from(format!("score-{}", row.id));
    let label = format!(
        "#{} · {} · {} patterns · {}{}{}{}",
        row.ordinal,
        row.author,
        row.clips,
        row.age,
        row.spend,
        if row.read_only { " · read only" } else { "" },
        if here {
            String::new()
        } else {
            format!(" · {}", row.venue)
        }
    );
    let pressed = app.clone();
    let track_id = track.id.clone();
    column_row(state, run, id)
        .px(px(ROW_INSET))
        .when(here, |el| {
            let raised = app.clone();
            let menu_track = track.id.clone();
            let (score_id, venue_id, ordinal, clips) =
                (row.id.clone(), row.venue_id.clone(), row.ordinal, row.clips);
            el.on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                let menu = ScoreMenu {
                    at: event.position,
                    score_id: score_id.clone(),
                    ordinal,
                    venue_id: venue_id.clone(),
                    clips,
                };
                raised.update(cx, |this, cx| this.open_score_menu(&menu_track, menu, cx));
            })
        })
        .on_click(move |_, window, cx| {
            let track_id = track_id.clone();
            pressed.update(cx, |this, cx| {
                this.focus_columns(window, cx);
                this.choose_score_item(&track_id, Item::Score(index), cx);
            });
        })
        // The handle leads: a score's own name is usually absent and its uuid
        // is unsayable, so `#2` is the only thing two people can both point at.
        .child(
            div()
                .flex_shrink_0()
                .w(px(28.))
                .text_size(px(11.))
                .font_weight(FontWeight::BOLD)
                .text_color(glass::ink(if open { 0.95 } else { glass::TEXT_SECONDARY }))
                .child(format!("#{}", row.ordinal)),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .overflow_hidden()
                .child(match rename {
                    Some(field) => name_field(row, field),
                    None => name_label(row, open, renamer),
                }),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(11.))
                .text_color(glass::ink(glass::TEXT_TERTIARY))
                .child(row.detail()),
        )
        .when(row.read_only, |el| el.child(luma_ui::caption("Read only")))
        .children(
            status.map(|status| crate::agent::status_dot(status, &format!("#{}", row.ordinal))),
        )
        .child(float::check(open))
        .agent_node(Role::Row, label)
        .into_any_element()
}

/// The score's name, or its owner when it has none. A double-click on it
/// starts a rename when `renamer` is present — this venue's own score.
///
/// The first click of the pair goes to the row, which opens the score and
/// closes the list after [`DOUBLE_CLICK`]. The second click stops here, so
/// the row does not see it, and the rename cancels the close.
fn name_label(
    row: &ScoreRow,
    open: bool,
    renamer: Option<(Entity<Luma>, SharedString, SharedString)>,
) -> AnyElement {
    let name = row.title();
    div()
        .id(SharedString::from(format!("score-name-{}", row.id)))
        .truncate()
        .text_size(px(12.))
        .text_color(glass::ink(if open { 0.95 } else { 0.8 }))
        .child(name.clone())
        .when_some(renamer, |el, (app, track_id, score_id)| {
            el.on_click(move |event, window, cx| {
                if event.click_count() != 2 {
                    return;
                }
                cx.stop_propagation();
                let (track_id, score_id) = (track_id.clone(), score_id.clone());
                app.update(cx, |this, cx| {
                    this.start_score_rename(track_id, score_id, window, cx)
                });
            })
        })
        .agent_node(
            Role::Text,
            format!("Score name #{} = {}", row.ordinal, name),
        )
        .into_any_element()
}

/// The name as a live field. The draft context gives the field Enter and
/// Escape ahead of the list's own keys. Presses inside it stop here, so
/// placing the caret does not open the score and take focus from the field.
fn name_field(row: &ScoreRow, field: &Entity<TextInput>) -> AnyElement {
    float::field()
        .id(SharedString::from(format!("score-name-field-{}", row.id)))
        .key_context(DRAFT_CONTEXT)
        .w_full()
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(div().w_full().child(field.clone()))
        .agent_node(Role::Input, format!("Score name #{}", row.ordinal))
        .into_any_element()
}

/// Mint another score on this `(track, venue)`.
fn new_score(state: Lit, run: usize, app: &Entity<Luma>, track_id: &str) -> AnyElement {
    let app = app.clone();
    let track_id = track_id.to_string();
    column_row(state, run, format!("new-score:{track_id}"))
        .px(px(ROW_INSET))
        .on_click(move |_, window, cx| {
            let track_id = track_id.clone();
            app.update(cx, |this, cx| {
                this.focus_columns(window, cx);
                this.choose_score_item(&track_id, Item::New, cx)
            })
        })
        .child(
            gpui_component::Icon::new(luma_ui::icons::IconName::Plus)
                .size(px(11.))
                .text_color(glass::ink(glass::TEXT_TERTIARY)),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(12.))
                .text_color(glass::ink(glass::TEXT_SECONDARY))
                .child("New score"),
        )
        .agent_node(Role::Button, "New score")
        .into_any_element()
}
