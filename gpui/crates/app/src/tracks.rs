//! The track browser: one venue's library as a striped table.
//!
//! Mirrors `src/features/tracks/components/track-browser.tsx` — the same
//! column order and widths, the same 10px uppercase silkscreen headers, the
//! same `bg-card` / `bg-stripe` alternation with a `--hover` lift. Read-only:
//! no selection, no sorting, no context menu.
//!
//! Album art comes from `album_art_path` — a path on disk, never inlined bytes
//! (see CLAUDE.md on why bulk responses carry paths). The web side has to route
//! that path through Tauri's asset protocol to get it past the webview; a
//! native host just reads the file, so `img(path)` is the whole story and
//! GPUI's image cache handles the decode and the lazy load.

use std::path::PathBuf;

use gpui::*;
use luma_ui::ladder;
use luma_ui::node::{Instrument, Role};

use luma_lib::models::tracks::TrackBrowserRow;

/// Row height. The web rows are content-sized by their 32px album-art cell.
const ROW_HEIGHT: f32 = 32.;
/// `grid-cols-[28px_56px_1fr_1fr_70px_60px_...]`, minus the columns this v0
/// does not render (status, added-by), which carry no data a reader needs.
const ART_WIDTH: f32 = 56.;
const BPM_WIDTH: f32 = 70.;
const TIME_WIDTH: f32 = 60.;
const GAP: f32 = 8.;
const PAD_X: f32 = 16.;

/// Render the browser for `venue_name`. `on_back` returns to the welcome
/// screen.
pub fn tracks(
    venue_name: &str,
    rows: &[TrackBrowserRow],
    error: Option<&str>,
    on_back: impl Fn(&mut Window, &mut App) + 'static,
) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(ladder::background())
        .text_color(ladder::foreground())
        .child(toolbar(venue_name, rows.len(), on_back))
        .child(header())
        .child(match error {
            Some(message) => plate(
                format!("Failed to load tracks: {message}"),
                rgb(0xf87171).into(),
            ),
            None if rows.is_empty() => plate(
                "No tracks imported".to_string(),
                ladder::muted_foreground().into(),
            ),
            None => body(rows).into_any_element(),
        })
}

/// Venue name, track count, and the way back.
fn toolbar(
    venue_name: &str,
    count: usize,
    on_back: impl Fn(&mut Window, &mut App) + 'static,
) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(12.))
        .px(px(PAD_X))
        .py(px(8.))
        .border_b_1()
        .border_color(ladder::trim())
        .child(
            luma_ui::luma_button("Back", false)
                .id("back")
                .on_click(move |_, window, cx| on_back(window, cx))
                .agent_node(Role::Button, "Back"),
        )
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .child(venue_name.to_string())
                .agent_node(Role::Text, venue_name.to_string()),
        )
        .child(
            div()
                .text_size(px(9.))
                .font_weight(FontWeight::BOLD)
                .text_color(ladder::muted_foreground())
                .child(format!("{count} TRACKS"))
                .agent_node(Role::Text, format!("{count} TRACKS")),
        )
}

/// The whole body when there is nothing to list: one centred line that says
/// so, named so a script can read the reason instead of inferring it from an
/// empty node list.
fn plate(message: String, color: gpui::Hsla) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.))
        .text_color(color)
        .child(message.clone())
        .agent_node(Role::Text, message)
        .into_any_element()
}

