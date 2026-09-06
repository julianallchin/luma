//! Score-local graph views. The timeline owns the editable score and its history.
use super::*;
use luma_patterns as p;

pub(super) struct ScoreGraph {
    pub score_id: String,
    pub root: String,
    pub clip: String,
    pub owner: Target,
    pub label: String,
    pub library: p::Library,
    pub view: Rc<Graph>,
    pub published: p::Score,
    pub draft: Option<p::Score>,
    pub draft_history: History<p::Score>,
    pub pending_port: Option<(String, String)>,
    pub catalog_open: bool,
    pub choice_open: Option<String>,
    pub catalog_query: String,
    pub search: Entity<luma_ui::text_input::TextInput>,
    pub _search_subscription: Subscription,
    pub controls: Option<super::controls::Controls>,
}

impl Editor {
    pub(crate) fn score_subject(&self) -> Option<(String, String, String)> {
        let Source::Score(score) = &self.source else {
            return None;
        };
        Some((
            self.context.track.clone(),
            self.context.venue.clone(),
            score.score_id.clone(),
        ))
    }
    pub(super) fn target(&self) -> Target {
        match &self.source {
            Source::Legacy(pattern) => Target::Graph {
                pattern: pattern.id.clone(),
            },
            Source::Score(score) => Target::ScoreGraph {
                score: score.score_id.clone(),
                graph: score.root.clone(),
            },
        }
    }
    pub(super) fn inspecting_builtin(&self) -> bool {
        match &self.source {
            Source::Legacy(_) => !self.inspection.is_empty(),
            Source::Score(source) => {
                let id = self
                    .inspection
                    .last()
                    .map(|(id, _)| id.as_str())
                    .unwrap_or(&source.root);
                p::standard_library().definitions.contains_key(id)
            }
        }
    }
}

impl Luma {
    pub(crate) fn open_score_graph(
        &mut self,
        owner: Target,
        clip_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(TabBody::TrackEditor(timeline)) = self.workspace.body(&owner) else {
            return;
        };
        let Some(score_id) = timeline.score_id() else {
            return;
        };
        let score = match timeline.graph_candidate() {
            Ok(score) => score,
            Err(_) => return,
        };
        let Some(clip) = score.clips.get(&clip_id) else {
            return;
        };
        let Ok(library) = score.library(&p::standard_library()) else {
            return;
        };
        let Some(view) = luma_lib::node_graph::lighting::project_definition(&library, &clip.graph)
        else {
            return;
        };
        let label = timeline
            .graph_label(&clip.graph)
            .unwrap_or_else(|| "Custom effect".into());
        let context = TrackContext {
            track: match &owner {
                Target::TrackEditor { track, .. } => track.clone(),
                _ => return,
            },
            venue: timeline.venue_id().to_string(),
            track_name: timeline.track_name().to_string().into(),
        };
        let target = Target::ScoreGraph {
            score: score_id.to_string(),
            graph: clip.graph.clone(),
        };
        let search = cx.new(|cx| luma_ui::text_input::TextInput::search("Search nodes…", cx));
        let search_target = target.clone();
        let subscription = cx.subscribe(&search, move |this, field, event, cx| {
            if event == &luma_ui::text_input::Event::Edited {
                let text = field.read(cx).text().to_string();
                this.edit_graph_tab(&search_target, cx, |editor| {
                    if let Source::Score(source) = &mut editor.source {
                        source.catalog_query = text;
                    }
                });
            }
        });
        let source = ScoreGraph {
            score_id: score_id.to_string(),
            root: clip.graph.clone(),
            clip: clip_id,
            owner,
            label,
            view: Rc::new(view),
            library,
            published: score,
            draft: None,
            draft_history: History::default(),
            pending_port: None,
            catalog_open: false,
            choice_open: None,
            catalog_query: String::new(),
            search,
            _search_subscription: subscription,
            controls: None,
        };
        let types = Rc::new(
            luma_lib::node_graph::lighting::types_for(&source.library)
                .into_iter()
                .map(|node| (node.id.clone(), node))
                .collect(),
        );
        if let Some(TabBody::Graph(editor)) = self.workspace.body_mut(&target) {
            if let Source::Score(existing) = &mut editor.source {
                existing.clip = source.clip.clone();
                if existing.draft.is_none() {
                    existing.published = source.published;
                    editor.refresh_score_view();
                }
            }
            self.workspace.select(&target);
        } else {
            let mut editor = Box::new(Editor {
                source: Source::Score(source),
                context,
                types,
                views: ViewData::snapshot(cx),
                document: None,
                inspection: Vec::new(),
                scene: Rc::new(RefCell::new(Scene::default())),
                selected: Vec::new(),
                history: History::default(),
                gesture: None,
                view: Rc::new(Cell::new(Viewport {
                    pan: point(px(0.), px(0.)),
                    zoom: 1.,
                })),
                fit: true,
                fitted_size: Rc::new(Cell::new(gpui::Size::default())),
                origin: Rc::new(Cell::new(Bounds::default().origin)),
                saving: false,
                dirty: false,
                error: None,
                preview: None,
                preview_error: None,
                preview_generation: 0,
                preview_running: false,
            });
            editor.rebuild();
            self.open_tab(target.clone(), move || TabBody::Graph(editor), cx);
        }
        self.refresh_score_graph_preview(&target, cx);
        cx.notify();
    }

