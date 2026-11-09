//! The stage's title in Arrange: the open song and score, centred over the
//! top of the stage, and the menu hung off it.
//!
//! The menu goes two places. Up, to the song list. Across, to any other score
//! of the same song — this venue's, and the ones in other venues, since "the
//! same song, scored in the warehouse too" is what the operator cannot
//! otherwise find from a timeline. A score in this venue opens on the spot;
//! one in another venue asks first, because taking it means the whole window
//! switching venue (`docs/specs/comet-shell.md` §6).
//!
//! The listing is the song list's own ([`Library::scores_across_venues`],
//! resolved by [`crate::tracks::scores::rows`]), read fresh on every opening
//! like the venue menu's.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use luma_ui::float::{self, RowState};
use luma_ui::glass;
use luma_ui::icons::IconName;
use luma_ui::node::{Instrument as _, Role};

use crate::shell::Body;
use crate::tracks::scores::{elsewhere, open_row, ScoreRow};
use crate::workspace::{Mode, Target};
use crate::Luma;

/// The menu while it is open or leaving: the song it is about, and that
/// song's scores in every venue, once they are read.
pub(crate) struct ScoreSwitcher {
    track_id: String,
    /// The song's scores; `Err` when they could not be read, the cause logged.
    rows: Option<Result<Rc<[ScoreRow]>, ()>>,
}

/// How wide the menu opens.
const MENU_WIDTH: f32 = 300.;
/// How tall the trigger is: the stage's corner controls' height, so the three
/// along the stage's top edge share one line.
const TRIGGER_HEIGHT: f32 = 32.;
/// How far the trigger sits from the stage's top edge — the corner controls'
/// inset, for the same reason.
pub(crate) const TRIGGER_TOP: f32 = 16.;
/// The room the trigger takes from the top of the stage, its inset included:
/// where anything else centred along the stage's top edge has to start.
pub(crate) const TRIGGER_ROOM: f32 = TRIGGER_TOP + TRIGGER_HEIGHT;

