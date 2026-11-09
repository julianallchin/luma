//! Row and state bodies shared by every list: a track row, its check box,
//! and the empty, loading and retry placeholders.

use gpui::prelude::*;
use gpui::*;
use gpui_component::Icon;
use luma_lib::models::tracks::TrackBrowserRow;
use luma_ui::dialog::morph::ContentMode;
use luma_ui::float::{self, RowState};
use luma_ui::icons::IconName;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::{glass, ladder};

use super::ART_SIZE;
use crate::{Luma, SourceTrack};

/// Row height of the track lists. `uniform_list` virtualizes on a fixed row
/// height, so this is a layout fact rather than a style choice.
const ROW_HEIGHT: f32 = 44.0;

/// One track row. `selectable` adds the leading check glyph that makes a
/// multi-select list say so before anything is selected — without it, a
/// selected row and a merely-highlighted row differ only by a ring the user
/// has no reason to read as "chosen".
pub(super) fn track_row(
    state: RowState,
    fade_key: &str,
    art: Option<&str>,
    title: &str,
    subtitle: Option<&str>,
    selectable: bool,
) -> Div {
    // The same square whether or not there is art to put in it, so a list
    // where only some tracks have covers keeps one left edge. `tracks::album_art`
    // is the app's one recipe for this — art is a path on disk, read through
    // gpui's image cache, never inlined bytes (CLAUDE.md).
    row_shell(state, fade_key, selectable)
        .child(crate::tracks::album_art(art, ART_SIZE))
        .child(float::row_text(
            title.to_string(),
            subtitle.map(|subtitle| subtitle.to_string().into()),
        ))
}

/// A track row up to its cover: the row, and the check box when the list
/// is multi-select. The caller adds the cover and the words.
pub(super) fn row_shell(state: RowState, fade_key: &str, selectable: bool) -> Div {
    let row = float::menu_row(state, fade_key.to_string())
        .h(px(ROW_HEIGHT))
        .w_full();
    if !selectable {
        return row;
    }
    let checked = state == RowState::Selected;
    row.child(
        div()
            .size(px(15.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(luma_ui::radius::CHIP))
            .border_1()
            .border_color(glass::hairline(if checked { 0.0 } else { 0.18 }))
            .when(checked, |box_| box_.bg(glass::ink(0.92)))
            .when(checked, |box_| {
                box_.child(
                    Icon::new(IconName::Check)
                        .size(px(11.0))
                        .text_color(ladder::background()),
                )
            }),
    )
}

/// The line a list shows in place of rows it does not have.
///
/// Named, like [`loading_body`], because "empty" is a state a driver has to be
/// able to tell apart from "still loading" — and one line of grey prose is
/// otherwise indistinguishable from another.
pub(super) fn empty_body(message: &'static str) -> AnyElement {
    float::viewport()
        .child(float::list().child(float::empty_row(message).agent_node(Role::Text, message)))
        .into_any_element()
}

/// Placeholder rows while a list loads.
///
/// The skeleton is a paint, but the *state* still has to be nameable: a
/// driver (and a screen reader) has no way to tell "loading" from "empty"
/// by looking at grey bars, so the label the rows stand in for rides along.
pub(super) fn loading_body(
    id: &'static str,
    label: &'static str,
    view: gpui::EntityId,
    cx: &mut gpui::App,
) -> AnyElement {
    float::viewport()
        .child(
            float::list().child(
                float::skeleton_rows(6, view, cx)
                    .id(id)
                    .agent_node(Role::Text, label),
            ),
        )
        .into_any_element()
}

pub(super) fn retry_body(message: String, mode: ContentMode, app: &Entity<Luma>) -> AnyElement {
    let retry = app.clone();
    float::viewport()
        .child(
            float::list().child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .px(px(14.0))
                    .py(px(10.0))
                    .child(luma_ui::notice::line(message))
                    .child(
                        float::btn("Retry", "add-tracks-retry")
                            .id("add-tracks-retry")
                            .when(mode == ContentMode::Interactive, |button| {
                                button.on_click(move |_, _, cx| {
                                    retry.update(cx, |this, cx| this.refresh_open_track_browser(cx))
                                })
                            })
                            .agent_node(Role::Button, "Retry")
                            .agent_disabled(mode != ContentMode::Interactive),
                    ),
            ),
        )
        .into_any_element()
}

pub(super) fn track_title(track: &TrackBrowserRow) -> String {
    track
        .title
        .clone()
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| {
            std::path::Path::new(&track.file_path)
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("Untitled")
                .to_string()
        })
}

pub(super) fn source_title(track: &SourceTrack) -> String {
    track
        .title
        .clone()
        .or_else(|| track.filename.clone())
        .unwrap_or_else(|| "Untitled".to_string())
}