    pub(super) fn refresh_score_graph_preview(&mut self, target: &Target, cx: &mut Context<Self>) {
        let Some(TabBody::Graph(editor)) = self.workspace.body(target) else {
            return;
        };
        let Source::Score(source) = &editor.source else {
            return;
        };
        if source.draft.is_some() {
            return;
        }
        let Some(TabBody::TrackEditor(timeline)) = self.workspace.body(&source.owner) else {
            return;
        };
        if timeline.score_id() != Some(source.score_id.as_str()) {
            return;
        }
        let score = match timeline.graph_candidate() {
            Ok(score) => score,
            Err(_) => return,
        };
        let pending = self
            .library
            .preview_score_clip(&source.score_id, &source.clip, &score);
        let mut generation = 0;
        self.edit_graph_tab(target, cx, |editor| {
            editor.preview_generation += 1;
            generation = editor.preview_generation;
            editor.preview_running = true;
        });
        let target = target.clone();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                this.edit_graph_tab(&target, cx, |editor| {
                    if editor.preview_generation != generation {
                        return;
                    }
                    editor.preview_running = false;
                    match result {
                        Ok(row) => {
                            let mut pixels = row.pixels;
                            for pixel in pixels.chunks_exact_mut(4) {
                                pixel.swap(0, 2);
                            }
                            editor.preview = image::RgbaImage::from_raw(
                                row.width, row.height, pixels,
                            )
                            .map(|image| {
                                std::sync::Arc::new(RenderImage::new([image::Frame::new(image)]))
                            });
                            editor.preview_error = editor
                                .preview
                                .is_none()
                                .then(|| "Preview returned an invalid image".into());
                        }
                        Err(error) => {
                            editor.preview = None;
                            editor.preview_error = Some(error.to_string());
                        }
                    }
                });
            })
            .ok();
        })
        .detach();
    }
}

impl Editor {
    pub(super) fn edited_definition(&self) -> Option<String> {
        let Source::Score(score) = &self.source else {
            return None;
        };
        Some(
            self.inspection
                .last()
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| score.root.clone()),
        )
    }
    pub(super) fn refresh_score_view(&mut self) {
        let Source::Score(source) = &mut self.source else {
            return;
        };
        let score = source.draft.as_ref().unwrap_or(&source.published);
        let Ok(library) = score.library(&p::standard_library()) else {
            return;
        };
        if let Some(view) =
            luma_lib::node_graph::lighting::project_definition(&library, &source.root)
        {
            source.view = Rc::new(view);
        }
        for (id, view) in &mut self.inspection {
            if let Some(projected) =
                luma_lib::node_graph::lighting::project_definition(&library, id)
            {
                *view = Rc::new(projected);
            }
        }
        self.types = Rc::new(
            luma_lib::node_graph::lighting::types_for(&library)
                .into_iter()
                .map(|node| (node.id.clone(), node))
                .collect(),
        );
        source.library = library;
        source.label = source.library.display_name(&source.root);
        source.pending_port = None;
        self.rebuild();
    }
}