impl Luma {
    /// Open or close the menu, reading the open song's scores afresh.
    pub(crate) fn toggle_score_switcher(&mut self, cx: &mut Context<Self>) {
        if self.score_switcher.is_open() {
            self.close_score_switcher(cx);
            return;
        }
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body() else {
            return;
        };
        let track_id = editor.track_id().to_string();
        self.score_switcher.open(ScoreSwitcher {
            track_id: track_id.clone(),
            rows: None,
        });
        let pending = self.library.scores_across_venues(&track_id);
        let user = self.library.user_id();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let listing = pending.await;
            this.update(cx, |this, cx| {
                // An answer about another song is a stale one: the menu was
                // closed and reopened over a different score meanwhile.
                if let Some(menu) = this
                    .score_switcher
                    .open_mut()
                    .filter(|menu| menu.track_id == track_id)
                {
                    menu.rows = Some(
                        listing
                            .map(|summaries| {
                                crate::tracks::scores::rows(&summaries, user.as_deref())
                            })
                            .map_err(|error| {
                                log::warn!("Could not load the song's scores: {error}");
                            }),
                    );
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Begin the menu's exit. Reports whether it was open — Escape's rung.
    pub(crate) fn close_score_switcher(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.score_switcher.is_open() {
            return false;
        }
        self.score_switcher.begin_close(cx);
        cx.notify();
        true
    }

    /// The menu's "Song list" row: the page of every song, over Arrange.
    fn score_switcher_to_songs(&mut self, cx: &mut Context<Self>) {
        self.close_score_switcher(cx);
        if !self.song_list_shown() {
            self.toggle_song_list(cx);
        }
    }

    /// A score chosen from the menu: opened here when it is this venue's,
    /// asked about first when it is another's.
    fn switch_to_listed_score(&mut self, score_id: &str, cx: &mut Context<Self>) {
        let chosen = self.score_switcher.get().and_then(|menu| {
            let row = menu
                .rows
                .as_ref()?
                .as_ref()
                .ok()?
                .iter()
                .find(|row| row.id.as_ref() == score_id)?;
            Some((
                menu.track_id.clone(),
                row.venue_id.clone(),
                row.venue.clone(),
                open_row(row),
            ))
        });
        self.close_score_switcher(cx);
        let Some((track_id, venue_id, venue, score)) = chosen else {
            return;
        };
        let Some(songs) = &self.songs else {
            return;
        };
        if songs.venue_id() == venue_id.as_ref() {
            self.open_score(&track_id, score, None, cx);
            return;
        }
        self.ask_to_switch_venue(venue_id, venue, &track_id, score, cx);
    }

    /// Ask before opening `score` of `track_id` in another venue, `venue_id`
    /// named `venue`: taking it switches the whole window there. The stage's
    /// menu and the song list both ask this one question.
    pub(crate) fn ask_to_switch_venue(
        &mut self,
        venue_id: SharedString,
        venue: SharedString,
        track_id: &str,
        score: crate::track_editor::Score,
        cx: &mut Context<Self>,
    ) {
        let Some(songs) = &self.songs else {
            return;
        };
        let here = songs.venue_name().to_string();
        self.ask(
            crate::confirm::Confirm {
                title: format!("Switch to {venue}?").into(),
                body: format!(
                    "The window will switch to {venue} and open score #{} there. \
                     {here} stays as you left it.",
                    score.ordinal
                )
                .into(),
                verb: "Switch".into(),
                action: crate::confirm::Action::SwitchVenue {
                    venue_id,
                    track_id: track_id.to_string().into(),
                    score,
                },
            },
            cx,
        );
    }

    /// Switch the window to `venue_id` and open `score` of `track_id` there,
    /// in Arrange: the confirmed half of choosing another venue's score. The
    /// score opens once the venue's catalogue has landed — see
    /// [`crate::tracks::Tracks::arrival`].
    pub(crate) fn open_score_in_venue(
        &mut self,
        venue_id: SharedString,
        track_id: SharedString,
        score: crate::track_editor::Score,
        cx: &mut Context<Self>,
    ) {
        let venues = self.library.venues();
        cx.spawn(async move |this, cx| {
            let venues = venues.await;
            this.update(cx, |this, cx| {
                let venue = match venues {
                    Ok(venues) => venues
                        .into_iter()
                        .find(|venue| venue.id == venue_id.as_ref()),
                    Err(error) => {
                        this.report_failure("Could not switch to that venue", error, cx);
                        return;
                    }
                };
                let Some(venue) = venue else {
                    return;
                };
                this.open_venue(venue, cx);
                if let Some(songs) = &mut this.songs {
                    songs.arrival = Some((track_id.to_string(), score));
                }
            })
            .ok();
        })
        .detach();
    }
}

/// The trigger, centred along the top of the stage, with the menu hung off
/// it: the open score's song and `#n`, and its album art when it has some.
/// `None` unless Arrange has a score open.
pub(crate) fn trigger(app: &Luma, entity: &Entity<Luma>, cx: &App) -> Option<AnyElement> {
    if app.workspace.mode() != Mode::Arrange {
        return None;
    }
    let Some(Body::TrackEditor(editor)) = app.workspace.active_body() else {
        return None;
    };
    let art = app
        .songs
        .as_ref()
        .and_then(|songs| songs.find(editor.track_id()))
        .and_then(|track| track.album_art_path);
    let title = editor.track_name().to_string();
    let score = editor.score_label();
    let open = app.score_switcher.is_open();
    let pressed = entity.clone();
    let button = luma_ui::button("", luma_ui::Enabled::Yes)
        .id("score-switcher")
        .relative()
        .h(px(TRIGGER_HEIGHT))
        .gap(px(8.))
        .max_w(px(420.))
        // Opaque over the stage, so the pointer it covers is its own and not
        // the camera's.
        .occlude()
        .on_click(move |_, _, cx| pressed.update(cx, |this, cx| this.toggle_score_switcher(cx)))
        .children(
            art.is_some()
                .then(|| crate::tracks::album_art(art.as_deref(), 20.)),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(glass::ink(0.95))
                .child(title.clone()),
        )
        .child(div().text_color(glass::ink(0.4)).child("/"))
        .child(
            div()
                .flex_none()
                .text_color(glass::ink(0.6))
                .child(score.clone()),
        )
        .child(
            gpui_component::Icon::new(if open {
                IconName::ChevronUp
            } else {
                IconName::ChevronDown
            })
            .size(px(11.))
            .text_color(glass::ink(0.42)),
        )
        .children(menu(app, editor.target(), entity, cx))
        .agent_node(Role::Button, format!("{title} · {score}"));
    Some(
        div()
            .absolute()
            .top(px(TRIGGER_TOP))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(button)
            .into_any_element(),
    )
}

/// The menu: the song list, then the song's scores — this venue's first,
/// then each other venue's under its name — with the open one checked.
fn menu(app: &Luma, open: Target, entity: &Entity<Luma>, cx: &App) -> Option<AnyElement> {
    let menu = app.score_switcher.get()?;
    let closing = app.score_switcher.closing_since();
    let pressable = closing.is_none();
    let here = app.songs.as_ref()?.venue_id().to_string();
    let open_score = match &open {
        Target::Score { score, .. } => Some(score.clone()),
        _ => None,
    };
    let songs = {
        let pressed = entity.clone();
        float::menu_row(RowState::Rest, "score-switcher-songs")
            .id("score-switcher-songs")
            .when(pressable, |row| {
                row.on_click(move |_, _, cx| {
                    pressed.update(cx, |this, cx| this.score_switcher_to_songs(cx))
                })
            })
            .child(div().flex_1().child("Song list"))
            .children(luma_ui::shortcut::cap(&crate::keymap::ToggleSongList, cx))
            .agent_node(Role::Row, "Song list")
    };
    let row = |row: &ScoreRow, elsewhere: bool| {
        let pressed = entity.clone();
        let id = row.id.clone();
        let label = if elsewhere {
            format!("#{} {} · {}", row.ordinal, row.title(), row.venue)
        } else {
            format!("#{} {}", row.ordinal, row.title())
        };
        float::menu_row(RowState::Rest, format!("score-switcher:{}", row.id))
            .id(SharedString::from(format!("score-switcher:{}", row.id)))
            .when(pressable, |el| {
                el.on_click(move |_, _, cx| {
                    pressed.update(cx, |this, cx| this.switch_to_listed_score(&id, cx))
                })
            })
            .child(
                div()
                    .flex_none()
                    .text_color(glass::ink(0.6))
                    .child(format!("#{}", row.ordinal)),
            )
            .child(div().flex_1().min_w_0().truncate().child(row.title()))
            .child(float::check(open_score.as_deref() == Some(row.id.as_ref())))
            .agent_node(Role::Row, label)
    };
    let mut card = float::popover_card().w(px(MENU_WIDTH)).child(songs);
    match &menu.rows {
        None => card = card.child(float::empty_row("Loading scores…")),
        Some(Err(())) => card = card.child(float::empty_row("Could not load the scores")),
        Some(Ok(rows)) => {
            let venue_name = app
                .songs
                .as_ref()
                .map(|songs| songs.venue_name().to_string());
            card = card
                .child(rule())
                .child(float::section_heading(venue_name.unwrap_or_default()))
                .children(
                    rows.iter()
                        .filter(|listed| listed.venue_id.as_ref() == here)
                        .map(|listed| row(listed, false)),
                );
            for (venue, listed) in elsewhere(rows, &here) {
                card = card
                    .child(rule())
                    .child(float::section_heading(venue))
                    .children(listed.into_iter().map(|listed| row(listed, true)));
            }
        }
    }
    let card = card
        .agent_node(Role::Card, "Scores menu")
        .into_any_element();
    Some(match closing {
        Some(since) => float::anchored_below_closing(
            "score-switcher-menu",
            TRIGGER_HEIGHT,
            card,
            luma_ui::motion::exit_progress(&luma_ui::motion::MENU_OUT, since),
        ),
        None => {
            let dismiss = entity.clone();
            float::anchored_below(
                "score-switcher-menu",
                TRIGGER_HEIGHT,
                float::Dismiss::on_press_out(move |_, cx| {
                    dismiss.update(cx, |this, cx| {
                        this.close_score_switcher(cx);
                    });
                }),
                card,
            )
        }
    })
}

/// A hairline between the menu's sections.
fn rule() -> Div {
    div().my(px(4.)).h(px(1.)).bg(glass::inner_rule())
}
