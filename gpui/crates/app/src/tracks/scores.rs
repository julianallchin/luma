//! A song's scores, expanded in place under its row on the song list.
//!
//! A score is a `(track, venue)` document owned by one principal, and a pair
//! holds as many of them as there are principals who annotated it. One score
//! is open at a time: choosing one opens it in Arrange and closes the list.
//!
//! # Why in place
//!
//! Scores are *navigation*: choosing one is choosing what the timeline is a
//! view of, which is the same kind of act as choosing the track. So they live
//! where the track is, one press deeper, and the way back is the same press.
//!
//! # This venue's scores are the list; the others are context
//!
//! The listing is cross-venue ([`Library::scores_across_venues`]) because "the
//! same track, scored in the warehouse too" is the fact the operator cannot
//! otherwise find. Only the open venue's rows answer the pointer: another
//! venue's score belongs to that venue.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use luma_ui::float::{self, RowState};
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

/// The level's whole state: the track it is about, and the listing for it.

/// One song's expansion: the track it is about, where in the list it was
/// expanded, and the listing for it.
///
/// The track is a *snapshot*, not an index into the page's rows: the filters
/// behind it can admit a different set while it is open.
pub(crate) struct Scores {
    pub(crate) track: TrackBrowserRow,
    /// The folder the song was expanded under, or none under All songs. A song
    /// is listed once per place, and expands at the one that was pressed.
    pub(crate) folder: Option<String>,
    pub(crate) rows: Rc<[ScoreRow]>,
    /// Whether the listing has come back. Written in the same assignment as
    /// [`Self::rows`] and [`Self::error`], so "still loading" and "no scores"
    /// can never be confused for one another.
    pub(crate) loaded: bool,
    pub(crate) error: Option<String>,
    /// The context menu a right-click raised, or none. Held here and not by
    /// the row: a row is rebuilt every frame from the listing, and the menu
    /// has to outlive that.
    pub(crate) menu: Option<ScoreMenu>,
    /// The score whose name is being typed, or none. One caret is what a
    /// person has.
    rename: Option<Rename>,
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
    /// Tell the list an expansion changed height: its entry is measured again
    /// on the next draw, and the rows around it keep theirs.
    fn remeasure(&mut self, track_id: &str) {
        let Some(row) = self.rows.iter().position(|row| row.id == track_id) else {
            return;
        };
        let at: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| matches!(entry, super::Entry::Scores { row: r, .. } if *r == row))
            .map(|(index, _)| index)
            .collect();
        for index in at {
            self.list.splice(index..index + 1, 1);
        }
    }
}

// -- navigation and writes ----------------------------------------------------

impl Luma {
    /// Expand `track_id` to its scores under the row pressed at `folder`, or
    /// fold them away. A song open in one place and pressed in another moves
    /// there.
    ///
    /// The listing is read fresh on every expansion rather than cached with
    /// the row: a score minted in another window is exactly the thing this
    /// exists to show.
    pub(crate) fn toggle_track_scores(
        &mut self,
        track_id: &str,
        folder: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(songs) = &mut self.songs else {
            return;
        };
        songs.menu = None;
        if let Some(open) = songs.scores.get_mut(track_id) {
            if open.folder == folder {
                songs.scores.remove(track_id);
            } else {
                open.folder = folder;
            }
            songs.refilter();
            cx.notify();
            return;
        }
        let Some(track) = songs.find(track_id) else {
            return;
        };
        songs.scores.insert(
            track_id.to_string(),
            Scores {
                track,
                folder,
                rows: Vec::new().into(),
                loaded: false,
                error: None,
                menu: None,
                rename: None,
            },
        );
        songs.refilter();
        cx.notify();
        self.load_track_scores(track_id, cx);
    }

    /// Read `track_id`'s listing into its expansion.
    fn load_track_scores(&mut self, track_id: &str, cx: &mut Context<Self>) {
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

    /// Re-read the venue's songs, and every expanded song's scores.
    pub(crate) fn refresh_score_listing(&mut self, cx: &mut Context<Self>) {
        let Some(songs) = &self.songs else {
            return;
        };
        let venue = songs.venue_id().to_owned();
        let generation = songs.load_generation();
        let expanded: Vec<String> = songs.scores.keys().cloned().collect();
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
        for track in expanded {
            self.load_track_scores(&track, cx);
        }
    }

    /// Run `edit` against `track_id`'s expansion, while it is still open —
    /// the same admission rule the venue's own reads take.
    fn with_scores(
        &mut self,
        track_id: &str,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Scores),
    ) {
        let Some(songs) = &mut self.songs else {
            return;
        };
        let Some(level) = songs.scores.get_mut(track_id) else {
            return;
        };
        edit(level);
        songs.remeasure(track_id);
        cx.notify();
    }

