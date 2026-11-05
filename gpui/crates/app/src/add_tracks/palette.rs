//! The palette frame every route wears: the header band with its filter and
//! submit chip, the import-source menu, and the footer legend.

use gpui::prelude::*;
use gpui::*;
use gpui_component::Icon;
use luma_lib::models::tracks::TrackImportPhase;
use luma_ui::dialog::morph::ContentMode;
use luma_ui::float::{self, RowState};
use luma_ui::icons::IconName;
use luma_ui::ladder;
use luma_ui::node::{AgentNode, Instrument, Role};

use super::import::TrackImportActivity;
use super::{preview, AddTracks, ImportChoice, Purpose, Route};
use crate::Luma;

/// Header band, body, footer legend — the shape every route wears.
///
/// The body is handed no height: it takes whatever the card has left, which is
/// what keeps a loading, empty or filtered list from resizing the dialog.
#[allow(clippy::too_many_arguments)]
pub(super) fn palette(
    state: &AddTracks,
    activity: Option<&TrackImportActivity>,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
    body: AnyElement,
) -> AnyElement {
    let interactive = mode == ContentMode::Interactive;
    let keys = app.clone();
    let mut frame = div().size_full().flex().flex_col().overflow_hidden();
    if interactive {
        // No `track_focus`: the dialog host already owns this card's focus
        // trap and tab group, and a second focus container inside it would add
        // a stop carrying no control. `on_key_down` fires for anything that
        // bubbles through, which is every key the focused field leaves unbound.
        frame = frame.on_key_down(move |event, _, cx| {
            let event = event.clone();
            keys.update(cx, |this, cx| this.add_tracks_key(&event, cx));
        });
        // Bound above the search field (`keymap::init_field_overrides`), so
        // each one hands the key back to the field when it is not about a
        // preview.
        let space = app.clone();
        let back = app.clone();
        let forward = app.clone();
        frame = frame
            .on_action(move |_: &crate::keymap::ToggleSourcePreview, _, cx| {
                if !space.update(cx, |this, cx| this.add_tracks_space(cx)) {
                    cx.propagate();
                }
            })
            .on_action(move |_: &crate::keymap::SeekSourcePreviewBack, _, cx| {
                if !back.update(cx, |this, cx| {
                    this.nudge_source_preview(-preview::SEEK_STEP, cx)
                }) {
                    cx.propagate();
                }
            })
            .on_action(move |_: &crate::keymap::SeekSourcePreviewForward, _, cx| {
                if !forward.update(cx, |this, cx| {
                    this.nudge_source_preview(preview::SEEK_STEP, cx)
                }) {
                    cx.propagate();
                }
            });
    }
    frame
        .child(header(state, activity, route, mode, app, window))
        .child(div().flex_1().min_h_0().flex().child(body))
        .child(footer(activity, route))
        .into_any_element()
}

