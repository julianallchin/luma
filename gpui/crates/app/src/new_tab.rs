//! The tab strip's `+`: a combo box over the venue's songs and its venue tab
//! (`docs/specs/venue-tabs.md` rule 9).
//!
//! The root level lists "Venue" and every track in the venue. A track goes one
//! level deeper, to its scores in this venue; a score or "Venue" opens that
//! tab, or brings it to the front, and closes the box. A track with one score
//! still goes a level deeper: the same keys always do the same thing.
//!
//! The box is [`luma_ui::combo`]; this module says what is in it and what a
//! pick does.

use gpui::*;
use gpui_component::Icon;
use luma_lib::models::tracks::TrackBrowserRow;
use luma_ui::combo::{self, Combo, Level};
use luma_ui::float;
use luma_ui::icons::IconName;
use luma_ui::{glass, radius};

use crate::tracks::{album_art, scores};
use crate::Luma;

/// One row of the box.
#[derive(Clone)]
pub(crate) enum Entry {
    /// The venue tab. `name` is the venue's, shown under the word.
    Venue { name: SharedString },
    /// A track, which opens its scores.
    Track(TrackBrowserRow),
    /// One score of `track` in this venue.
    Score {
        track: SharedString,
        score: crate::track_editor::Score,
        title: SharedString,
        detail: SharedString,
    },
}

/// The lead box every row has, so the titles share one left edge.
const LEAD: f32 = 32.0;

impl combo::Row for Entry {
    fn key(&self) -> SharedString {
        match self {
            Self::Venue { .. } => "new-tab:venue".into(),
            Self::Track(track) => format!("new-tab:track:{}", track.id).into(),
            Self::Score { score, .. } => format!("new-tab:score:{}", score.id).into(),
        }
    }

    fn label(&self) -> SharedString {
        match self {
            Self::Venue { .. } => "Venue".into(),
            Self::Track(track) => crate::tracks::track_name(track).into(),
            Self::Score { score, title, .. } => format!("#{} · {title}", score.ordinal).into(),
        }
    }

    /// Venue by its word or its name; a track the way the add-track dialog
    /// and the sidebar match one; a score by its handle and its title.
    fn matches(&self, query: &str) -> bool {
        match self {
            Self::Venue { name } => "venue".contains(query) || name.to_lowercase().contains(query),
            Self::Track(track) => crate::tracks::matches(track, query),
            Self::Score { .. } => self.label().to_lowercase().contains(query),
        }
    }

    fn content(&self) -> AnyElement {
        let (lead, title, subtitle) = match self {
            Self::Venue { name } => (glyph(IconName::Cpu), "Venue".into(), Some(name.clone())),
            Self::Track(track) => (
                album_art(track.album_art_path.as_deref(), LEAD).into_any_element(),
                SharedString::from(crate::tracks::track_name(track)),
                track.artist.clone().map(SharedString::from),
            ),
            Self::Score {
                score,
                title,
                detail,
                ..
            } => (
                lead_box()
                    .text_size(px(11.0))
                    .font_weight(FontWeight::BOLD)
                    .child(format!("#{}", score.ordinal))
                    .into_any_element(),
                title.clone(),
                Some(detail.clone()),
            ),
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(lead)
            .child(float::row_text(title, subtitle))
            .into_any_element()
    }
}

/// The square a row's lead sits in, the album art's size.
fn lead_box() -> Div {
    div()
        .flex_none()
        .size(px(LEAD))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius::ROW))
        .bg(glass::ink(0.06))
        .text_color(glass::ink(0.7))
}

fn glyph(icon: IconName) -> AnyElement {
    lead_box()
        .child(Icon::new(icon).size(px(15.0)))
        .into_any_element()
}