fn header() -> Div {
    row_shell()
        .flex_shrink_0()
        .py(px(8.))
        .border_b_1()
        .border_color(ladder::trim())
        .text_size(px(10.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ladder::muted_foreground())
        .child(art_cell().child(""))
        .child(flex_cell().child("TITLE"))
        .child(flex_cell().child("ARTIST"))
        .child(numeric_cell(BPM_WIDTH).child("BPM"))
        .child(numeric_cell(TIME_WIDTH).child("TIME"))
}

/// The scrolling rows. `uniform_list` virtualizes them, so a library of
/// thousands costs one screenful of elements — the same reason the web side
/// runs a virtualizer.
fn body(rows: &[TrackBrowserRow]) -> Div {
    let rows: Vec<TrackBrowserRow> = rows.to_vec();
    let count = rows.len();
    div().flex_1().overflow_hidden().child(
        uniform_list("tracks", count, move |range, _, _| {
            range.map(|index| track_row(index, &rows[index])).collect()
        })
        .size_full(),
    )
}

fn track_row(index: usize, track: &TrackBrowserRow) -> AnyElement {
    let stripe = if index.is_multiple_of(2) {
        ladder::background()
    } else {
        ladder::stripe()
    };
    let name = track_name(track);
    row_shell()
        .h(px(ROW_HEIGHT))
        .bg(stripe)
        .hover(|s| s.bg(ladder::hover()))
        .text_size(px(12.))
        .text_color(ladder::foreground_90())
        .child(art(track))
        .child(flex_cell().child(name.clone()))
        .child(
            flex_cell().child(
                track
                    .artist
                    .clone()
                    .unwrap_or_else(|| "Unknown artist".into()),
            ),
        )
        .child(
            numeric_cell(BPM_WIDTH)
                .font_family("SF Mono")
                .child(match track.bpm {
                    Some(bpm) => format!("{bpm:.1}"),
                    None => "--".into(),
                }),
        )
        .child(
            numeric_cell(TIME_WIDTH)
                .font_family("SF Mono")
                .child(duration(track.duration_seconds)),
        )
        .agent_node(Role::Row, name)
        .into_any_element()
}

/// The web side falls back through title → filename; `file_path`'s basename is
/// the same last resort.
fn track_name(track: &TrackBrowserRow) -> String {
    if let Some(title) = track.title.as_ref().filter(|t| !t.is_empty()) {
        return title.clone();
    }
    std::path::Path::new(&track.file_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| track.file_path.clone())
}

/// `formatDuration`: `M:SS`, `--:--` when unknown.
fn duration(seconds: Option<f64>) -> String {
    match seconds {
        Some(seconds) if seconds.is_finite() => {
            let total = seconds.max(0.);
            format!("{}:{:02}", (total / 60.) as u64, (total % 60.) as u64)
        }
        _ => "--:--".into(),
    }
}

// -- cell geometry, shared by the header and every row ------------------------

/// One row's box. `w_full` is load-bearing: a `uniform_list` item is laid out
/// against its own content unless it claims the list's width, and without it
/// the `flex_1` cells never expand and the stripe stops where the text does.
fn row_shell() -> Div {
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(GAP))
        .px(px(PAD_X))
        .overflow_hidden()
}

/// The art column's box: `h-8 w-14 overflow-hidden`, which is what makes the
/// row 32px tall on the web side too.
fn art_cell() -> Div {
    div()
        .flex_shrink_0()
        .w(px(ART_WIDTH))
        .h(px(ROW_HEIGHT))
        .overflow_hidden()
}

/// One row's art: the image cropped to fill, or the same "no art" plate the
/// web side draws when a track has none.
fn art(track: &TrackBrowserRow) -> Div {
    let cell = art_cell();
    match &track.album_art_path {
        // Explicit pixel bounds, not `size_full`: an `Img` falls back to the
        // decoded image's intrinsic size when its own box isn't definite, and
        // an oversized cell doesn't just overflow — it breaks `uniform_list`,
        // which scrolls on the assumption that every row is the same height.
        Some(path) => cell.child(
            img(PathBuf::from(path))
                .w(px(ART_WIDTH))
                .h(px(ROW_HEIGHT))
                .object_fit(ObjectFit::Cover),
        ),
        None => cell
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000033))
            .text_size(px(7.))
            .text_color(ladder::muted_foreground())
            .child("NO ART"),
    }
}

fn flex_cell() -> Div {
    div().flex_1().min_w(px(0.)).overflow_hidden()
}

fn numeric_cell(width: f32) -> Div {
    div()
        .flex_shrink_0()
        .w(px(width))
        .flex()
        .justify_end()
        .overflow_hidden()
}
