//! The sidebar's second level: one track's scores.
//!
//! A score is a `(track, venue)` document owned by one principal, and a pair
//! holds as many of them as there are principals who annotated it
//! (`migrations/20260325000000_multi_score.sql`). The editor opens the most
//! recently touched one; this level is where the rest of them exist.
//!
//! # Why a level and not a strip
//!
//! Scores are *navigation*: choosing one is choosing what the timeline is a
//! view of, which is the same kind of act as choosing the track. So they live
//! where the track list lives, one push deeper — the column's subject narrows
//! from "this venue's library" to "this track's documents", and the way back
//! is the way back. A strip above the canvas would have made the same choice
//! read as a property of the editor, which it is not.
//!
//! # This venue's scores are the list; the others are context
//!
//! The listing is cross-venue ([`Library::scores_across_venues`]) because "the
//! same track, scored in the warehouse too" is the fact the operator cannot
//! otherwise find. Only the open venue's rows answer the pointer: a tab is
//! keyed by `(track, venue)`, so another venue's score is a different tab and
//! not a different reading of this one.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use luma_ui::float::{self, RowState};
use luma_ui::glass;
use luma_ui::node::{AgentNode, Instrument, Role};

use luma_lib::models::authored_state::ActorLabel;
use luma_lib::models::scores::ScoreSummary;
use luma_lib::models::tracks::TrackBrowserRow;

use super::{track_face, Level, PAD_X};
use crate::Luma;

