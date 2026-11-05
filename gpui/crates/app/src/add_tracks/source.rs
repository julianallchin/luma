//! The DJ-source route: reading a source's catalog, playlists and search,
//! selecting rows to import, and the rows with their preview controls.

use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::*;
use gpui_component::IconNamed;
use luma_ui::dialog::morph::ContentMode;
use luma_ui::float::{self, RowState};
use luma_ui::icons::IconName;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::{glass, ladder};

use super::rows::{empty_body, loading_body, retry_body, row_shell, source_title};
use super::{AddTracks, Ambient, Route, SourcePreview, SourceRowScope, ART_SIZE};
use crate::{Luma, TrackImportRequest, TrackSource};

impl Luma {
    pub(super) fn choose_source(&mut self, engine: bool, cx: &mut Context<Self>) {
        let source = if engine {
            None
        } else {
            Some(TrackSource::Rekordbox)
        };
        if let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() {
            state.source_generation = state
                .source_generation
                .checked_add(1)
                .expect("source generation exhausted");
            state.source = source.clone();
            state.source_library = None;
            state.playlists = Rc::from(Vec::new());
            state.source_rows = Rc::from(Vec::new());
            state.begin_source_row_read(SourceRowScope::All);
            state.request(Route::SourceLibrary, cx);
        }
        let (instance_id, source_generation, row_generation) = match self.overlay.as_open() {
            Some(crate::shell::Overlay::AddTracks(state)) => (
                state.instance_id,
                state.source_generation,
                state.source_row_generation,
            ),
            _ => return,
        };
        let engine_source = engine.then(|| self.library.default_engine_dj_source());
        cx.spawn(async move |this, cx| {
            let source = match (source, engine_source) {
                (Some(source), None) => Ok(source),
                (None, Some(source)) => source.await,
                _ => unreachable!("one source choice"),
            };
            let source = match source {
                Ok(source) => source,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        if let Some(crate::shell::Overlay::AddTracks(state)) =
                            this.overlay.open_mut()
                        {
                            if state.instance_id != instance_id
                                || state.source_generation != source_generation
                            {
                                return;
                            }
                            state.source_loaded = true;
                            state.source_error = Some(error.to_string());
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
            };
            let Ok(library) =
                this.read_with(cx, |this, _| this.library.source_library(source.clone()))
            else {
                return;
            };
            let library = library.await;
            let Ok(playlists) =
                this.read_with(cx, |this, _| this.library.source_playlists(source.clone()))
            else {
                return;
            };
            let playlists = playlists.await;
            let Ok(tracks) =
                this.read_with(cx, |this, _| this.library.source_tracks(source.clone()))
            else {
                return;
            };
            let tracks = tracks.await;
            this.update(cx, |this, cx| {
                let Some(crate::shell::Overlay::AddTracks(state)) = this.overlay.open_mut() else {
                    return;
                };
                if state.instance_id != instance_id || state.source_generation != source_generation
                {
                    return;
                }
                state.source = Some(source);
                match library {
                    Ok(library) => state.source_library = Some(library),
                    Err(error) => state.source_error = Some(error.to_string()),
                }
                match playlists {
                    Ok(playlists) => state.playlists = playlists.into(),
                    Err(error) => state.source_error = Some(error.to_string()),
                }
                if state.source_row_generation == row_generation {
                    state.source_loaded = true;
                    match tracks {
                        Ok(tracks) => state.source_rows = tracks.into(),
                        Err(error) => state.source_error = Some(error.to_string()),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn select_source_playlist(
        &mut self,
        playlist_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() else {
            return;
        };
        let Some(source) = state.source.clone() else {
            return;
        };
        let instance_id = state.instance_id;
        let source_generation = state.source_generation;
        let row_generation = state.begin_source_row_read(match &playlist_id {
            Some(id) => SourceRowScope::Playlist(id.clone()),
            None => SourceRowScope::All,
        });
        // The scope that was just chosen replaces the filter, so the field has
        // to stop showing a query that is no longer being applied. The echo
        // this provokes is absorbed by `source_filter_changed`'s guard.
        let filter = state.source_filter.clone();
        state.source_active = 0;
        filter.update(cx, |field, cx| field.set_text("", cx));
        let all = playlist_id
            .is_none()
            .then(|| self.library.source_tracks(source.clone()));
        let playlist = playlist_id
            .as_deref()
            .map(|id| self.library.source_playlist_tracks(source, id));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = match (all, playlist) {
                (Some(all), None) => all.await,
                (None, Some(playlist)) => playlist.await,
                _ => unreachable!("one source track read"),
            };
            this.update(cx, |this, cx| {
                let Some(crate::shell::Overlay::AddTracks(state)) = this.overlay.open_mut() else {
                    return;
                };
                if state.instance_id != instance_id
                    || state.source_generation != source_generation
                    || state.source_row_generation != row_generation
                {
                    return;
                }
                state.source_loaded = true;
                match result {
                    Ok(rows) => state.source_rows = rows.into(),
                    Err(error) => state.source_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The source library filters server-side, so an edit starts a fresh read
    /// rather than narrowing what is already in hand.
    pub(super) fn source_filter_changed(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() else {
            return;
        };
        // An edit that lands on the query already in force is not a new read.
        // This is what keeps `select_source_playlist`'s field-clear from
        // starting a second, racing read — and it is also the right rule on
        // its own: emptying the filter while a playlist is chosen should keep
        // that playlist, not silently widen to every track in the source.
        if query == state.source_query() {
            return;
        }
        let Some(source) = state.source.clone() else {
            return;
        };
        let instance_id = state.instance_id;
        let source_generation = state.source_generation;
        state.source_active = 0;
        state.source_navigating = false;
        state.source_scroll.scroll_to_item(0, ScrollStrategy::Top);
        let row_generation = state.begin_source_row_read(if query.is_empty() {
            SourceRowScope::All
        } else {
            SourceRowScope::Search(query.clone())
        });
        let all = query
            .is_empty()
            .then(|| self.library.source_tracks(source.clone()));
        let search = (!query.is_empty()).then(|| self.library.search_source_tracks(source, &query));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = match (all, search) {
                (Some(all), None) => all.await,
                (None, Some(search)) => search.await,
                _ => unreachable!("one source search read"),
            };
            this.update(cx, |this, cx| {
                let Some(crate::shell::Overlay::AddTracks(state)) = this.overlay.open_mut() else {
                    return;
                };
                if state.instance_id != instance_id
                    || state.source_generation != source_generation
                    || state.source_row_generation != row_generation
                {
                    return;
                }
                state.source_loaded = true;
                match result {
                    Ok(rows) => state.source_rows = rows.into(),
                    Err(error) => state.source_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn toggle_source_track(&mut self, track_id: &str, cx: &mut Context<Self>) {
        if let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() {
            if !state.selected.remove(track_id) {
                state.selected.insert(track_id.to_string());
            }
            cx.notify();
        }
    }

    pub(super) fn import_source_selection(&mut self, cx: &mut Context<Self>) {
        let Some(crate::shell::Overlay::AddTracks(state)) = self.overlay.open_mut() else {
            return;
        };
        let Some(source) = state.source.clone() else {
            return;
        };
        if state.selected.is_empty()
            || self
                .track_import
                .as_ref()
                .is_some_and(|activity| activity.active)
        {
            return;
        }
        let selected: Vec<String> = state.selected.iter().cloned().collect();
        state.source_error = None;
        self.begin_track_import(
            TrackImportRequest::Source {
                source,
                track_ids: selected,
            },
            cx,
        );
    }
}

pub(super) fn source_library(
    state: &AddTracks,
    ambient: &Ambient<'_>,
    mode: ContentMode,
    app: &Entity<Luma>,
    cx: &mut gpui::App,
) -> AnyElement {
    let list = if let Some(error) = &state.source_error {
        retry_body(format!("Source error: {error}"), mode, app)
    } else if !state.source_loaded {
        loading_body(
            "add-tracks-source-skeleton",
            "Loading source library…",
            app.entity_id(),
            cx,
        )
    } else if state.source_rows.is_empty() {
        empty_body("No source tracks")
    } else {
        source_rows(state, ambient, mode, app)
    };
    div()
        .size_full()
        .flex()
        .flex_row()
        .child(div().flex_1().min_w_0().flex().flex_col().child(list))
        .child(source_rail(state, mode, app))
        .into_any_element()
}

fn source_rows(
    state: &AddTracks,
    ambient: &Ambient<'_>,
    mode: ContentMode,
    app: &Entity<Luma>,
) -> AnyElement {
    let rows = Rc::clone(&state.source_rows);
    let selected = Rc::new(state.selected.clone());
    let active = state.source_active;
    let scroll = state.source_scroll.clone();
    let app = app.clone();
    let art = ambient.art.clone();
    let interactive = mode == ContentMode::Interactive;
    // A press on a cover or a scrub bar is a pointer gesture inside a
    // keyboard-first list: it hands focus back to the search field, or the
    // arrows and space would stop reaching the list the moment you clicked.
    let keyboard = state.source_filter_focus.clone();
    let preview = ambient
        .preview
        .map(|preview| (preview.track_id.clone(), RowPreview::of(preview)));
    float::viewport()
        .child(
            uniform_list("source-tracks", rows.len(), move |range, _, cx| {
                let mut asked = false;
                let items = range
                    .map(|index| {
                        let track = &rows[index];
                        let id = track.id.clone();
                        let checked = selected.contains(&id);
                        let title = source_title(track);
                        let key = format!("add-tracks-source-track-{id}");
                        // Asked for here, at layout, so only rows the list
                        // actually lays out — the visible ones — are read.
                        let cover = art.lookup(track);
                        asked |= cover.asked;
                        let previewing = preview
                            .as_ref()
                            .filter(|(track_id, _)| *track_id == id)
                            .map(|(_, preview)| preview);
                        let subtitle = match previewing {
                            Some(preview) => {
                                Some(scrub_line(preview, &app, &keyboard, interactive))
                            }
                            None => track
                                .artist
                                .clone()
                                .map(|artist| float::row_subtitle(artist).into_any_element()),
                        };
                        let toggle = app.clone();
                        row_shell(RowState::of(checked, index == active), &key, true)
                            .child(preview_art(
                                cover.image,
                                previewing,
                                &key,
                                &id,
                                &title,
                                (&app, &keyboard),
                                interactive,
                            ))
                            .child(float::row_text_with(title.clone(), subtitle))
                            .id(SharedString::from(key.clone()))
                            .when(interactive, |row| {
                                row.on_click(move |_, _, cx| {
                                    toggle.update(cx, |this, cx| this.toggle_source_track(&id, cx));
                                })
                            })
                            .agent_node(Role::Row, title)
                            .agent_disabled(!interactive)
                    })
                    .collect();
                if asked {
                    // Deferred: this runs mid-layout, and serving a cover
                    // starts work on the app — which is not to be re-entered
                    // from inside its own frame.
                    let app = app.clone();
                    cx.defer(move |cx| app.update(cx, |this, cx| this.pump_source_art(cx)));
                }
                items
            })
            .track_scroll(&scroll)
            .size_full()
            .px(px(8.0)),
        )
        .into_any_element()
}

/// The preview as one row draws it — copied out of the shell's state so the
/// list's render closure, which outlives this frame's borrows, can hold it.
#[derive(Clone)]
struct RowPreview {
    playing: bool,
    /// Decoding: there is no duration or overview yet.
    loading: bool,
    failed: Option<String>,
    position: f32,
    duration: f32,
    fraction: f32,
    scrubbing: bool,
    overview: Rc<[f32]>,
}

impl RowPreview {
    fn of(preview: &SourcePreview) -> Self {
        let (duration, overview) = preview.loaded.as_ref().map_or_else(
            || (0.0, Rc::from(Vec::new())),
            |loaded| (loaded.duration, Rc::clone(&loaded.overview)),
        );
        Self {
            playing: preview.playing,
            loading: preview.loaded.is_none() && preview.failed.is_none(),
            failed: preview.failed.clone(),
            position: preview.position,
            duration,
            fraction: preview.fraction(),
            scrubbing: preview.scrubbing,
            overview,
        }
    }
}

/// The scrim a cover dims to under the play glyph.
const ART_SCRIM: f32 = 0.45;
/// The playing row's scrim: lighter, because it stays up while the row
/// plays and should read as a state of the cover, not a cover over it.
const ART_SCRIM_ACTIVE: f32 = 0.32;

/// The row's cover, which is also its play button: on hover it dims under a
/// play glyph, and while the row previews it holds a pause glyph.
#[allow(clippy::too_many_arguments)]
fn preview_art(
    cover: Option<Arc<RenderImage>>,
    preview: Option<&RowPreview>,
    key: &str,
    track_id: &str,
    title: &str,
    (app, keyboard): (&Entity<Luma>, &FocusHandle),
    interactive: bool,
) -> AnyElement {
    let group = SharedString::from(format!("{key}-art"));
    let keyboard = keyboard.clone();
    let playing = preview.is_some_and(|preview| preview.playing);
    let held = preview.is_some_and(|preview| preview.failed.is_none());
    let loading = preview.is_some_and(|preview| preview.loading);
    let label = if playing {
        format!("Pause preview of {title}")
    } else {
        format!("Play preview of {title}")
    };
    let toggle = app.clone();
    let track_id = track_id.to_string();
    let has_cover = cover.is_some();
    let glyph = if playing {
        IconName::Pause
    } else {
        IconName::Play
    };
    let scrim = if held { ART_SCRIM_ACTIVE } else { ART_SCRIM };
    let glyph_alpha = if loading { 0.55 } else { 0.95 };
    let reveal = held || interactive;
    // Cover and overlay are painted, each in a layer of its own. Inside the
    // dialog's layer every primitive shares one draw order, and gpui then
    // draws images after quads and glyphs — so an overlay laid out as a
    // sibling of the image would sit *under* it. A layer per pass is what
    // puts the scrim and the glyph on top.
    let paint = canvas(
        |_, _, _| {},
        move |bounds, _, window, cx| {
            let corners = Corners::all(px(luma_ui::radius::CHIP));
            if let Some(cover) = &cover {
                window.paint_layer(bounds, |window| {
                    window
                        .paint_image(bounds, bounds, corners, Arc::clone(cover), 0, false)
                        .ok();
                });
            }
            if !(held || reveal && bounds.contains(&window.mouse_position())) {
                return;
            }
            window.paint_layer(bounds, |window| {
                window.paint_quad(fill(bounds, glass::scrim(scrim)).corner_radii(corners));
                let edge = px(ART_GLYPH);
                let glyph_bounds = Bounds::new(
                    point(
                        bounds.center().x - edge / 2.0,
                        bounds.center().y - edge / 2.0,
                    ),
                    size(edge, edge),
                );
                window
                    .paint_svg(
                        glyph_bounds,
                        glyph.path(),
                        None,
                        TransformationMatrix::unit(),
                        glass::ink(glyph_alpha),
                        cx,
                    )
                    .ok();
            });
        },
    )
    .size_full();
    div()
        .id(group.clone())
        .relative()
        .flex_none()
        .size(px(ART_SIZE))
        .rounded(px(luma_ui::radius::CHIP))
        .bg(glass::wash(glass::WASH_SUBTLE))
        // No visual change of its own: a hover style is what makes gpui
        // repaint this cover as the pointer enters and leaves, which is when
        // the painted overlay above has to appear and go.
        .hover(|style| style)
        .when(held, |art| {
            art.border_1().border_color(glass::hairline(0.35))
        })
        .child(if has_cover {
            paint
                .agent_node(Role::Text, format!("Cover of {title}"))
                .into_any_element()
        } else {
            paint.into_any_element()
        })
        .when(interactive, |art| {
            art.cursor_pointer().on_click(move |_, window, cx| {
                // The cover plays; the row around it selects. One press is
                // one of the two.
                cx.stop_propagation();
                window.focus(&keyboard, cx);
                let track_id = track_id.clone();
                toggle.update(cx, |this, cx| this.toggle_source_preview(&track_id, cx));
            })
        })
        .agent_node(Role::Button, label)
        .agent_disabled(!interactive)
        .into_any_element()
}

/// The play / pause glyph's edge on a cover.
const ART_GLYPH: f32 = 13.0;

/// The previewing row's second line: elapsed time, the scrub bar, remaining
/// time — or why it is not playing.
fn scrub_line(
    preview: &RowPreview,
    app: &Entity<Luma>,
    keyboard: &FocusHandle,
    interactive: bool,
) -> AnyElement {
    let line = div()
        .h(px(SCRUB_HEIGHT))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .text_size(px(10.0))
        .text_color(ladder::muted_foreground());
    if let Some(error) = &preview.failed {
        let message = format!("Can't preview: {error}");
        return line
            .child(
                div()
                    .truncate()
                    .text_color(ladder::danger())
                    .child(message.clone()),
            )
            .agent_node(Role::Text, message)
            .into_any_element();
    }
    if preview.loading {
        return line
            .child("Loading preview…")
            .agent_node(Role::Text, "Loading preview")
            .into_any_element();
    }
    let elapsed = clock(preview.position);
    let remaining = format!("-{}", clock(preview.duration - preview.position));
    let label = format!("Preview position: {elapsed} of {}", clock(preview.duration));
    line.child(time_label(elapsed))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(scrub_bar(
                    preview.clone(),
                    app.clone(),
                    keyboard.clone(),
                    interactive,
                ))
                .agent_node(Role::Slider, label)
                .agent_disabled(!interactive),
        )
        .child(time_label(remaining))
        .into_any_element()
}

/// The scrub line's height: the subtitle's line, so a row does not change
/// height when it starts previewing.
const SCRUB_HEIGHT: f32 = 14.0;

fn time_label(text: String) -> Div {
    div()
        .flex_none()
        .min_w(px(28.0))
        .font_family(luma_ui::fonts::MONO)
        .child(text)
}

/// `m:ss`, rounding down — a playhead at 9.99 s has not reached 0:10.
fn clock(seconds: f32) -> String {
    let seconds = seconds.max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// The waveform overview the playhead walks across; a press or drag seeks.
///
/// A canvas because the drag outlives the element under the pointer: the
/// listeners are window-level and keyed to the bar's painted bounds, so a
/// drag that leaves the bar keeps scrubbing to its nearer end.
fn scrub_bar(
    preview: RowPreview,
    app: Entity<Luma>,
    keyboard: FocusHandle,
    interactive: bool,
) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let width = f32::from(bounds.size.width).max(1.0);
            let height = f32::from(bounds.size.height);
            let played = glass::ink(0.78);
            let ahead = glass::ink(0.22);
            let peaks = preview.overview.len();
            if peaks == 0 {
                // No overview: a plain track with the played part lit.
                let track = Bounds::new(
                    point(bounds.origin.x, bounds.center().y - px(1.0)),
                    size(bounds.size.width, px(2.0)),
                );
                window.paint_quad(fill(track, ahead));
                let mut done = track;
                done.size.width = px(width * preview.fraction);
                window.paint_quad(fill(done, played));
            } else {
                let step = width / peaks as f32;
                let bar = (step - 1.0).max(1.0);
                for (index, peak) in preview.overview.iter().enumerate() {
                    let x = index as f32 * step;
                    let h = (peak * height).clamp(1.0, height);
                    let quad = Bounds::new(
                        point(
                            bounds.origin.x + px(x),
                            bounds.origin.y + px((height - h) / 2.0),
                        ),
                        size(px(bar), px(h)),
                    );
                    let lit = (x + bar / 2.0) / width <= preview.fraction;
                    window.paint_quad(fill(quad, if lit { played } else { ahead }));
                }
            }
            let head = Bounds::new(
                point(
                    bounds.origin.x + px(width * preview.fraction - 0.5),
                    bounds.origin.y,
                ),
                size(px(1.0), bounds.size.height),
            );
            window.paint_quad(fill(head, ladder::playhead()));
            if !interactive {
                return;
            }
            let duration = preview.duration;
            let seek_at = move |x: Pixels| {
                ((f32::from(x - bounds.origin.x)) / width).clamp(0.0, 1.0) * duration
            };
            let down = app.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !bounds.contains(&event.position)
                {
                    return;
                }
                // The row selects on click; a press on its bar is a seek.
                cx.stop_propagation();
                window.focus(&keyboard, cx);
                let seconds = seek_at(event.position.x);
                down.update(cx, |this, cx| {
                    this.set_source_preview_scrubbing(true, cx);
                    this.seek_source_preview(seconds, cx);
                });
            });
            if !preview.scrubbing {
                return;
            }
            let drag = app.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble && event.pressed_button == Some(MouseButton::Left)
                {
                    let seconds = seek_at(event.position.x);
                    drag.update(cx, |this, cx| this.seek_source_preview(seconds, cx));
                }
            });
            let up = app.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                    up.update(cx, |this, cx| this.set_source_preview_scrubbing(false, cx));
                }
            });
        },
    )
    .size_full()
}

fn source_rail(state: &AddTracks, mode: ContentMode, app: &Entity<Luma>) -> impl IntoElement {
    let interactive = mode == ContentMode::Interactive;
    let all = app.clone();
    float::rail()
        .id("add-tracks-source-rail")
        .overflow_y_scroll()
        .child(float::section_heading("Playlists"))
        .child(
            float::nav_row(
                RowState::of(state.active_playlist().is_none(), false),
                "add-tracks-source-all",
            )
            .id("add-tracks-source-all")
            .child(div().flex_1().min_w_0().truncate().child("All tracks"))
            .when(interactive, |row| {
                row.on_click(move |_, _, cx| {
                    all.update(cx, |this, cx| this.select_source_playlist(None, cx))
                })
            })
            .agent_node(Role::Row, "All tracks")
            .agent_disabled(!interactive),
        )
        .children(state.playlists.iter().map(|playlist| {
            let app = app.clone();
            let id = playlist.id.clone();
            let label = playlist.name.clone();
            let active = state.active_playlist() == Some(id.as_str());
            let key = format!("add-tracks-playlist-{id}");
            float::nav_row(RowState::of(active, false), key.clone())
                .id(SharedString::from(key))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(label.clone())),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(ladder::muted_foreground())
                        .child(format!("{}", playlist.track_count)),
                )
                .when(interactive, |row| {
                    row.on_click(move |_, _, cx| {
                        let id = id.clone();
                        app.update(cx, |this, cx| this.select_source_playlist(Some(id), cx));
                    })
                })
                .agent_node(Role::Row, label)
                .agent_disabled(!interactive)
        }))
}