fn header(
    state: &AddTracks,
    activity: Option<&TrackImportActivity>,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> Div {
    let interactive = mode == ContentMode::Interactive;
    let mut band = float::header_band();

    // Back sits where ← would take you, at the leading edge.
    if let Some(parent) = route.parent() {
        let back = app.clone();
        let cap = if interactive {
            float::key_cap_pressable(float::key_cap())
        } else {
            float::key_cap()
        };
        band = band.child(
            cap.id("add-tracks-back")
                .when(interactive, |cap| {
                    cap.track_focus(&state.back_focus)
                        .tab_index(0)
                        .on_click(move |_, _, cx| {
                            back.update(cx, |this, cx| this.add_tracks_route(parent, cx))
                        })
                })
                .child(Icon::new(IconName::ArrowLeft).size(px(12.5)))
                .agent_node(Role::Button, "Back")
                .agent_disabled(!interactive)
                .agent_focused(interactive && state.back_focus.is_focused(window)),
        );
    }

    band = band.child(filter_field(state, activity, route, mode, window));

    if let Some(chip) = submit_chip(state, route, mode, app, window) {
        band = band.child(chip);
    }

    let close = app.clone();
    let cap = if interactive {
        float::key_cap_pressable(float::key_cap())
    } else {
        float::key_cap()
    };
    band.child(
        cap.id("add-tracks-close")
            .when(interactive, |cap| {
                cap.track_focus(&state.close_focus)
                    .tab_index(0)
                    .on_click(move |_, _, cx| close.update(cx, |this, cx| this.dismiss_overlay(cx)))
            })
            .child("esc")
            .agent_node(Role::Button, "Close")
            .agent_disabled(!interactive)
            .agent_focused(interactive && state.close_focus.is_focused(window)),
    )
}

/// The filter field, or the route's title where there is nothing to filter.
///
/// A paint-only morph copy renders the typed text as plain glyphs instead of
/// the field: a live [`TextInput`](luma_ui::text_input::TextInput) would register a focus handle, and the
/// morph contract says an in-flight copy owns none.
fn filter_field(
    state: &AddTracks,
    activity: Option<&TrackImportActivity>,
    route: Route,
    mode: ContentMode,
    window: &Window,
) -> AnyElement {
    let slot = div().flex_1().min_w_0().text_size(px(14.0));
    if route == Route::Progress {
        let title = activity.map_or_else(|| "No import running".to_string(), |a| a.summary());
        return slot
            .truncate()
            .font_weight(FontWeight::MEDIUM)
            .child(title.clone())
            .agent_node(Role::Text, title)
            .into_any_element();
    }
    let (field, placeholder, query) = match route {
        Route::TrackBrowser => (
            Some(&state.browser_filter),
            "Search all tracks…",
            state.browser_query.as_str(),
        ),
        Route::SourceLibrary => (
            Some(&state.source_filter),
            "Search source…",
            state.source_query(),
        ),
        Route::Progress => unreachable!("the progress route has a title, not a field"),
    };
    let Some(field) = field else {
        return slot
            .font_weight(FontWeight::MEDIUM)
            .child(placeholder.to_string())
            .agent_node(Role::Text, placeholder)
            .into_any_element();
    };
    if mode != ContentMode::Interactive {
        return slot
            .text_color(if query.is_empty() {
                ladder::muted_foreground().into()
            } else {
                ladder::foreground_alpha(1.0)
            })
            .child(if query.is_empty() {
                placeholder.to_string()
            } else {
                query.to_string()
            })
            .agent_node(Role::Input, placeholder)
            .agent_disabled(true)
            .into_any_element();
    }
    let focused = match route {
        Route::TrackBrowser => state.browser_filter_focus.is_focused(window),
        Route::SourceLibrary => state.source_filter_focus.is_focused(window),
        Route::Progress => false,
    };
    slot.child(field.clone())
        .agent_node(Role::Input, placeholder)
        .agent_focused(focused)
        .into_any_element()
}

/// The one committing action, as the lit key in a row of key caps.
fn submit_chip(
    state: &AddTracks,
    route: Route,
    mode: ContentMode,
    app: &Entity<Luma>,
    window: &Window,
) -> Option<AnyElement> {
    // Choosing a song to play imports nothing.
    if matches!(state.purpose, Purpose::PlayVirtual(_)) {
        return None;
    }
    let interactive = mode == ContentMode::Interactive;
    let (label, enabled, id, node) = match route {
        Route::TrackBrowser => (
            "Import tracks".to_string(),
            true,
            "add-tracks-import",
            "Import tracks",
        ),
        Route::SourceLibrary => (
            if state.selected.is_empty() {
                "Import".to_string()
            } else {
                format!("Import {}", state.selected.len())
            },
            !state.selected.is_empty(),
            "add-tracks-import-selected",
            "Import selected",
        ),
        Route::Progress => ("Done".to_string(), true, "add-tracks-done", "Done"),
    };
    let submit = app.clone();
    let menu = (route == Route::TrackBrowser)
        .then(|| source_menu(state, app))
        .flatten();
    Some(
        float::btn_primary_chip()
            .relative()
            .id(id)
            .when(enabled && interactive, |chip| {
                chip.track_focus(&state.submit_focus).tab_index(0)
            })
            .when(!enabled || !interactive, |chip| {
                chip.opacity(float::INERT_OPACITY)
            })
            .when(enabled && interactive, |chip| {
                chip.on_click(move |_, _, cx| {
                    submit.update(cx, |this, cx| this.add_tracks_submit(route, cx))
                })
            })
            // The ⌘ glyph rather than an icon: this chip's whole job is to say
            // "the chord in the legend below does this".
            .child("⌘")
            .child(label)
            .children(menu)
            .agent_node(Role::Button, node)
            .agent_disabled(!enabled || !interactive)
            .agent_focused(enabled && interactive && state.submit_focus.is_focused(window))
            .into_any_element(),
    )
}

/// The height of [`float::btn_primary_chip`], which the source menu hangs off
/// — `anchored_below` needs it to know how far a menu that opens *upward* has
/// to clear the chip. The chip is cap-sized by construction, so the number
/// comes from the cap rather than being written down a second time.
const SUBMIT_CHIP_HEIGHT: f32 = float::KEY_CAP_HEIGHT;

/// The import-source menu, hanging off the header chip.
///
/// An actions menu, not a value picker: each row goes somewhere. It lives as a
/// child of the trigger so `anchored_below` can find the edge to hang from.
fn source_menu(state: &AddTracks, app: &Entity<Luma>) -> Option<AnyElement> {
    if !state.source_menu.is_shown() {
        return None;
    }
    let dismiss = app.clone();
    let mut card = float::popover_card().w(px(248.0));
    for choice in ImportChoice::ALL {
        let pick = app.clone();
        card = card.child(
            float::menu_row(RowState::Rest, choice.id())
                .id(choice.id())
                .on_click(move |_, _, cx| {
                    pick.update(cx, |this, cx| this.choose_import_source(choice, cx))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(div().child(choice.label()))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(11.0))
                                .text_color(ladder::muted_foreground())
                                .child(choice.description()),
                        ),
                )
                .agent_node(Role::Row, choice.label()),
        );
    }
    Some(match state.source_menu.exit() {
        Some(progress) => float::anchored_below_closing(
            "add-tracks-source-menu",
            SUBMIT_CHIP_HEIGHT,
            card.into_any_element(),
            progress,
        ),
        None => float::anchored_below(
            "add-tracks-source-menu",
            SUBMIT_CHIP_HEIGHT,
            float::Dismiss::on_press_out(move |_, cx| {
                dismiss.update(cx, |this, cx| this.toggle_source_menu(cx));
            }),
            card.into_any_element(),
        ),
    })
}

