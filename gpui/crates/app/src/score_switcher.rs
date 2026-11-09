//! The title bar's song and score crumbs in Arrange — `venue / song / score`
//! — and the score crumb's menu.
//!
//! The song crumb goes up, to the song list with the song selected. The score
//! crumb's menu goes across, to any other score of the same song — this
//! venue's, and the ones in other venues, since "the same song, scored in the
//! warehouse too" is what the operator cannot otherwise find from a timeline.
//! A score in this venue opens on the spot; one in another venue asks first,
//! because taking it means the whole window switching venue
//! (`docs/specs/comet-shell.md` §6).
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
use crate::tracks::scores::{elsewhere, name_field, open_row, Rename, RenameHost, ScoreRow};
use crate::workspace::{Mode, Target};
use crate::Luma;

/// The menu while it is open or leaving: the song it is about, and that
/// song's scores in every venue, once they are read.
pub(crate) struct ScoreSwitcher {
    track_id: String,
    /// The song's scores; `Err` when they could not be read, the cause logged.
    rows: Option<Result<Rc<[ScoreRow]>, ()>>,
    /// The open score's name being typed, in its row — the song list's
    /// rename, held here.
    rename: Option<Rename>,
}

impl ScoreSwitcher {
    /// The rename, taken to be ended, with the song it is about.
    pub(crate) fn take_rename(&mut self) -> Option<(String, Rename)> {
        Some((self.track_id.clone(), self.rename.take()?))
    }
}

/// How wide the menu opens.
const MENU_WIDTH: f32 = 300.;

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
            rename: None,
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

    /// The menu's "New score": an empty score of this song, here, opened.
    fn new_score_from_menu(&mut self, cx: &mut Context<Self>) {
        let Some(track_id) = self.score_switcher.get().map(|menu| menu.track_id.clone()) else {
            return;
        };
        self.close_score_switcher(cx);
        self.create_listed_score(&track_id, cx);
    }

    /// The menu's "Rename": the open score's row becomes its name, editable.
    fn rename_from_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body() else {
            return;
        };
        let score_id: SharedString = editor.target_score_id().into();
        let opened_on = editor.score_name().unwrap_or_default().to_string();
        let rename = Rename::begin(RenameHost::ScoreMenu, score_id, opened_on, window, cx);
        let focus = rename.field.read(cx).focus_handle(cx);
        if let Some(menu) = self.score_switcher.open_mut() {
            menu.rename = Some(rename);
        }
        window.focus(&focus, cx);
        cx.notify();
    }

    /// The menu's "Duplicate": the open score and its clips copied into a new
    /// score here, `<name> copy`, opened.
    fn duplicate_from_menu(&mut self, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body() else {
            return;
        };
        let (track_id, score_id) = (
            editor.track_id().to_string(),
            editor.target_score_id().to_string(),
        );
        let title = editor.score_title();
        self.close_score_switcher(cx);
        self.duplicate_score(&track_id, &score_id, &title, cx);
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
    /// named `venue`: taking it switches the whole window there. The song crumb's
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

/// The crumbs that follow the venue's on the title bar's trail, in Arrange.
/// With a score open: its song, which goes up a level to the song list, and
/// the score by name, with its menu hung off it. With none: the song the list
/// has selected, alone. `None` when there is no song to name.
pub(crate) fn crumbs(
    app: &Luma,
    entity: &Entity<Luma>,
) -> Option<(AnyElement, Option<AnyElement>)> {
    if app.workspace.mode() != Mode::Arrange {
        return None;
    }
    let editor = match app.workspace.active_body() {
        Some(Body::TrackEditor(editor)) => Some(editor),
        _ => None,
    };
    let (track_id, title) = match editor {
        Some(editor) => (
            editor.track_id().to_string(),
            editor.track_name().to_string(),
        ),
        None if app.song_list_shown() => {
            let row = app.songs.as_ref()?.selected_row()?;
            (row.id.clone(), crate::tracks::track_name(&row))
        }
        None => return None,
    };
    let revealed = entity.clone();
    let song = float::nav_row(RowState::Rest, "song-crumb")
        .id("song-crumb")
        .flex_shrink(1.)
        .min_w_0()
        .max_w(px(320.))
        .text_size(px(13.))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| revealed.update(cx, |this, cx| this.reveal_song(&track_id, cx)))
        .child(crumb_label(
            title.clone(),
            if editor.is_some() { 0.6 } else { 0.95 },
        ))
        .agent_node(Role::Button, title.clone())
        .into_any_element();
    let score = editor.map(|editor| {
        let pressed = entity.clone();
        float::nav_row(
            RowState::of(app.score_switcher.is_open(), false),
            "score-switcher",
        )
        .id("score-switcher")
        .relative()
        .flex_shrink(1.)
        .min_w_0()
        .max_w(px(320.))
        .text_size(px(13.))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| pressed.update(cx, |this, cx| this.toggle_score_switcher(cx)))
        .child({
            let shown = editor.score_title();
            crumb_label(shown.clone(), 0.95).agent_node(Role::Text, shown)
        })
        .children(menu(app, editor.target(), entity))
        .agent_node(Role::Button, format!("{title} · {}", editor.score_label()))
        .into_any_element()
    });
    Some((song, score))
}