impl Luma {
    pub(super) fn apply_score_graph_edit(
        &mut self,
        target: &Target,
        edit: p::GraphEdit,
        cx: &mut Context<Self>,
    ) {
        let Some(TabBody::Graph(editor)) = self.workspace.body(target) else {
            return;
        };
        let Some(definition) = editor.edited_definition() else {
            return;
        };
        let Source::Score(source) = &editor.source else {
            return;
        };
        let owner = source.owner.clone();
        let score_id = source.score_id.clone();
        let Some(TabBody::TrackEditor(timeline)) = self.workspace.body(&owner) else {
            return;
        };
        if timeline.score_id() != Some(score_id.as_str()) {
            return;
        }
        let base = source.published.clone();
        let had_draft = source.draft.is_some();
        let current = match timeline.graph_candidate() {
            Ok(score) => score,
            Err(_) => return,
        };
        let previous = match &source.draft {
            Some(draft) => draft.clone(),
            None => current.clone(),
        };
        let mut candidate = previous.clone();
        let reframe = matches!(
            &edit,
            p::GraphEdit::Add { .. }
                | p::GraphEdit::Remove { .. }
                | p::GraphEdit::Bind {
                    binding: Some(p::Binding::Connection { .. }),
                    ..
                }
                | p::GraphEdit::Output { .. }
        );
        let moved = matches!(&edit, p::GraphEdit::Move { .. });
        let added = match &edit {
            p::GraphEdit::Add { id, .. } => Some(id.clone()),
            _ => None,
        };
        if let Err(error) = candidate.edit_graph(&p::standard_library(), &definition, edit) {
            self.edit_graph_tab(target, cx, |editor| editor.error = Some(error.to_string()));
            return;
        }
        let mut validation = candidate
            .validate(&p::standard_library())
            .map_err(|e| e.to_string());
        if validation.is_ok() && had_draft {
            match luma_lib::services::graph_scores::merge_working_copy(&base, &current, &candidate)
            {
                Ok(merged) => candidate = merged,
                Err(error) => validation = Err(error),
            }
        }
        if validation.is_ok() {
            let mut refused = None;
            if let Some(TabBody::TrackEditor(timeline)) = self.workspace.body_mut(&owner) {
                refused = timeline.publish_graph_edit(candidate.clone()).err();
            }
            if let Some(error) = refused {
                self.edit_graph_tab(target, cx, |editor| editor.error = Some(error));
                return;
            }
        }
        let valid = validation.is_ok();
        self.edit_graph_tab(target, cx, |editor| {
            let Source::Score(source) = &mut editor.source else {
                return;
            };
            if source.draft.is_none() {
                source.published = previous.clone();
            }
            if valid {
                source.published = candidate;
                source.draft = None;
                source.draft_history = History::default();
                editor.error = None;
            } else {
                source.draft_history.record(previous);
                source.draft = Some(candidate);
                editor.error = validation.err().map(|error| format!("Draft · {error}"));
                editor.preview = None;
                editor.preview_error = Some("Finish connecting the graph to preview it".into());
            }
            editor.refresh_score_view();
            if moved {
                editor.fit = false;
            } else if reframe {
                editor.fit = true;
                editor.fitted_size.set(gpui::Size::default());
            }
            if let Some(id) = added {
                editor.selected = vec![id.into()];
            }
        });
        if valid {
            self.refresh_working_scene_for(&owner, cx);
            self.commit_graph_score_for(owner, cx);
            self.refresh_score_graph_preview(target, cx);
        }
    }

