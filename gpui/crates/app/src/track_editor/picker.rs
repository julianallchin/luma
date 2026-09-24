//! Searchable preset insertion with an isolated, looping venue preview.
//!
//! # Hovering is instant
//!
//! A person sweeps the pointer down the list to look, so a row they already
//! passed must not load again. Each row's frames and its first rendered image
//! are kept for the life of the dialog. Only one frame request runs at a time;
//! while nothing is wanted, the rows beside the highlighted one load ahead.
//! The previous image stays up while a new one loads, and "Loading preview…"
//! shows only when a load is slow enough to notice.
use std::collections::{HashMap, VecDeque};

use super::*;
use crate::{picker_preview, shell::Overlay};
use luma_lib::{
    models::universe::UniverseState,
    stage_render::{Continuity, Sequence},
};
use luma_ui::dialog::morph::{self, MorphSize};

const SIZE: MorphSize = MorphSize::new(880., 540.);
/// The list column, left of the preview. Its right edge is a 1px rule.
const LIST_W: f32 = 360.;
/// The preview's title above the frame, and its note below it.
const TITLE_H: f32 = 44.;
const NOTE_H: f32 = 34.;
/// The frame's box: everything right of the list between the title and the
/// note. The render is asked for this size, so it fills the box exactly.
const PREVIEW_W: f32 = SIZE.width - LIST_W - 1.;
const PREVIEW_H: f32 = SIZE.height - float::HEADER_HEIGHT - float::FOOTER_HEIGHT - TITLE_H - NOTE_H;
/// How many first-frame images the dialog keeps. Each is the preview box in
/// RGBA, under 1 MB at 1x and about 3.6 MB at 2x.
const IMAGE_CACHE: usize = 16;
/// A load shorter than this shows no "Loading preview…" text.
const LOADING_HINT: Duration = Duration::from_millis(250);
/// Rows either side of the highlighted one that load ahead, nearest first.
const AHEAD: [isize; 4] = [1, -1, 2, -2];

pub(crate) struct Picker {
    generation: uuid::Uuid,
    target: Target,
    sequence: Option<Arc<Sequence>>,
    /// The filtered rows, and the query they were built for.
    choices: Option<(String, Arc<Vec<InsertChoice>>)>,
    /// The row the preview is showing, and when it was highlighted.
    shown: Option<String>,
    shown_at: std::time::Instant,
    /// The row whose frames are being fetched. One at a time.
    fetching: Option<String>,
    fetched: HashMap<String, Arc<Vec<UniverseState>>>,
    failed: HashMap<String, String>,
    frames: Arc<Vec<UniverseState>>,
    since: std::time::Instant,
    drawn: Option<usize>,
    drawing: bool,
    /// Whose frame the sequence last rendered. Its haze and motion history
    /// carry over, so a frame of another row has to be a cut.
    rendered: Option<String>,
    image: Option<Arc<RenderImage>>,
    /// First frames by row, oldest first, capped at [`IMAGE_CACHE`].
    firsts: VecDeque<(String, Arc<RenderImage>)>,
    error: Option<String>,
    scene_error: Option<String>,
}

pub(super) fn open(app: &mut Luma, cx: &mut Context<Luma>) {
    let Some(Body::TrackEditor(editor)) = app.workspace.active_body_mut() else {
        return;
    };
    if editor.menu.is_none() {
        return;
    }
    editor.menu_query.clear();
    editor
        .menu_search
        .update(cx, |field, cx| field.set_text("", cx));
    let target = Target::TrackEditor {
        track: editor.track_id.clone(),
        venue: editor.venue_id.clone(),
    };
    let rig = app.library.venue_rig(&editor.venue_id);
    let settings = app
        .visualizer
        .as_ref()
        .map(crate::visualizer::Visualizer::render_settings);
    let pixels = picker_preview::pixels(PREVIEW_W, PREVIEW_H, app.scale_factor);
    let generation = uuid::Uuid::new_v4();
    let now = std::time::Instant::now();
    app.overlay.open(Overlay::InsertPattern(Box::new(Picker {
        generation,
        target,
        sequence: None,
        choices: None,
        shown: None,
        shown_at: now,
        fetching: None,
        fetched: HashMap::new(),
        failed: HashMap::new(),
        frames: Arc::default(),
        since: now,
        drawn: None,
        drawing: false,
        rendered: None,
        image: None,
        firsts: VecDeque::new(),
        error: None,
        scene_error: None,
    })));
    cx.spawn(async move |this, cx| {
        let result = match rig.await {
            Ok(rig) => {
                cx.background_executor()
                    .spawn(async move { picker_preview::install(&rig, settings, pixels) })
                    .await
            }
            Err(error) => Err(error.to_string()),
        };
        this.update(cx, |this, cx| {
            if let Some(Overlay::InsertPattern(state)) = this.overlay.open_mut() {
                if state.generation != generation {
                    return;
                }
                match result {
                    Ok(scene) => state.sequence = Some(scene),
                    Err(error) => state.scene_error = Some(error),
                }
                cx.notify();
            }
        })
        .ok();
    })
    .detach();
    cx.notify();
}