/// A crumb's label, truncated when the trail runs out of room.
fn crumb_label(text: impl Into<SharedString>, ink: f32) -> Div {
    div()
        .min_w_0()
        .truncate()
        .font_weight(FontWeight::MEDIUM)
        .text_color(glass::ink(ink))
        .child(text.into())
}

/// The menu: the song's scores — this venue's first, then each other venue's
/// under its name — with the open one checked.
fn menu(app: &Luma, open: Target, entity: &Entity<Luma>) -> Option<AnyElement> {
    let menu = app.score_switcher.get()?;
    let closing = app.score_switcher.closing_since();
    let pressable = closing.is_none();
    let here = app.songs.as_ref()?.venue_id().to_string();
    let open_score = match &open {
        Target::Score { score, .. } => Some(score.clone()),
        _ => None,
    };
    let renaming = menu
        .rename
        .as_ref()
        .map(|rename| (rename.score_id.clone(), rename.field.clone()));
    let row = |row: &ScoreRow, elsewhere: bool| {
        if let Some((_, field)) = renaming.as_ref().filter(|(id, _)| *id == row.id) {
            return div()
                .px(px(float::ROW_INSET))
                .py(px(2.))
                .child(name_field(row, field))
                .into_any_element();
        }
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
            .into_any_element()
    };
    let mut card = float::popover_card().w(px(MENU_WIDTH));
    match &menu.rows {
        None => card = card.child(float::empty_row("Loading scores…")),
        Some(Err(())) => card = card.child(float::empty_row("Could not load the scores")),
        Some(Ok(rows)) => {
            let venue_name = app
                .songs
                .as_ref()
                .map(|songs| songs.venue_name().to_string());
            card = card
                .child(float::section_heading(venue_name.unwrap_or_default()))
                .children(
                    rows.iter()
                        .filter(|listed| listed.venue_id.as_ref() == here)
                        .map(|listed| row(listed, false)),
                )
                .child(act(
                    "score-switcher-new",
                    "New score",
                    Some(IconName::Plus),
                    pressable,
                    entity,
                    |this, _, cx| this.new_score_from_menu(cx),
                ));
            for (venue, listed) in elsewhere(rows, &here) {
                card = card
                    .child(rule())
                    .child(float::section_heading(venue))
                    .children(listed.into_iter().map(|listed| row(listed, true)));
            }
        }
    }
    // What can be done to the open score itself.
    let card = card
        .child(rule())
        .child(act(
            "score-switcher-rename",
            "Rename",
            None,
            pressable && menu.rename.is_none(),
            entity,
            |this, window, cx| this.rename_from_menu(window, cx),
        ))
        .child(act(
            "score-switcher-duplicate",
            "Duplicate",
            None,
            pressable,
            entity,
            |this, _, cx| this.duplicate_from_menu(cx),
        ))
        .agent_node(Role::Card, "Scores menu")
        .into_any_element();
    Some(match closing {
        Some(since) => float::anchored_below_closing(
            "score-switcher-menu",
            float::NAV_ROW_HEIGHT,
            card,
            luma_ui::motion::exit_progress(&luma_ui::motion::MENU_OUT, since),
        ),
        None => {
            let dismiss = entity.clone();
            float::anchored_below(
                "score-switcher-menu",
                float::NAV_ROW_HEIGHT,
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

/// One of the menu's acts: a row with an optional leading glyph, which runs
/// `run` when pressed.
fn act(
    key: &'static str,
    label: &'static str,
    icon: Option<IconName>,
    pressable: bool,
    entity: &Entity<Luma>,
    run: impl Fn(&mut Luma, &mut Window, &mut Context<Luma>) + 'static,
) -> AnyElement {
    let pressed = entity.clone();
    float::menu_row(RowState::Rest, key)
        .id(key)
        .when(pressable, |row| {
            row.on_click(move |_, window, cx| pressed.update(cx, |this, cx| run(this, window, cx)))
        })
        .children(icon.map(|icon| {
            gpui_component::Icon::new(icon)
                .size(px(11.))
                .text_color(glass::ink(glass::TEXT_TERTIARY))
        }))
        .child(div().flex_1().child(label))
        .agent_node(Role::Row, label)
        .into_any_element()
}

/// A hairline between the menu's sections.
fn rule() -> Div {
    div().my(px(4.)).h(px(1.)).bg(glass::inner_rule())
}
