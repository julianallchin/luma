//! The visualizer auditions one compiled clip. Its cursor and loop preference
//! are temporary; the score supplies timing, selection and argument overrides.
use super::*;
use luma_lib::services::graph_scores::ClipPreview;
use std::sync::Arc;
use std::time::Duration;
mod inspection;

/// Audio outlives tabs. A monotonically increasing request identity prevents a
/// closed/reopened graph from accepting a load belonging to its previous tab.
#[derive(Default)]
pub(crate) struct Audio {
    target: Option<Target>,
    session: Option<u64>,
}

#[derive(Clone, Default)]
pub(super) struct State {
    pub scene: Option<Arc<ClipPreview>>,
    scratch: Rc<RefCell<luma_lib::eval::Arena>>,
    pub failure: Rc<RefCell<Option<String>>>,
    pub cursor: Option<f32>,
    pub playing: bool,
    session: Option<u64>,
    pub starting: bool,
    pub looping: bool,
    inspection: inspection::State,
}

impl State {
    pub(super) fn set_scene(&mut self, scene: ClipPreview) {
        self.cursor = Some(
            self.cursor
                .unwrap_or(scene.span.0)
                .clamp(scene.span.0, scene.span.1),
        );
        self.inspection.prepare(&scene);
        self.scene = Some(Arc::new(scene));
    }
}

#[derive(Clone)]
pub(crate) struct View {
    pub target: Target,
    label: String,
    context: String,
    state: State,
    error: Option<String>,
    loading: bool,
}

impl Editor {
    pub(crate) fn preview_view(&self) -> Option<View> {
        let source = &self.source;
        let clip = source.published.clips.get(&source.clip);
        Some(View {
            target: self.target(),
            label: source.label.clone(),
            context: clip.map_or_else(
                || "Clip removed".into(),
                |clip| {
                    format!(
                        "{} · {} beats · {} · {} clip overrides",
                        self.context.track_name,
                        clip.duration,
                        clip.selection.expression,
                        clip.inputs.len(),
                    )
                },
            ),
            state: source.preview.clone(),
            error: self.preview_error.clone(),
            loading: self.preview_running,
        })
    }
}

impl View {
    pub(crate) fn time(&self, library: &crate::library::Library) -> f32 {
        let span = self
            .state
            .scene
            .as_ref()
            .map_or((0., 0.), |scene| scene.span);
        if self.state.playing {
            self.state
                .session
                .and_then(|session| library.preview_time(session))
                .or(self.state.cursor)
                .unwrap_or(span.0)
                .clamp(span.0, span.1)
        } else {
            self.state.cursor.unwrap_or(span.0).clamp(span.0, span.1)
        }
    }

    pub(crate) fn sample(
        &self,
        time: f32,
    ) -> Result<luma_lib::models::universe::UniverseState, String> {
        let result = self
            .state
            .scene
            .as_ref()
            .map(|scene| {
                scene
                    .scene
                    .try_render(
                        &[time],
                        luma_lib::eval::Scope::Composite,
                        &mut self.state.scratch.borrow_mut(),
                    )
                    .map(|mut frames| frames.pop().unwrap_or_default())
            })
            .unwrap_or_else(|| Ok(Default::default()));
        *self.state.failure.borrow_mut() = result.as_ref().err().cloned();
        result
    }
}

impl Luma {
    fn with_preview(&mut self, target: &Target, edit: impl FnOnce(&mut State)) {
        if let Some(TabBody::Graph(editor)) = self.workspace.body_mut(target) {
            {
                let source = &mut editor.source;
                edit(&mut source.preview);
            }
        }
    }

    pub(crate) fn stop_graph_preview(&mut self, target: &Target, cx: &mut Context<Self>) {
        let session = if self.graph_audio.target.as_ref() == Some(target) {
            self.graph_audio.target = None;
            self.graph_audio.session.take()
        } else {
            None
        };
        let time = session.and_then(|session| self.library.preview_time(session));
        self.with_preview(target, |state| {
            if state.playing {
                state.cursor = time.or(state.cursor);
            }
            state.session = None;
            state.playing = false;
            state.starting = false;
        });
        if let Some(session) = session {
            let pause = self.library.pause(session);
            cx.background_spawn(async move {
                if let Err(error) = pause.await {
                    eprintln!("Failed to stop graph preview: {error}");
                }
            })
            .detach();
        }

        cx.notify();
    }