impl Picker {
    /// The rows as last built. Empty until the first tick builds them.
    fn rows(&self) -> &[InsertChoice] {
        self.choices
            .as_ref()
            .map_or(&[][..], |(_, rows)| rows.as_slice())
    }

    /// Point the preview at `id`, from the caches when they have it.
    fn show(&mut self, id: String) {
        self.frames = self.fetched.get(&id).cloned().unwrap_or_default();
        self.error = self.failed.get(&id).cloned();
        self.since = std::time::Instant::now();
        self.shown_at = self.since;
        self.drawn = None;
        if let Some((_, image)) = self.firsts.iter().find(|(first, _)| *first == id) {
            self.image = Some(Arc::clone(image));
            self.drawn = Some(0);
        }
        self.shown = Some(id);
    }

    fn remember_first(&mut self, id: String, image: Arc<RenderImage>) {
        self.firsts.retain(|(first, _)| *first != id);
        if self.firsts.len() >= IMAGE_CACHE {
            self.firsts.pop_front();
        }
        self.firsts.push_back((id, image));
    }

    /// Whether a load has run long enough to say so.
    fn slow(&self) -> bool {
        self.shown.is_some()
            && self.frames.is_empty()
            && self.error.is_none()
            && self.scene_error.is_none()
            && self.shown_at.elapsed() >= LOADING_HINT
    }
}

pub(crate) fn tick(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) {
    let Some(Overlay::InsertPattern(picker)) = app.overlay.as_open() else {
        return;
    };
    if app.workspace.active() != Some(&picker.target) {
        app.close_overlay(cx);
        return;
    }
    let Some(Body::TrackEditor(editor)) = app.workspace.active_body() else {
        return;
    };
    let focus = editor.menu_search.read(cx).focus_handle(cx);
    if !focus.is_focused(window) {
        window.focus(&focus, cx);
    }
    let key = editor.menu_query.clone();
    let rebuilt = (picker.choices.as_ref().map(|(built, _)| built) != Some(&key))
        .then(|| Arc::new(editor.insertion_choices()));
    let Some(menu) = editor.menu else {
        return;
    };
    let Some(Overlay::InsertPattern(state)) = app.overlay.open_mut() else {
        return;
    };
    if let Some(rows) = rebuilt {
        state.choices = Some((key, rows));
    }
    let rows = state
        .choices
        .as_ref()
        .map(|(_, rows)| Arc::clone(rows))
        .unwrap_or_default();
    let Some(current) = rows.get(menu.active) else {
        state.shown = None;
        state.frames = Arc::default();
        state.image = None;
        state.error = None;
        state.drawn = None;
        return;
    };
    let id = current.id();
    if state.shown.as_ref() != Some(&id) {
        state.show(id.clone());
    }
    if state.frames.is_empty() && state.error.is_none() {
        // Wake once the hint is due, so it appears without another event.
        window.request_animation_frame();
    }
    if state.fetching.is_none() {
        let wanted = std::iter::once(0)
            .chain(AHEAD)
            .filter_map(|offset| rows.get(menu.active.checked_add_signed(offset)?))
            .find(|choice| {
                let id = choice.id();
                !state.fetched.contains_key(&id) && !state.failed.contains_key(&id)
            })
            .copied();
        if let Some(choice) = wanted {
            fetch(app, menu, choice, cx);
        }
    }
    draw(app, window, cx);
}