/// One score, as the level reads it. Everything here is resolved once, when
/// the listing lands: a draw compares strings and paints, and never asks the
/// seam or the clock anything.
pub(crate) struct ScoreRow {
    pub(crate) id: SharedString,
    /// The seam's display ordinal within `(track, venue)` — `#2`. A handle,
    /// not a key: it is a rank by creation instant and it renumbers when an
    /// earlier score is deleted, which is why nothing here stores it.
    pub(crate) ordinal: i64,
    pub(crate) venue_id: SharedString,
    pub(crate) venue: SharedString,
    /// Who *wrote* it: the actor on the newest revision of its authored
    /// document, read through [`ActorLabel`]. Not ownership — a score an agent
    /// authored through this person's session is still their document, and the
    /// row that said "You" for it was answering the wrong question. Falls back
    /// to the owner when the score has no authored history at all.
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
    /// How many revisions the document has. Shown only past one, because
    /// every score has at least the revision that created it.
    revisions: i64,
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
            // The principal, for the two cases the actor deliberately does not
            // name: `user` records that a human wrote it, not which one.
            let principal = || {
                if read_only {
                    SharedString::from(short_uid(score.uid.as_deref().unwrap_or_default()))
                } else {
                    SharedString::from("You")
                }
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
                author: match score.last_actor.as_deref().map(ActorLabel::parse) {
                    None | Some(ActorLabel::User) => principal(),
                    Some(actor) => actor.to_string().into(),
                },
                name: score.name.clone().map(SharedString::from),
                clips: score.annotation_count,
                age: crate::welcome::relative_age(
                    score
                        .last_authored_at
                        .as_deref()
                        .unwrap_or(&score.updated_at),
                )
                .into(),
                revisions: score.revision_count,
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
///
/// The track is a *snapshot*, not an index into the browser's rows: the head
/// has to keep drawing the same track while the push plays, and the filters
/// behind it can admit a different set by the time it lands.
pub(crate) struct Scores {
    pub(crate) track: TrackBrowserRow,
    /// The way back, and the level's keyboard seat. Focused on arrival so `←`
    /// and Escape have a dispatch path in the sidebar's key context the
    /// instant the level is up — a level nothing focuses is a level the
    /// keyboard cannot leave.
    back_focus: FocusHandle,
    pub(crate) rows: Rc<[ScoreRow]>,
    /// Whether the listing has come back. Written in the same assignment as
    /// [`Self::rows`] and [`Self::error`], so "still loading" and "no scores"
    /// can never be confused for one another.
    pub(crate) loaded: bool,
    pub(crate) error: Option<String>,
    /// The context menu a right-click raised, or none. Held by the level and
    /// not by the row: a row is rebuilt every frame from the listing, and the
    /// menu has to outlive that — and there is exactly one, because two menus
    /// open at once is the bug the single slot rules out.
    pub(crate) menu: Option<ScoreMenu>,
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

impl Scores {
    /// This venue's rows, which are the ones that answer the pointer.
    fn here<'a>(&'a self, venue: &'a str) -> impl Iterator<Item = &'a ScoreRow> {
        self.rows.iter().filter(move |row| row.venue_id == venue)
    }
}

// -- navigation and writes ----------------------------------------------------

impl Luma {
    /// Push the sidebar to `track_id`'s scores, with the row that named it
    /// flying from `row_top` (region-local, see [`super::Push`]) to the head.
    ///
    /// The listing is read fresh on every push rather than cached with the
    /// browser row: a score minted in another window is exactly the thing this
    /// level exists to show, and a cache would be showing yesterday's answer
    /// to the question the push just asked.
    pub(crate) fn show_scores(
        &mut self,
        track_id: &str,
        row_top: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(browser) = &self.sidebar else {
            return;
        };
        let Some(track) = browser.find(track_id) else {
            return;
        };
        let pending = self.library.scores_across_venues(track_id);
        let user = self.library.user_id();
        let track_id = track.id.clone();
        let back_focus = cx.focus_handle().tab_stop(true);
        window.focus(&back_focus, cx);
        if let Some(browser) = &mut self.sidebar {
            browser.enter(
                Scores {
                    track,
                    back_focus,
                    rows: Vec::new().into(),
                    loaded: false,
                    error: None,
                    menu: None,
                },
                row_top,
                cx,
            );
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let listing = pending.await;
            this.update(cx, |this, cx| {
                this.with_scores(&track_id, cx, |level| {
                    level.loaded = true;
                    match listing {
                        Ok(summaries) => level.rows = rows(&summaries, user.as_deref()),
                        Err(error) => level.error = Some(error.to_string()),
                    }
                });
            })
            .ok();
        })
        .detach();
    }

    /// Run `edit` against the scores level, only while it is still showing
    /// `track_id` — the same admission rule the venue's own reads take.
    fn with_scores(
        &mut self,
        track_id: &str,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Scores),
    ) {
        let Some(browser) = &mut self.sidebar else {
            return;
        };
        let Level::Scores(level) = &mut browser.level else {
            return;
        };
        if level.track.id != track_id {
            return;
        }
        edit(level);
        cx.notify();
    }

    /// Show `score` on the track's timeline, opening the tab if it is not up.
    ///
    /// Stays on this level: choosing a score is reading the list, and a list
    /// that dismissed itself on the first choice could not be compared.
    pub(crate) fn open_sidebar_score(
        &mut self,
        track_id: SharedString,
        score: crate::track_editor::Score,
        cx: &mut Context<Self>,
    ) {
        self.open_track(&track_id, cx);
        let Some(browser) = &self.sidebar else {
            return;
        };
        let target = crate::tabs::Target::TrackEditor {
            track: track_id.to_string(),
            venue: browser.venue_id().to_string(),
        };
        self.load_score(target, score, cx);
    }

    /// Mint this track another score in the open venue and show it.
    ///
    /// `create_score`, not `ensure_track_in_venue`: the row says *new*, and
    /// the idempotent form would silently hand back the score already there.
    /// Adding a track to a venue is the other gesture and keeps the other
    /// seam.
    pub(crate) fn create_sidebar_score(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = &self.sidebar else {
            return;
        };
        let Level::Scores(level) = &browser.level else {
            return;
        };
        let (track_id, venue_id) = (level.track.id.clone(), browser.venue_id().to_string());
        let request_id = uuid::Uuid::new_v4().to_string();
        let pending = self
            .library
            .create_score(&request_id, &track_id, &venue_id, None);
        cx.spawn(async move |this, cx| {
            // Sequential on purpose: the listing has to be taken *after* the
            // score exists, or the row about to be selected is not in it yet.
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
                    this.open_sidebar_score(track_id.clone().into(), score, cx);
                }
            })
            .ok();
        })
        .detach();
    }
}

impl Luma {
    /// Raise the score menu over the row that was right-clicked.
    ///
    /// Only rows in the open venue get here — see [`score_row`] — because the
    /// acts on the menu are acts on *this* room's document, and another
    /// venue's score is a different tab's subject.
    pub(crate) fn open_score_menu(&mut self, menu: ScoreMenu, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.sidebar else {
            return;
        };
        let Level::Scores(level) = &mut browser.level else {
            return;
        };
        level.menu = Some(menu);
        cx.notify();
    }