    pub(super) fn step_score_graph_history(
        &mut self,
        target: &Target,
        forward: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(TabBody::Graph(editor)) = self.workspace.body_mut(target) else {
            return;
        };
        let Source::Score(source) = &mut editor.source else {
            return;
        };
        if let Some(current) = source.draft.as_ref() {
            let next = if forward {
                source.draft_history.redo(current.clone())
            } else {
                source.draft_history.undo(current.clone())
            };
            if let Some(next) = next {
                source.draft = (next != source.published).then_some(next);
                editor.error = source
                    .draft
                    .as_ref()
                    .and_then(|draft| draft.validate(&p::standard_library()).err())
                    .map(|e| format!("Draft · {e}"));
                editor.refresh_score_view();
                self.refresh_score_graph_preview(target, cx);
                cx.notify();
            }
            return;
        }
        let owner = source.owner.clone();
        let score_id = source.score_id.clone();
        let Some(TabBody::TrackEditor(timeline)) = self.workspace.body_mut(&owner) else {
            return;
        };
        if timeline.score_id() != Some(score_id.as_str()) || !timeline.step_graph_history(forward) {
            return;
        }
        let Ok(score) = timeline.graph_candidate() else {
            return;
        };
        self.edit_graph_tab(target, cx, |editor| {
            if let Source::Score(source) = &mut editor.source {
                source.published = score;
            }
            editor.error = None;
            editor.refresh_score_view();
        });
        self.refresh_working_scene_for(&owner, cx);
        self.commit_graph_score_for(owner, cx);
        self.refresh_score_graph_preview(target, cx);
    }

    /// Click an output, then an input. Wire type/rate checks belong to GraphEdit.
    pub(super) fn score_graph_port_press(
        &mut self,
        target: &Target,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(TabBody::Graph(editor)) = self.workspace.body_mut(target) else {
            return false;
        };
        if !matches!(editor.source, Source::Score(_)) {
            return false;
        }
        let cursor = editor.view.get().to_graph(editor.origin.get(), at);
        let hit = editor.scene.borrow().hit(cursor, editor.view.get().zoom);
        let Hit::Port { card, port, output } = hit else {
            return false;
        };
        let (node, port) = {
            let scene = editor.scene.borrow();
            let card = &scene.cards[card];
            (
                card.node_id.to_string(),
                if output {
                    card.outputs[port].id.to_string()
                } else {
                    card.inputs[port].id.to_string()
                },
            )
        };
        editor.selected = vec![node.clone().into()];
        if editor.inspecting_builtin() {
            cx.notify();
            return true;
        }
        let Source::Score(source) = &mut editor.source else {
            unreachable!()
        };
        if output {
            source.pending_port = Some((node, port));
        } else if let Some((from, output)) = source.pending_port.take() {
            self.apply_score_graph_edit(
                target,
                p::GraphEdit::Bind {
                    node,
                    input: port,
                    binding: Some(p::Binding::Connection { node: from, output }),
                },
                cx,
            );
        }
        cx.notify();
        true
    }
}

impl Luma {
    pub(super) fn add_score_graph_node(
        &mut self,
        target: &Target,
        definition: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(TabBody::Graph(editor)) = self.workspace.body(target) else {
            return;
        };
        let Some(id) = editor.edited_definition() else {
            return;
        };
        let Source::Score(source) = &editor.source else {
            return;
        };
        let Some(p::Definition {
            body: p::Body::Graph(graph),
            ..
        }) = source.library.definitions.get(&id)
        else {
            return;
        };
        let mut index = 1;
        let mut node = format!("{definition}_{index}");
        while graph.nodes.contains_key(&node) {
            index += 1;
            node = format!("{definition}_{index}");
        }
        self.apply_score_graph_edit(
            target,
            p::GraphEdit::Add {
                id: node,
                definition: definition.into(),
            },
            cx,
        );
    }
}
