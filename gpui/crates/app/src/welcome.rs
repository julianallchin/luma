//! The welcome screen: the wordmark over a grid of venue cards.
//!
//! Mirrors `src/features/app/components/welcome-screen.tsx` and
//! `src/features/venues/components/venue-list.tsx` — the gutter ground, the
//! centered column, the 3×2 grid of 144px cards, the name / description /
//! updated-date stack, and the dashed placeholders that keep the grid a
//! constant shape when there are fewer than six venues.

use gpui::prelude::FluentBuilder;
use gpui::*;
use luma_ui::ladder;
use luma_ui::node::{Instrument, Role};
use luma_ui::Enabled;

use luma_lib::models::venues::Venue;

/// The card is the primitive and the grid is derived from it, not the other
/// way round. Deriving the card from `w-2xl` (672px / 3) is what CSS grid
/// does, but a flex-wrap container has to compare *summed* item widths against
/// its own, and 3 × 213.33 rounds a hair over 672 — which silently wraps the
/// third card onto the next row. Sizing the card in whole pixels and adding
/// the gaps back keeps that comparison exact.
const CARD_WIDTH: f32 = 213.;
const CARD_HEIGHT: f32 = 144.;
const GAP: f32 = 16.;
const COLUMNS: usize = 3;
const ROWS: usize = 2;
const SLOTS: usize = COLUMNS * ROWS;
const GRID_WIDTH: f32 = CARD_WIDTH * COLUMNS as f32 + GAP * (COLUMNS as f32 - 1.);

/// Render the screen. `on_open` receives the venue id of a clicked card;
/// `on_patterns` opens the pattern list, which hangs off this screen rather
/// than off a venue because a pattern belongs to the library.
pub fn welcome(
    venues: &[Venue],
    error: Option<&str>,
    on_open: impl Fn(&str, &mut Window, &mut App) + Clone + 'static,
    on_patterns: impl Fn(&mut Window, &mut App) + 'static,
) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(32.))
        // No ground of its own: the picker is *cards on the shell*, and the
        // overlay plane under it is a scrim over the glass. A fill here would
        // paint the shell out — and since this overlay is the app's opening
        // state, it would be the launch screen deciding the app has no glass.
        .text_color(ladder::foreground())
        .child(wordmark())
        .child(match error {
            Some(message) => failure(message).into_any_element(),
            None => grid(venues, on_open).into_any_element(),
        })
        .child(
            luma_ui::luma_button("Patterns", Enabled::Yes)
                .id("patterns")
                .on_click(move |_, window, cx| on_patterns(window, cx))
                .agent_node(Role::Button, "Patterns"),
        )
}

/// `text-6xl font-extralight tracking-[0.2em] opacity-80`. GPUI has no
/// letter-spacing yet — the same gap the harness documents for `tracking-
/// wider` on buttons — so the wordmark is ~0.2em/glyph tighter than the web's.
fn wordmark() -> impl IntoElement {
    div()
        .text_size(px(60.))
        .font_weight(FontWeight::EXTRA_LIGHT)
        .text_color(ladder::foreground_alpha(0.8))
        .child("luma")
        .agent_node(Role::Text, "luma")
}

fn failure(message: &str) -> impl IntoElement {
    div()
        .w(px(GRID_WIDTH))
        .p(px(16.))
        .bg(ladder::background())
        .border_1()
        .border_color(ladder::border())
        .text_size(px(12.))
        .text_color(ladder::danger())
        .child(format!("Failed to load venues: {message}"))
        .agent_node(Role::Text, format!("Failed to load venues: {message}"))
}

fn grid(venues: &[Venue], on_open: impl Fn(&str, &mut Window, &mut App) + Clone + 'static) -> Div {
    let shown: Vec<&Venue> = venues.iter().take(SLOTS).collect();
    let empty = SLOTS - shown.len();
    div()
        .w(px(GRID_WIDTH))
        .flex()
        .flex_wrap()
        .gap(px(GAP))
        .children(shown.into_iter().map(|venue| card(venue, on_open.clone())))
        .children((0..empty).map(|_| placeholder()))
}

fn card(
    venue: &Venue,
    on_open: impl Fn(&str, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let id = SharedString::from(venue.id.clone());
    let open_id = venue.id.clone();
    div()
        .id(ElementId::Name(id))
        .w(px(CARD_WIDTH))
        .h(px(CARD_HEIGHT))
        .p(px(16.))
        .flex()
        .flex_col()
        .justify_between()
        .bg(ladder::background())
        .border_1()
        .border_color(ladder::border())
        .hover(|s| s.bg(ladder::hover()))
        .on_click(move |_, window, cx| on_open(&open_id, window, cx))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(14.))
                                .font_weight(FontWeight::MEDIUM)
                                .child(venue.name.clone()),
                        )
                        .when(venue.is_member(), |el| el.child(joined_badge())),
                )
                .when_some(venue.description.clone(), |el, description| {
                    el.child(
                        div()
                            .text_size(px(12.))
                            .text_color(ladder::muted_foreground())
                            .child(description),
                    )
                }),
        )
        .child(
            div()
                .text_size(px(10.))
                .text_color(ladder::muted_foreground())
                .child(local_date(&venue.updated_at)),
        )
        .agent_node(Role::Card, venue.name.clone())
}

/// The `joined` chip on a venue this user is a member of rather than owner of.
fn joined_badge() -> Div {
    div()
        .px(px(6.))
        .py(px(2.))
        .bg(ladder::foreground_alpha(0.1))
        .text_size(px(9.))
        .text_color(ladder::muted_foreground())
        .child("joined")
}

/// An unfilled slot. The web side draws a dashed border; GPUI has no dashed
/// border style, so the slot is a value step down from a card instead — same
/// job (this space is empty, the grid is still 3×2), same ladder.
fn placeholder() -> Div {
    div()
        .w(px(CARD_WIDTH))
        .h(px(CARD_HEIGHT))
        .bg(ladder::trim())
        .border_1()
        .border_color(ladder::trim())
}

/// `new Date(updatedAt).toLocaleDateString()`, near enough: the timestamps are
/// stored as `YYYY-MM-DD HH:MM:SS`, so the date is the leading 10 characters.
/// Reformatted to `M/D/YYYY` to match the web side's US-locale rendering.
fn local_date(timestamp: &str) -> String {
    let date = timestamp.get(..10).unwrap_or(timestamp);
    let mut parts = date.split('-');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(year), Some(month), Some(day)) if year.len() == 4 => {
            format!(
                "{}/{}/{year}",
                month.trim_start_matches('0'),
                day.trim_start_matches('0')
            )
        }
        _ => date.to_string(),
    }
}