    /// Close it, reporting whether there was one — which is what puts this on
    /// Escape's ladder without giving the menu a binding of its own.
    pub(crate) fn close_score_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(browser) = &mut self.sidebar else {
            return false;
        };
        let Level::Scores(level) = &mut browser.level else {
            return false;
        };
        if level.menu.take().is_none() {
            return false;
        }
        cx.notify();
        true
    }

    /// The delete gesture: straight through when the score holds nothing, and
    /// through [`crate::confirm`] when it holds clips.
    ///
    /// The count decides, not a preference: a confirmation on an empty score
    /// is a dialog that can only ever be answered one way, and one that is
    /// always answered the same way stops being read — which is exactly when
    /// the one over a score with forty clips gets waved through too.
    pub(crate) fn request_delete_score(&mut self, track_id: SharedString, cx: &mut Context<Self>) {
        let Some(browser) = &self.sidebar else {
            return;
        };
        let Level::Scores(level) = &browser.level else {
            return;
        };
        let Some(menu) = level.menu.as_ref() else {
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
                    if clips == 1 { "clip" } else { "clips" }
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

    /// Archive the score, take the editor off it if that is what it was
    /// showing, and re-read the listing.
    ///
    /// The editor is cleared *first* and unconditionally — see
    /// [`Luma::unload_score`]. The listing is re-read after the seam answers,
    /// for the same reason [`Luma::create_sidebar_score`] reads after the
    /// create: a listing taken before the write still has the row in it.
    pub(crate) fn delete_score(
        &mut self,
        track_id: &str,
        score_id: &str,
        venue_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.close_score_menu(cx);
        let pending = self.library.delete_score(score_id);
        self.unload_score(
            &crate::tabs::Target::TrackEditor {
                track: track_id.to_string(),
                venue: venue_id.to_string(),
            },
            score_id,
            cx,
        );
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
}

/// The row, as the editor holds the score it is showing. This level is the one
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

/// The head's Back affordance, and therefore where the flying track row lands:
/// the head's row sits directly under it, at the same inset the list rows have,
/// so the shared element travels in `y` alone.
pub(super) const BACK_ROW_HEIGHT: f32 = 26.;

/// The whole level: the head, this venue's scores, the way to mint another,
/// and — quietly, at the foot — the other rooms this track is scored in.
pub(super) fn level(
    shell: &Luma,
    state: &super::Tracks,
    scores: &Scores,
    app: &Entity<Luma>,
    window: &Window,
    flying: bool,
) -> AnyElement {
    let venue = state.venue_id();
    let open = shell.open_score_id(&scores.track.id, venue);
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(head(scores, app, window, flying))
        .child(body(state, scores, open.as_deref(), app))
        // A sibling of the body, not a child of a row: the body clips, and a
        // menu inside it would be cut off at the column's edge. Same argument
        // the tab strip's `+` menu makes about its own rail.
        .children(menu(scores, app))
        .agent_node(Role::Card, "Scores level")
        .into_any_element()
}

/// The context menu over the right-clicked score, if one is up.
fn menu(scores: &Scores, app: &Entity<Luma>) -> Option<AnyElement> {
    let open = scores.menu.as_ref()?;
    let track_id = SharedString::from(scores.track.id.clone());
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

/// The track this level is about, under the way back to the list it came from.
fn head(scores: &Scores, app: &Entity<Luma>, window: &Window, flying: bool) -> Div {
    let back = app.clone();
    div()
        .flex()
        .flex_shrink_0()
        .flex_col()
        .child(
            div()
                .id("scores-back")
                .track_focus(&scores.back_focus)
                .tab_stop(true)
                .h(px(BACK_ROW_HEIGHT))
                .px(px(PAD_X))
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(11.))
                .text_color(glass::ink(0.55))
                .hover(|row| row.text_color(glass::ink(0.9)))
                .on_click(move |_, _, cx| {
                    back.update(cx, |this, cx| this.leave_scores(cx));
                })
                .child(
                    gpui_component::Icon::new(gpui_component::IconName::ChevronLeft).size(px(11.)),
                )
                .child("Tracks")
                .agent_node(Role::Button, "Back to tracks")
                .agent_focused(scores.back_focus.is_focused(window)),
        )
        // While the shared element is in flight it *is* this row; drawing both
        // would be two of the same track on one column.
        .child(track_face(&scores.track, true).when(flying, |row| row.opacity(0.)))
}

/// This venue's scores, the way to mint another, and the other venues' as
/// context under a divider.
fn body(state: &super::Tracks, scores: &Scores, open: Option<&str>, app: &Entity<Luma>) -> Div {
    let venue = state.venue_id();
    let here: Vec<&ScoreRow> = scores.here(venue).collect();
    div()
        .flex_1()
        .min_h(px(0.))
        .overflow_hidden()
        .flex()
        .flex_col()
        .px(px(PAD_X - float::ROW_INSET))
        .pt(px(6.))
        .gap(px(2.))
        .children(scores.error.clone().map(float::error_row))
        .when(scores.error.is_none() && !scores.loaded, |el| {
            el.child(float::empty_row("Loading scores…"))
        })
        .when(scores.loaded && here.is_empty(), |el| {
            el.child(float::empty_row(format!(
                "No scores in {}",
                state.venue_name()
            )))
        })
        .children(here.into_iter().map(|row| {
            score_row(
                row,
                open == Some(row.id.as_ref()),
                Some((app, &scores.track)),
            )
        }))
        .when(scores.loaded, |el| el.child(new_score(app)))
        .children(elsewhere(scores, venue).map(|(venue, rows)| {
            div()
                .flex()
                .flex_col()
                .pt(px(8.))
                .child(float::divider())
                .child(float::section_heading(venue).pt(px(8.)))
                .children(rows.into_iter().map(|row| score_row(row, false, None)))
        }))
        .child(div().flex_1())
}

/// Every other venue's rows, grouped, in the order the venues first appear —
/// which is newest-created first, so the room with the newest score leads.
fn elsewhere<'a>(
    scores: &'a Scores,
    venue: &str,
) -> impl Iterator<Item = (SharedString, Vec<&'a ScoreRow>)> {
    let mut order: Vec<SharedString> = Vec::new();
    let mut groups: std::collections::HashMap<SharedString, Vec<&ScoreRow>> =
        std::collections::HashMap::new();
    for row in scores.rows.iter().filter(|row| row.venue_id != venue) {
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
/// `app` present means the row is openable; `None` is another venue's score,
/// which is readable here and openable only as its own tab. Comet's selection
/// recipe decides the open one, through [`float::menu_row`]: hover and
/// selection share the fill, and only the open score carries the inset ring.
fn score_row(
    row: &ScoreRow,
    open: bool,
    press: Option<(&Entity<Luma>, &TrackBrowserRow)>,
) -> AnyElement {
    let id = SharedString::from(format!("score-{}", row.id));
    let revisions = if row.revisions > 1 {
        format!(" · {} rev", row.revisions)
    } else {
        String::new()
    };
    let label = format!(
        "#{} · {} · {} clips · {}{revisions}{}{}",
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
        let score = open_row(row);
        let raised = app.clone();
        let (score_id, venue_id, ordinal, clips) =
            (row.id.clone(), row.venue_id.clone(), row.ordinal, row.clips);
        // Right-click is the only gesture in the sidebar that opens a menu, so
        // it is filtered on the button here rather than routed through a shared
        // press handler. `stop_propagation` keeps it off the column behind it.
        el.on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
            cx.stop_propagation();
            let menu = ScoreMenu {
                at: event.position,
                score_id: score_id.clone(),
                ordinal,
                venue_id: venue_id.clone(),
                clips,
            };
            raised.update(cx, |this, cx| this.open_score_menu(menu, cx));
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
            app.update(cx, |this, cx| this.open_sidebar_score(track_id, score, cx));
        })
    })
    // The handle leads: a score's own name is usually absent and its uuid
    // is unsayable, so `#2` is the only thing two people can both point at.
    .child(
        div()
            .flex_shrink_0()
            .text_size(px(11.))
            .font_weight(FontWeight::BOLD)
            .text_color(glass::ink(if open { 0.95 } else { 0.6 }))
            .child(format!("#{}", row.ordinal)),
    )
    .child(
        div()
            .flex_1()
            .min_w(px(0.))
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap(px(1.))
            .child(
                div()
                    .truncate()
                    .text_size(px(12.))
                    .text_color(glass::ink(if open { 0.9 } else { 0.7 }))
                    .child(row.name.clone().unwrap_or_else(|| row.author.clone())),
            )
            .child(
                div()
                    .truncate()
                    .text_size(px(10.))
                    .text_color(glass::ink(0.45))
                    .child(format!(
                        "{} clips · {}{revisions}{}",
                        row.clips, row.age, row.spend
                    )),
            ),
    )
    .when(row.read_only, |el| {
        el.child(luma_ui::silkscreen("RO".to_string()))
    })
    .agent_node(Role::Row, label)
    .into_any_element()
}

/// Mint another score on this `(track, venue)`.
fn new_score(app: &Entity<Luma>) -> AnyElement {
    let app = app.clone();
    float::menu_row(RowState::Rest, "new-score")
        .id("new-score")
        .on_click(move |_, _, cx| app.update(cx, |this, cx| this.create_sidebar_score(cx)))
        .child(
            gpui_component::Icon::new(gpui_component::IconName::Plus)
                .size(px(11.))
                .text_color(glass::ink(0.55)),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(12.))
                .text_color(glass::ink(0.7))
                .child("New score"),
        )
        .agent_node(Role::Button, "New score")
        .into_any_element()
}