/// Ask for one row's frames. The answer is kept whether or not the row is
/// still highlighted when it arrives.
fn fetch(app: &mut Luma, menu: InsertMenu, choice: InsertChoice, cx: &mut Context<Luma>) {
    let Some(Body::TrackEditor(editor)) = app.workspace.active_body() else {
        return;
    };
    let id = choice.id();
    let start = menu.start;
    // At most four seconds and sixty samples; the preview never drives DMX.
    let end = menu.end.min(start + 4.);
    let count = (((end - start) * 15.).ceil() as usize).clamp(1, 60);
    let task = app.library.preview_definition_frames(
        luma_lib::models::composable_patterns::ComposablePreviewRequest {
            venue_id: editor.venue_id.clone(),
            track_id: editor.track_id.clone(),
            definition: choice.0.form.clone(),
            inputs: choice.0.inputs.clone(),
            targets: vec![luma_lib::models::selection::Selection::new("all")],
            times: (0..count)
                .map(|i| start + i as f64 * (end - start) / count as f64)
                .collect(),
            clip_start: start,
            clip_end: menu.end,
            seed: 0,
        },
    );
    let pending = async move {
        task.await
            .map(|preview| preview.frames)
            .map_err(|error| error.to_string())
    };
    let Some(Overlay::InsertPattern(state)) = app.overlay.open_mut() else {
        return;
    };
    state.fetching = Some(id.clone());
    let generation = state.generation;
    cx.spawn(async move |this, cx| {
        let result = pending.await;
        this.update(cx, |this, cx| {
            if let Some(Overlay::InsertPattern(state)) = this.overlay.open_mut() {
                if state.generation != generation {
                    return;
                }
                state.fetching = None;
                let shown = state.shown.as_ref() == Some(&id);
                match result {
                    Ok(frames) => {
                        let frames = Arc::new(frames);
                        if shown {
                            state.frames = Arc::clone(&frames);
                            state.since = std::time::Instant::now();
                        }
                        state.fetched.insert(id, frames);
                    }
                    Err(error) => {
                        if shown {
                            state.error = Some(error.clone());
                        }
                        state.failed.insert(id, error);
                    }
                }
                cx.notify();
            }
        })
        .ok();
    })
    .detach();
}

/// Render the frame the loop is on, one at a time, off the main thread.
fn draw(app: &mut Luma, window: &mut Window, cx: &mut Context<Luma>) {
    let Some(Overlay::InsertPattern(state)) = app.overlay.open_mut() else {
        return;
    };
    if state.frames.is_empty() || state.drawing {
        return;
    }
    let Some(sequence) = state.sequence.clone() else {
        return;
    };
    let Some(id) = state.shown.clone() else {
        return;
    };
    let index = (state.since.elapsed().as_secs_f32() * 15.) as usize % state.frames.len();
    window.request_animation_frame();
    if state.drawn == Some(index) {
        return;
    }
    let cut = index == 0 || state.drawn.is_none() || state.rendered.as_ref() != Some(&id);
    let frame = state.frames[index].clone();
    let generation = state.generation;
    state.drawing = true;
    cx.spawn(async move |this, cx| {
        let result = cx
            .background_executor()
            .spawn(async move {
                let (width, height) = sequence.size();
                sequence
                    .frame(
                        Some(&frame),
                        index as f32 / 15.,
                        // The live budget, not the goldens': the preview shares
                        // the GPU with the window, and a 16-subframe cut froze
                        // it for about 300 ms on every new row.
                        if cut { luma_render::LIVE_SUBFRAMES } else { 1 },
                        if cut {
                            Continuity::Cut
                        } else {
                            Continuity::Next
                        },
                    )
                    .and_then(|rgba| picker_preview::image_from_rgba(rgba, width, height))
                    .map(Arc::new)
            })
            .await;
        this.update(cx, |this, cx| {
            if let Some(Overlay::InsertPattern(state)) = this.overlay.open_mut() {
                if state.generation != generation {
                    return;
                }
                state.drawing = false;
                state.rendered = Some(id.clone());
                match result {
                    Ok(image) => {
                        if index == 0 {
                            state.remember_first(id.clone(), Arc::clone(&image));
                        }
                        if state.shown.as_ref() == Some(&id) {
                            state.image = Some(image);
                            state.drawn = Some(index);
                        }
                    }
                    Err(error) => {
                        if state.shown.as_ref() == Some(&id) {
                            state.error = Some(error);
                        }
                    }
                }
                cx.notify();
            }
        })
        .ok();
    })
    .detach();
}