/// The open box, and where the keyboard was when it opened.
pub(crate) struct NewTab {
    combo: Entity<Combo<Entry>>,
    /// The keyboard goes back here when the box closes without opening a
    /// tab. The box's field is the only focus inside it, so closing it would
    /// otherwise leave the keyboard on an element no frame draws.
    return_focus: Option<WeakFocusHandle>,
    _events: Subscription,
}

impl NewTab {
    pub(crate) fn combo(&self) -> Entity<Combo<Entry>> {
        self.combo.clone()
    }
}

impl Luma {
    /// Press the `+`: open the box on the venue's songs, or close it.
    pub(crate) fn toggle_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_tab.is_some() {
            self.close_new_tab(window, cx);
            return;
        }
        let Some(browser) = &self.sidebar else {
            return;
        };
        let rows = std::iter::once(Entry::Venue {
            name: browser.venue_name().to_string().into(),
        })
        .chain(browser.venue_rows().cloned().map(Entry::Track))
        .collect();
        let root = Level::new(
            "root",
            "Open a song or the venue…",
            "No songs in this venue",
        )
        .with_rows(rows);
        let combo = cx.new(|cx| Combo::new("New tab", root, cx));
        let events = cx.subscribe_in(&combo, window, Self::new_tab_event);
        self.new_tab = Some(NewTab {
            combo,
            return_focus: window.focused(cx).map(|focus| focus.downgrade()),
            _events: events,
        });
        cx.notify();
    }

    /// Close the box and hand the keyboard back to where it was.
    pub(crate) fn close_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(new_tab) = self.new_tab.take() else {
            return;
        };
        match new_tab.return_focus.and_then(|focus| focus.upgrade()) {
            Some(focus) => window.focus(&focus, cx),
            None => window.focus(&self.focus, cx),
        }
        cx.notify();
    }

    fn new_tab_event(
        &mut self,
        combo: &Entity<Combo<Entry>>,
        event: &combo::Event<Entry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            combo::Event::Dismiss => self.close_new_tab(window, cx),
            combo::Event::Pick(Entry::Venue { .. }) => {
                self.close_new_tab(window, cx);
                self.open_venue_tab(None, cx);
            }
            combo::Event::Pick(Entry::Score { track, score, .. }) => {
                self.close_new_tab(window, cx);
                self.open_score(track.as_ref(), score.clone(), None, cx);
            }
            combo::Event::Pick(Entry::Track(track)) => {
                self.show_new_tab_scores(combo.clone(), track, cx)
            }
        }
    }

    /// Go one level into `track`: its scores in this venue, newest first —
    /// the order the sidebar lists them in.
    fn show_new_tab_scores(
        &mut self,
        combo: Entity<Combo<Entry>>,
        track: &TrackBrowserRow,
        cx: &mut Context<Self>,
    ) {
        let Some(browser) = &self.sidebar else {
            return;
        };
        let venue = browser.venue_id().to_string();
        let name = crate::tracks::track_name(track);
        combo.update(cx, |combo, cx| {
            combo.push(
                Level::new(
                    track.id.clone(),
                    format!("Scores of {name}…"),
                    "No scores in this venue",
                ),
                cx,
            );
        });
        let listing = self.library.scores_across_venues(&track.id);
        let user = self.library.user_id();
        let track = SharedString::from(track.id.clone());
        // Weak: a box closed before the listing lands is gone, not kept.
        let combo = combo.downgrade();
        cx.spawn(async move |_, cx| {
            let rows = listing
                .await
                .map(|summaries| {
                    scores::rows(&summaries, user.as_deref())
                        .iter()
                        .filter(|row| row.venue_id.as_ref() == venue)
                        .map(|row| Entry::Score {
                            track: track.clone(),
                            score: scores::open_row(row),
                            title: row.title(),
                            detail: row.detail(),
                        })
                        .collect()
                })
                .map_err(|error| error.to_string());
            combo
                .update(cx, |combo, cx| combo.fill(&track, rows, cx))
                .ok();
        })
        .detach();
    }
}
