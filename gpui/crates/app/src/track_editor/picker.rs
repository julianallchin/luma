//! Searchable pattern insertion with an isolated, looping venue preview.
use super::*;
use crate::{picker_preview, shell::Overlay};
use luma_lib::{
    models::universe::UniverseState,
    stage_render::{Continuity, Sequence},
};
use luma_ui::dialog::morph::{self, MorphSize};

const SIZE: MorphSize = MorphSize::new(880., 540.);
const PIXELS: (u32, u32) = (1040, 880);

pub(crate) struct Picker {
    generation: uuid::Uuid,
    target: Target,
    sequence: Option<Arc<Sequence>>,
    requested: Option<String>,
    loading: bool,
    frames: Arc<Vec<UniverseState>>,
    since: std::time::Instant,
    drawn: Option<usize>,
    drawing: bool,
    image: Option<Arc<RenderImage>>,
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
    let generation = uuid::Uuid::new_v4();
    app.overlay.open(Overlay::InsertPattern(Box::new(Picker {
        generation,
        target,
        sequence: None,
        requested: None,
        loading: false,
        frames: Arc::default(),
        since: std::time::Instant::now(),
        drawn: None,
        drawing: false,
        image: None,
        error: None,
        scene_error: None,
    })));
    cx.spawn(async move |this, cx| {
        let result = match rig.await {
            Ok(rig) => {
                cx.background_executor()
                    .spawn(async move { picker_preview::install(&rig, settings, PIXELS) })
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
    let choice = editor.menu_choice();
    let desired = choice.as_ref().map(|(_, choice)| choice.id());
    if desired.is_none() {
        if let Some(Overlay::InsertPattern(state)) = app.overlay.open_mut() {
            state.requested = None;
            state.frames = Arc::default();
            state.image = None;
            state.error = None;
            state.drawn = None;
        }
        return;
    }
    if picker.requested != desired && !picker.loading {
        if let Some((menu, choice)) = choice {
            let id = choice.id();
            let generation = picker.generation;
            let start = menu.start;
            // At most four seconds and sixty samples; the preview never drives DMX.
            let end = menu.end.min(start + 4.);
            let pending: std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<Vec<UniverseState>, String>>>,
            > = match &choice {
                InsertChoice::Pattern(pattern) => {
                    let task = app.library.preview_pattern_frames(
                        &pattern.id,
                        &editor.track_id,
                        &editor.venue_id,
                        start,
                        end,
                    );
                    Box::pin(async move { task.await.map_err(|error| error.to_string()) })
                }
                InsertChoice::Node {
                    effect: definition, ..
                }
                | InsertChoice::Graph { id: definition, .. } => {
                    let library = match editor
                        .graph_score
                        .as_ref()
                        .map(|graph| graph.library())
                        .transpose()
                    {
                        Ok(library) => library,
                        Err(error) => {
                            if let Some(Overlay::InsertPattern(state)) = app.overlay.open_mut() {
                                state.requested = Some(id);
                                state.frames = Arc::default();
                                state.error = Some(error.to_string());
                            }
                            return;
                        }
                    };
                    let count = (((end - start) * 15.).ceil() as usize).clamp(1, 60);
                    let task = app.library.preview_definition_frames(
                        luma_lib::models::composable_patterns::ComposablePreviewRequest {
                            venue_id: editor.venue_id.clone(),
                            track_id: editor.track_id.clone(),
                            definition: definition.clone(),
                            library,
                            inputs: Default::default(),
                            targets: vec![luma_lib::models::selection::Selection::new("all")],
                            times: (0..count)
                                .map(|i| start + i as f64 * (end - start) / count as f64)
                                .collect(),
                            clip_start: start,
                            clip_end: menu.end,
                            seed: 0,
                        },
                    );
                    Box::pin(async move {
                        task.await
                            .map(|preview| preview.frames)
                            .map_err(|error| error.to_string())
                    })
                }
            };
            if let Some(Overlay::InsertPattern(state)) = app.overlay.open_mut() {
                state.loading = true;
                state.error = None;
                state.requested = Some(id.clone());
                state.frames = Arc::default();
                state.drawn = None;
            }
            cx.spawn(async move |this, cx| {
                let result = pending.await;
                this.update(cx, |this, cx| {
                    let current = match this.workspace.active_body() {
                        Some(Body::TrackEditor(editor)) => {
                            editor.menu_choice().map(|(_, choice)| choice.id())
                        }
                        _ => None,
                    };
                    if let Some(Overlay::InsertPattern(state)) = this.overlay.open_mut() {
                        if state.generation != generation {
                            return;
                        }
                        state.loading = false;
                        if current.as_ref() == Some(&id) {
                            match result {
                                Ok(frames) => {
                                    state.frames = Arc::new(frames);
                                    state.since = std::time::Instant::now();
                                }
                                Err(error) => state.error = Some(error),
                            }
                        }
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }
    let Some(Overlay::InsertPattern(state)) = app.overlay.open_mut() else {
        return;
    };
    if state.frames.is_empty() || state.drawing {
        return;
    }
    let Some(sequence) = state.sequence.clone() else {
        return;
    };
    let index = (state.since.elapsed().as_secs_f32() * 15.) as usize % state.frames.len();
    window.request_animation_frame();
    if state.drawn == Some(index) {
        return;
    }
    let cut = state.drawn.is_none() || index == 0;
    let frame = state.frames[index].clone();
    let id = state.requested.clone();
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
                        if cut {
                            luma_render::DEFAULT_SUBFRAMES
                        } else {
                            1
                        },
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
                if state.requested != id || state.loading {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(image) => {
                        state.image = Some(image);
                        state.drawn = Some(index);
                    }
                    Err(error) => state.error = Some(error),
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
    let rows = editor.insertion_choices();
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
                        .w(px(360.))
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
                                        list.child(float::empty_row("No matching patterns"))
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
                                .p(px(12.))
                                .flex_none()
                                .flex()
                                .flex_col()
                                .gap(px(4.))
                                .child(
                                    rows.get(active)
                                        .map_or("Preview", |choice| choice.name())
                                        .to_owned(),
                                )
                                .when(
                                    rows.get(active).is_some()
                                        && state.drawn.is_none()
                                        && state.error.is_none()
                                        && state.scene_error.is_none(),
                                    |header| {
                                        header.child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(ladder::muted_foreground())
                                                .child("Loading preview…"),
                                        )
                                    },
                                ),
                        )
                        .child(div().flex_1().min_h_0().child(picker_preview::image(
                            "Pattern preview",
                            state.image.as_ref(),
                            state.scene_error.as_ref().or(state.error.as_ref()),
                        )))
                        .child(
                            div()
                                .p(px(10.))
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