/// The key legend, plus whatever the route has to say about an import in
/// flight. Import status lives *here* rather than in a band of its own: a
/// strip that appears and disappears mid-flow shifts everything under it, and
/// the footer is already reserved space on every route.
fn footer(activity: Option<&TrackImportActivity>, route: Route) -> Div {
    if route == Route::Progress {
        // The page is the progress, so the footer does not repeat it.
        return float::footer_band()
            .child(float::key_hint(IconName::ArrowLeft, "Add more"))
            .child(float::key_hint_text("⌘↵", "Done"));
    }
    let mut band = float::nav_legend(match route {
        Route::SourceLibrary => "Select",
        Route::TrackBrowser | Route::Progress => "Open",
    });
    if route == Route::SourceLibrary {
        band = band.child(float::key_hint_text("space", "Preview"));
    }
    band = band.child(float::key_hint_text("⌘↵", "Import"));
    band = band.child(div().flex_1().min_w_0());
    // A route's own failure is shown where its rows would have been, by
    // `retry_body` — the footer would be a second copy of the same sentence.
    // What lands here is import progress, which belongs to no list.
    if let Some(activity) = activity {
        band = band.child(import_status(activity));
    }
    band
}

/// Import progress as one quiet line in the footer legend.
fn import_status(activity: &TrackImportActivity) -> Div {
    let phase = activity
        .progress
        .as_ref()
        .map(|progress| match progress.phase {
            TrackImportPhase::Importing => "importing",
            TrackImportPhase::Analyzing => "analyzing",
            TrackImportPhase::Complete => "complete",
        })
        .unwrap_or("importing");
    let phase_label = format!("Track import phase: {phase}");
    let mut line = div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .text_size(px(10.5))
        .text_color(ladder::muted_foreground())
        .child(
            div()
                .child(phase_label.clone())
                .agent_node(Role::Chip, phase_label),
        );
    if let Some(progress) = &activity.progress {
        let count = format!(
            "Track import progress: {}/{}",
            progress.done, progress.total
        );
        line = line.child(div().child(count.clone()).agent_node(Role::Text, count));
    }
    if let Some(error) = &activity.error {
        let label = format!("Track import error: {error}");
        line = line.child(
            div()
                .max_w(px(200.0))
                .truncate()
                .text_color(ladder::danger())
                .child(label.clone())
                .agent_node(Role::Text, label),
        );
    }
    line
}