pub(crate) fn render(state: &Picker, editor: &Editor, app: &Entity<Luma>) -> AnyElement {
    let close = app.clone();
    let rows = state.rows();
    let active = editor.menu.map_or(0, |menu| menu.active);
    let content = div()
        .size_full()
        .flex()
        .flex_col()
        .text_color(ladder::foreground())
        .child(
            float::header_band()
                .child("Insert pattern")
                .child(div().flex_1())
                .child(
                    float::btn("Cancel", "close-pattern-picker")
                        .id("close-pattern-picker")
                        .on_click(move |_, _, cx| {
                            close.update(cx, |app, cx| app.close_overlay(cx))
                        }),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(
                    div()
                        .w(px(LIST_W))
                        .flex_none()
                        .flex()
                        .flex_col()
                        .border_r_1()
                        .border_color(ladder::trim())
                        .child(
                            div()
                                .p(px(10.))
                                .flex_none()
                                .child(editor.menu_search.clone())
                                .agent_node(Role::Input, "Search patterns…"),
                        )
                        .child(
                            float::viewport().child(
                                float::list()
                                    .id("insert-menu-list")
                                    .overflow_y_scroll()
                                    .track_scroll(&editor.menu_scroll)
                                    .when(rows.is_empty(), |list| {
                                        list.child(float::empty_row("No matching presets"))
                                    })
                                    .children(rows.iter().enumerate().map(|(index, choice)| {
                                        let choose = app.clone();
                                        let hover = app.clone();
                                        let name: SharedString = choice.name().to_owned().into();
                                        div()
                                            .id(SharedString::from(format!(
                                                "preview-{}",
                                                choice.id()
                                            )))
                                            .w_full()
                                            .on_hover(move |over, _, cx| {
                                                if *over {
                                                    hover.update(cx, |app, cx| {
                                                        app.with_track_editor(cx, |editor| {
                                                            if let Some(menu) = editor.menu.as_mut()
                                                            {
                                                                menu.active = index;
                                                            }
                                                        })
                                                    });
                                                }
                                            })
                                            .child(
                                                float::menu_row(
                                                    float::RowState::of(false, index == active),
                                                    format!("insert-{}", choice.id()),
                                                )
                                                .h(px(36.))
                                                .w_full()
                                                .px(px(10.))
                                                .child(name.clone())
                                                .child(div().flex_1())
                                                .child(
                                                    div()
                                                        .text_size(px(11.))
                                                        .text_color(ladder::muted_foreground())
                                                        .child(choice.origin()),
                                                )
                                                .id(SharedString::from(format!(
                                                    "insert-row-{}",
                                                    choice.id()
                                                )))
                                                .on_click(move |_, _, cx| {
                                                    choose.update(cx, |app, cx| {
                                                        app.with_track_editor(cx, |editor| {
                                                            if let Some(menu) = editor.menu.as_mut()
                                                            {
                                                                menu.active = index;
                                                            }
                                                        });
                                                        app.commit_insert_menu(cx);
                                                    })
                                                })
                                                .agent_node(Role::Row, name),
                                            )
                                    })),
                            ),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(TITLE_H))
                                .px(px(12.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    rows.get(active)
                                        .map_or("Preview", |choice| choice.name())
                                        .to_owned(),
                                )
                                .when(state.slow(), |header| {
                                    header.child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(ladder::muted_foreground())
                                            .child("Loading preview…"),
                                    )
                                }),
                        )
                        .child(div().flex_1().min_h_0().child(picker_preview::image(
                            "Pattern preview",
                            state.image.as_ref(),
                            state.scene_error.as_ref().or(state.error.as_ref()),
                        )))
                        .child(
                            div()
                                .h(px(NOTE_H))
                                .px(px(12.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .text_size(px(11.))
                                .text_color(ladder::muted_foreground())
                                .child("Uses visualizer settings"),
                        ),
                ),
        )
        .child(
            float::footer_band()
                .child(float::key_hint_text("↑ ↓", "Navigate"))
                .child(float::key_hint_text("↵", "Insert"))
                .child(float::key_hint_text("esc", "Cancel")),
        );
    morph::fixed_card("Insert pattern dialog", SIZE, content.into_any_element())
}