    fn preview_current(&self, target: &Target, session: u64) -> bool {
        self.workspace.active() == Some(target)
            && self.graph_audio.target.as_ref() == Some(target)
            && self.graph_audio.session == Some(session)
    }

    pub(crate) fn toggle_graph_preview(&mut self, target: &Target, cx: &mut Context<Self>) {
        let Some(TabBody::Graph(editor)) = self.workspace.body(target) else {
            return;
        };
        let source = &editor.source;
        if source.preview.playing || source.preview.starting {
            self.stop_graph_preview(target, cx);
            return;
        }
        let Some(scene) = &source.preview.scene else {
            return;
        };
        let span = scene.span;
        let mut time = source
            .preview
            .cursor
            .unwrap_or(span.0)
            .clamp(span.0, span.1);
        if time >= span.1 {
            time = span.0;
        }
        let track = editor.context.track.clone();
        let (session, load) = self.library.load_audio(&track);
        self.graph_audio.session = Some(session);
        self.graph_audio.target = Some(target.clone());
        self.with_preview(target, |state| {
            state.starting = true;
            state.session = Some(session);
            state.cursor = Some(time);
            *state.failure.borrow_mut() = None;
        });
        self.edit_graph_tab(target, cx, |editor| editor.preview_error = None);
        let target = target.clone();
        cx.spawn(async move |this, cx| {
            let result = async {
                load.await?;
                // Session ownership makes late controls harmless after navigation.
                let Some(region) = this
                    .read_with(cx, |this, _| {
                        if !this.preview_current(&target, session) { return None; }
                        let Some(TabBody::Graph(editor)) = this.workspace.body(&target) else { return None };
                        let source = &editor.source;
                        Some(this.library.set_preview_range(session, span, source.preview.looping))
                    })
                    .ok()
                    .flatten()
                else {
                    return Ok(false);
                };
                region.await?;
                let Some(play) = this
                    .read_with(cx, |this, _| {
                        this.preview_current(&target, session)
                            .then(|| this.library.play(session, time))
                    })
                    .ok()
                    .flatten()
                else {
                    return Ok(false);
                };
                play.await?;
                Ok::<_, crate::library::LibraryError>(true)
            }
            .await;
            let started = this
                .update(cx, |this, cx| {
                    if !this.preview_current(&target, session) {
                        return false;
                    }
                    let playing = matches!(&result, Ok(true));
                    this.with_preview(&target, |state| {
                        state.starting = false;
                        state.playing = playing;
                    });
                    if let Err(error) = result {
                        this.stop_graph_preview(&target, cx);
                        this.edit_graph_tab(&target, cx, |editor| {
                            editor.preview_error = Some(error.to_string())
                        });
                    }
                    cx.notify();
                    playing
                })
                .unwrap_or(false);
            if !started {
                return;
            }
            loop {
                let Ok(pending) = this.read_with(cx, |this, _| {
                    this.library.transport_after(Duration::from_millis(30))
                }) else {
                    break;
                };
                let snapshot = pending.await;
                let again = this
                    .update(cx, |this, cx| {
                        if !this.preview_current(&target, session) {
                            return false;
                        }
                        let failure = match this.workspace.body(&target) {
                            Some(TabBody::Graph(editor)) => editor.source.preview.failure.borrow().clone(),
                            _ => None,
                        };
                        if let Some(error) = failure {
                            this.stop_graph_preview(&target, cx);
                            this.edit_graph_tab(&target, cx, |editor| editor.preview_error = Some(error));
                            return false;
                        }
                        let looping = matches!(this.workspace.body(&target), Some(TabBody::Graph(editor)) if editor.source.preview.looping);
                        match snapshot {
                            Ok(snapshot)
                                if snapshot.session == session
                                    && snapshot.is_playing
                                    && (looping || snapshot.current_time < span.1) =>
                            {
                                this.with_preview(&target, |state| {
                                    state.cursor = Some(snapshot.current_time)
                                });
                                cx.notify();
                                true
                            }
                            Err(error) => {
                                this.stop_graph_preview(&target, cx);
                                this.edit_graph_tab(&target, cx, |editor| editor.preview_error = Some(error.to_string()));
                                false
                            }
                            Ok(_) => {
                                this.stop_graph_preview(&target, cx);
                                false
                            }
                        }
                    })
                    .unwrap_or(false);
                if !again {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn scrub_graph_preview(&mut self, target: &Target, seconds: f32, cx: &mut Context<Self>) {
        self.edit_graph_tab(target, cx, |editor| editor.preview_error = None);
        let mut playing = false;
        self.with_preview(target, |state| {
            state.cursor = Some(seconds);
            playing = state.playing;
        });
        if playing {
            let Some(session) = self.graph_audio.session else {
                return;
            };
            let pending = self.library.seek(session, seconds);
            let target = target.clone();
            cx.spawn(async move |this, cx| {
                if let Err(error) = pending.await {
                    this.update(cx, |this, cx| {
                        this.edit_graph_tab(&target, cx, |editor| {
                            editor.preview_error = Some(error.to_string())
                        })
                    })
                    .ok();
                }
            })
            .detach();
        }
        cx.notify();
    }

    fn toggle_graph_preview_loop(&mut self, target: &Target, cx: &mut Context<Self>) {
        let mut region = None;
        self.with_preview(target, |state| {
            state.looping = !state.looping;
            if state.playing {
                region = state
                    .scene
                    .as_ref()
                    .map(|scene| (scene.span, state.looping));
            }
        });
        if let Some((span, looping)) = region {
            let Some(session) = self.graph_audio.session else {
                return;
            };
            let pending = self.library.set_preview_range(session, span, looping);
            let target = target.clone();
            cx.spawn(async move |this, cx| {
                if let Err(error) = pending.await {
                    this.update(cx, |this, cx| {
                        this.edit_graph_tab(&target, cx, |editor| {
                            editor.preview_error = Some(error.to_string())
                        })
                    })
                    .ok();
                }
            })
            .detach();
        }
        cx.notify();
    }
}

pub(crate) fn controls(
    view: &View,
    app: &Entity<Luma>,
    library: &crate::library::Library,
) -> impl IntoElement {
    let mut row = div().flex().items_center().gap(px(8.));
    let label = if view.state.starting {
        "Cancel preview"
    } else if view.state.playing {
        "Pause preview"
    } else {
        "Play preview"
    };
    let ready = view.state.scene.is_some() || view.state.starting;
    let target = view.target.clone();
    let play = app.clone();
    row = row.child(
        luma_ui::button(label, ready.into())
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                if ready {
                    play.update(cx, |this, cx| this.toggle_graph_preview(&target, cx));
                }
            })
            .agent_node(Role::Button, label),
    );
    let target = view.target.clone();
    let looping = app.clone();
    let loop_label = if view.state.looping {
        "Loop on"
    } else {
        "Loop off"
    };
    row = row.child(
        luma_ui::button(loop_label, luma_ui::Enabled::Yes)
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                looping.update(cx, |this, cx| this.toggle_graph_preview_loop(&target, cx));
            })
            .agent_node(Role::Button, loop_label),
    );
    if let Some(scene) = &view.state.scene {
        let (start, end) = scene.span;
        let time = view.time(library) - start;
        let target = view.target.clone();
        let scrub = app.clone();
        row = row
            .child(
                luma_ui::luma_slider(
                    "Preview time",
                    time,
                    0.,
                    end - start,
                    180.,
                    move |value, _, cx| {
                        scrub.update(cx, |this, cx| {
                            this.scrub_graph_preview(&target, start + value, cx)
                        });
                    },
                )
                .agent_node(Role::Slider, "Preview time"),
            )
            .child(format!("{time:.2} / {:.2} s", end - start));
    }
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(4.))
        .px(px(12.))
        .py(px(6.))
        .text_size(px(12.))
        .text_color(ladder::foreground())
        .child(
            div()
                .flex()
                .gap(px(12.))
                .child(format!("Preview · {}", view.label))
                .when(view.loading, |el| el.child("Preparing preview…")),
        )
        .child(row)
        .child(
            div()
                .text_color(ladder::muted_foreground())
                .child(view.context.clone()),
        )
        .when_some(
            view.error
                .clone()
                .or_else(|| view.state.failure.borrow().clone()),
            |el, error| {
                el.child(
                    div()
                        .text_color(ladder::danger())
                        .child(error.clone())
                        .agent_node(Role::Text, error),
                )
            },
        )
        .child(inspection::controls(view, app, library))
        .agent_node(Role::Card, "Clip preview transport")
}