    /// Open `score` and close the song list so the timeline has the room.
    ///
    /// The close waits [`DOUBLE_CLICK`]: the click that lands here may be the
    /// first of a rename double-click, and the second click has to find its
    /// row. A rename in that time cancels the close. A score that is already
    /// the open one closes the list at once.
    pub(crate) fn open_listed_score(
        &mut self,
        track_id: SharedString,
        score: crate::track_editor::Score,
        cx: &mut Context<Self>,
    ) {
        let Some(songs) = &self.songs else {
            return;
        };
        let target = Target::Score {
            venue: songs.venue_id().to_string(),
            track: track_id.to_string(),
            score: score.id.clone(),
        };
        let already = self.workspace.body(&target).is_some();
        self.open_score(&track_id, score, None, cx);
        if already {
            self.song_list = false;
            self.song_list_close = None;
            cx.notify();
            return;
        }
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
    /// the open venue get here — another venue's score is not this list's to
    /// act on.
    fn open_score_menu(&mut self, track_id: &str, menu: ScoreMenu, cx: &mut Context<Self>) {
        let Some(songs) = &mut self.songs else {
            return;
        };
        for (track, level) in songs.scores.iter_mut() {
            level.menu = None;
            if track == track_id {
                level.menu = Some(menu);
                break;
            }
        }
        cx.notify();
    }

    /// Close it, reporting whether there was one — which is what puts this on
    /// Escape's ladder without giving the menu a binding of its own.
    pub(crate) fn close_score_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(songs) = &mut self.songs else {
            return false;
        };
        let mut closed = false;
        for level in songs.scores.values_mut() {
            closed |= level.menu.take().is_some();
        }
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
            .and_then(|songs| songs.scores.get(track_id.as_ref()))
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
            .and_then(|songs| songs.scores.get(track_id.as_ref()))
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
        if let Some(songs) = self.songs.as_mut() {
            for level in songs.scores.values_mut() {
                level.rename = None;
            }
            if let Some(level) = songs.scores.get_mut(track_id.as_ref()) {
                level.menu = None;
                level.rename = Some(Rename {
                    score_id,
                    field,
                    opened_on,
                    _subscriptions: vec![keys, blur],
                });
            }
            songs.remeasure(&track_id);
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
            .iter_mut()
            .find_map(|(track, level)| Some((track.clone(), level.rename.take()?)))
        else {
            return;
        };
        songs.remeasure(&track_id);
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

/// How far an expansion's rows sit in from the page's edge: under the song's
/// title, past its disclosure and art.
const INDENT: f32 = 16. + 16. + 10. + 12. + 36. + 12.;

/// The expansion under `track_id`'s row: this venue's scores, the way to mint
/// another, and — quietly — the other rooms the song is scored in.
///
/// Returned as a closure over its inputs because the list draws it on demand,
/// once per frame it is in view.
pub(super) fn expansion(
    shell: &Luma,
    state: &super::Tracks,
    track_id: &str,
    statuses: &HashMap<Target, ChatStatus>,
    app: &Entity<Luma>,
) -> Rc<dyn Fn() -> AnyElement> {
    let Some(scores) = state.scores.get(track_id) else {
        return Rc::new(|| div().into_any_element());
    };
    let venue = state.venue_id().to_string();
    let venue_name = state.venue_name().to_string();
    let open = shell
        .workspace
        .of_mode(crate::workspace::Mode::Arrange)
        .map(|slot| slot.target.clone());
    let statuses = statuses.clone();
    let rows = Rc::clone(&scores.rows);
    let (loaded, error) = (scores.loaded, scores.error.clone());
    let track = scores.track.clone();
    let renaming = scores
        .rename
        .as_ref()
        .map(|rename| (rename.score_id.clone(), rename.field.clone()));
    let app = app.clone();
    Rc::new(move || {
        let here: Vec<&ScoreRow> = rows.iter().filter(|row| row.venue_id == venue).collect();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(2.))
            .py(px(6.))
            .pl(px(INDENT))
            .pr(px(16.))
            .bg(glass::wash(0x04 as f32 / 255.0))
            .border_b_1()
            .border_color(glass::inner_rule())
            .children(error.clone().map(float::error_row))
            .when(error.is_none() && !loaded, |el| {
                el.child(float::empty_row("Loading scores…"))
            })
            .when(loaded && here.is_empty(), |el| {
                el.child(float::empty_row(format!("No scores in {venue_name}")))
            })
            .children(here.into_iter().map(|row| {
                let target = Target::Score {
                    venue: venue.clone(),
                    track: track.id.clone(),
                    score: row.id.to_string(),
                };
                let rename = renaming
                    .as_ref()
                    .filter(|(id, _)| *id == row.id)
                    .map(|(_, field)| field);
                score_row(
                    row,
                    open.as_ref() == Some(&target),
                    statuses.get(&target).copied(),
                    Some((&app, &track)),
                    rename,
                )
            }))
            .when(loaded, |el| el.child(new_score(&app, &track.id)))
            .children(elsewhere(&rows, &venue).map(|(venue, rows)| {
                div()
                    .flex()
                    .flex_col()
                    .pt(px(6.))
                    .child(float::section_heading(venue))
                    .children(
                        rows.into_iter()
                            .map(|row| score_row(row, false, None, None, None)),
                    )
            }))
            .agent_node(
                Role::Card,
                format!("Scores of {}", super::track_name(&track)),
            )
            .into_any_element()
    })
}

/// The context menu over a right-clicked score, if one is up anywhere.
pub(super) fn menus(state: &super::Tracks, app: &Entity<Luma>) -> Option<AnyElement> {
    let (track_id, open) = state
        .scores
        .iter()
        .find_map(|(track, level)| Some((track.clone(), level.menu.as_ref()?)))?;
    let track_id = SharedString::from(track_id);
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

/// Every other venue's rows, grouped, in the order the venues first appear —
/// which is newest-created first, so the room with the newest score leads.
fn elsewhere<'a>(
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

/// One score.
///
/// `press` present means the row is openable; `None` is another venue's
/// score, which is readable here and openable only from that venue. The open
/// score wears the selected wash.
///
/// `rename` is the live field when this row's name is being typed, and
/// `status` the dot of the score's chat.
fn score_row(
    row: &ScoreRow,
    open: bool,
    status: Option<ChatStatus>,
    press: Option<(&Entity<Luma>, &TrackBrowserRow)>,
    rename: Option<&Entity<TextInput>>,
) -> AnyElement {
    let renamer = press.filter(|_| !row.read_only).map(|(app, track)| {
        (
            app.clone(),
            SharedString::from(track.id.clone()),
            row.id.clone(),
        )
    });
    let id = SharedString::from(format!("score-{}", row.id));
    let label = format!(
        "#{} · {} · {} patterns · {}{}{}",
        row.ordinal,
        row.author,
        row.clips,
        row.age,
        row.spend,
        if row.read_only { " · read only" } else { "" }
    );
    float::menu_row(
        if open {
            RowState::Selected
        } else {
            RowState::Rest
        },
        id.clone(),
    )
    .id(id)
    .when(press.is_none(), |row| row.cursor_default().opacity(0.55))
    .when_some(press, |el, (app, track)| {
        let app = app.clone();
        let track_id = SharedString::from(track.id.clone());
        let menu_track = track.id.clone();
        let score = open_row(row);
        let raised = app.clone();
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
        .on_click(move |_, _, cx| {
            let (track_id, score) = (
                track_id.clone(),
                crate::track_editor::Score {
                    id: score.id.clone(),
                    ordinal: score.ordinal,
                    read_only: score.read_only,
                },
            );
            app.update(cx, |this, cx| this.open_listed_score(track_id, score, cx));
        })
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
    .children(status.map(|status| crate::agent::status_dot(status, &format!("#{}", row.ordinal))))
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
fn new_score(app: &Entity<Luma>, track_id: &str) -> AnyElement {
    let app = app.clone();
    let track_id = track_id.to_string();
    float::menu_row(RowState::Rest, format!("new-score:{track_id}"))
        .id(SharedString::from(format!("new-score:{track_id}")))
        .on_click(move |_, _, cx| {
            let track_id = track_id.clone();
            app.update(cx, |this, cx| this.create_listed_score(&track_id, cx))
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
