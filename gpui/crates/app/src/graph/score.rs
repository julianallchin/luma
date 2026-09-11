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
    pub preview: super::preview::State,
    pub selected_output: Option<(String, String)>,
    pub catalog: Option<super::interaction::NodeMenu>,
    pub catalog_scroll: ScrollHandle,
    pub choice_open: Option<String>,
    pub catalog_query: String,
    pub search: Entity<luma_ui::text_input::TextInput>,
    pub _search_subscription: Subscription,
    pub controls: Option<super::controls::Controls>,
}

impl Editor {
    /// Auto layout is an opening aid. The first authored gesture freezes the
    /// arrangement the user is looking at before topology can change it.
    fn preserve_layout(&self) -> Vec<p::GraphEdit> {
        let source = &self.source;
        let Some(id) = self.edited_definition() else {
            return Vec::new();
        };
        let Some(parent) = source.library.definitions.get(&id) else {
            return Vec::new();
        };
        let p::Body::Graph(graph) = &parent.body else {
            return Vec::new();
        };
        let (mut nodes, mut inputs) = (
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
        );
        for card in &self.scene.borrow().cards {
            let position = [f64::from(card.origin.x), f64::from(card.origin.y)];
            if let Some(key) = luma_lib::node_graph::lighting::input_node_key(&card.node_id) {
                if graph
                    .input_nodes
                    .get(key)
                    .is_none_or(|node| node.position.is_none())
                {
                    inputs.insert(key.to_string(), position);
                }
            } else if graph
                .nodes
                .get(card.node_id.as_ref())
                .is_some_and(|node| node.position.is_none())
            {
                nodes.insert(card.node_id.to_string(), position);
            }
        }
        let mut edits = Vec::new();
        if !nodes.is_empty() {
            edits.push(p::GraphEdit::Move { positions: nodes });
        }
        if !inputs.is_empty() {
            edits.push(p::GraphEdit::MoveInputs { positions: inputs });
        }
        edits
    }

    pub(crate) fn score_subject(&self) -> Option<(String, String, String)> {
        let score = &self.source;
        Some((
            self.context.track.clone(),
            self.context.venue.clone(),
            score.score_id.clone(),
        ))
    }
    pub(crate) fn target(&self) -> Target {
        Target::ScoreGraph {
            score: self.source.score_id.clone(),
            graph: self.source.root.clone(),
        }
    }
    pub(super) fn inspecting_builtin(&self) -> bool {
        let id = self
            .inspection
            .last()
            .map(|(id, _)| id.as_str())
            .unwrap_or(&self.source.root);
        p::standard_library().definitions.contains_key(id)
    }
}

impl Luma {
    /// Graph tabs are views of the timeline's working score and history. Close
    /// those views when their score closes or the timeline switches scores.
    pub(crate) fn close_score_graph_tabs(
        &mut self,
        owner: &Target,
        keep: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let targets: Vec<_> = self
            .workspace
            .iter()
            .filter_map(|tab| {
                let TabBody::Graph(editor) = &tab.body else {
                    return None;
                };
                (editor.source.owner == *owner && Some(editor.source.score_id.as_str()) != keep)
                    .then(|| tab.target.clone())
            })
            .collect();
        for target in targets {
            self.close_tab(&target, None, cx);
        }
    }

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
        let draft = timeline.graph_draft(&clip.graph).cloned();
        let working = draft.as_ref().map_or(&score, |draft| &draft.candidate);
        let Ok(library) = working.library(&p::standard_library()) else {
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
                    let source = &mut editor.source;
                    source.catalog_query = text;
                    if let Some(menu) = &mut source.catalog {
                        menu.active = 0;
                    }
                    source.catalog_scroll.scroll_to_item(0);
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
            published: draft.as_ref().map_or(score, |draft| draft.base.clone()),
            draft: draft.as_ref().map(|draft| draft.candidate.clone()),
            preview: super::preview::State::default(),
            selected_output: None,
            catalog: None,
            catalog_scroll: ScrollHandle::new(),
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
        if matches!(self.workspace.body(&target), Some(TabBody::Graph(editor)) if editor.source.clip != source.clip)
        {
            self.stop_graph_preview(&target, cx);
        }
        if let Some(TabBody::Graph(editor)) = self.workspace.body_mut(&target) {
            {
                let existing = &mut editor.source;
                if existing.clip != source.clip {
                    existing.preview = super::preview::State::default();
                }
                existing.clip = source.clip.clone();
                existing.published = source.published;
                existing.draft = source.draft;
                editor.error = draft
                    .as_ref()
                    .map(|draft| format!("Draft · {}", draft.error));
                editor.refresh_score_view();
            }
            self.workspace.select(&target);
        } else {
            let mut editor = Box::new(Editor {
                source: Box::new(source),
                context,
                types,
                inspection: Vec::new(),
                scene: Rc::new(RefCell::new(Scene::default())),
                selected: Vec::new(),
                selected_edge: None,
                gesture: None,
                view: Rc::new(Cell::new(Viewport {
                    pan: point(px(0.), px(0.)),
                    zoom: 1.,
                })),
                fit: true,
                fitted_size: Rc::new(Cell::new(gpui::Size::default())),
                origin: Rc::new(Cell::new(Bounds::default().origin)),
                canvas_size: Rc::new(Cell::new(Size::default())),
                error: draft
                    .as_ref()
                    .map(|draft| format!("Draft · {}", draft.error)),
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
        let source = &editor.source;
        if source.draft.is_some() {
            self.stop_graph_preview(target, cx);
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
        let pending =
            self.library
                .prepare_score_clip_preview(&source.score_id, &source.clip, &score);
        let mut generation = 0;
        self.edit_graph_tab(target, cx, |editor| {
            editor.preview_generation += 1;
            generation = editor.preview_generation;
            editor.preview_running = true;
            editor.preview_error = None;
            {
                let source = &mut editor.source;
                *source.preview.failure.borrow_mut() = None;
            }
        });
        let target = target.clone();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let mut failed = false;
                this.edit_graph_tab(&target, cx, |editor| {
                    if editor.preview_generation != generation {
                        return;
                    }
                    editor.preview_running = false;
                    match result {
                        Ok(scene) => {
                            {
                                let source = &mut editor.source;
                                source.preview.set_scene(scene);
                            }
                            editor.preview_error = None;
                        }
                        Err(error) => {
                            failed = true;
                            editor.preview_error = Some(error.to_string());
                        }
                    }
                });
                if failed {
                    this.stop_graph_preview(&target, cx);
                }
            })
            .ok();
        })
        .detach();
    }
}

impl Editor {
    pub(super) fn edited_definition(&self) -> Option<String> {
        let score = &self.source;
        Some(
            self.inspection
                .last()
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| score.root.clone()),
        )
    }
    pub(super) fn refresh_score_view(&mut self) {
        let source = &mut self.source;
        let score = source.draft.as_ref().unwrap_or(&source.published);
        let Ok(library) = score.library(&p::standard_library()) else {
            return;
        };
        if let Some(view) =
            luma_lib::node_graph::lighting::project_definition(&library, &source.root)
        {
            source.view = Rc::new(view);
        } else {
            source.view = Rc::new(Graph {
                nodes: Vec::new(),
                edges: Vec::new(),
                args: Vec::new(),
            });
        }
        if let Some(missing) = self
            .inspection
            .iter()
            .position(|(id, _)| !library.definitions.contains_key(id))
        {
            self.inspection.truncate(missing);
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
        source.selected_output = None;
        if source.draft.is_some() {
            // A prior valid preview may still be in flight when undo/redo or a
            // deletion makes the graph incomplete. Retire that result as well.
            self.preview_generation += 1;
            self.preview_running = false;
            source.preview.scene = None;
            self.preview_error = Some("Finish connecting the graph to preview it".into());
        }
        self.gesture = None;
        self.rebuild();
    }
}

impl Luma {
    /// Keep all views of shared definitions in step with the timeline. Drafts
    /// retain their original base for the existing three-way merge on finish.
    pub(crate) fn sync_score_graph_tabs(&mut self, owner: &Target, cx: &mut Context<Self>) {
        let Some(TabBody::TrackEditor(timeline)) = self.workspace.body(owner) else {
            return;
        };
        let score_id = timeline.score_id().map(str::to_string);
        self.close_score_graph_tabs(owner, score_id.as_deref(), cx);
        let Some(score_id) = score_id else {
            return;
        };
        let Some(TabBody::TrackEditor(timeline)) = self.workspace.body(owner) else {
            return;
        };
        let Ok(current) = timeline.graph_candidate() else {
            return;
        };
        let targets: Vec<_> = self
            .workspace
            .iter()
            .filter_map(|tab| {
                let TabBody::Graph(editor) = &tab.body else {
                    return None;
                };
                let source = &editor.source;
                let draft = timeline.graph_draft(&source.root);
                let published = draft.map_or(&current, |draft| &draft.base);
                let candidate = draft.map(|draft| &draft.candidate);
                (source.owner == *owner
                    && source.score_id == score_id
                    && (&source.published != published || source.draft.as_ref() != candidate))
                    .then(|| {
                        (
                            tab.target.clone(),
                            published.clone(),
                            draft.cloned(),
                            !source.published.same_computation(published)
                                || source.draft.is_some() != candidate.is_some(),
                        )
                    })
            })
            .collect();
        for (target, published, draft, changed) in targets {
            self.edit_graph_tab(&target, cx, |editor| {
                let source = &mut editor.source;
                source.published = published;
                source.draft = draft.as_ref().map(|draft| draft.candidate.clone());
                editor.error = draft
                    .map(|draft| format!("Draft · {}", draft.error))
                    .or_else(|| {
                        (!current.definitions.contains_key(&source.root))
                            .then(|| "This graph was removed from the score".to_string())
                    });
                editor.refresh_score_view();
            });
            if changed {
                self.refresh_score_graph_preview(&target, cx);
            }
        }
    }

    pub(super) fn customize_score_graph_node(
        &mut self,
        target: &Target,
        node: &str,
        cx: &mut Context<Self>,
    ) {
        let result = (|| {
            let Some(TabBody::Graph(editor)) = self.workspace.body(target) else {
                return Ok(None);
            };
            let Some(graph) = editor.edited_definition() else {
                return Ok(None);
            };
            let source = &editor.source;
            if source.draft.is_some() {
                return Err(
                    "Finish connecting the current graph before customizing a node".to_string(),
                );
            }
            let owner = source.owner.clone();
            let score_id = source.score_id.clone();
            let Some(TabBody::TrackEditor(timeline)) = self.workspace.body_mut(&owner) else {
                return Ok(None);
            };
            if timeline.score_id() != Some(score_id.as_str()) {
                return Ok(None);
            }
            let mut candidate = timeline.graph_candidate()?;
            candidate
                .customize_node(
                    &p::standard_library(),
                    &graph,
                    node,
                    &uuid::Uuid::new_v4().to_string(),
                )
                .map_err(|error| error.to_string())?;
            timeline.publish_graph_edit(candidate.clone())?;
            Ok(Some((owner, candidate)))
        })();
        match result {
            Ok(Some((owner, candidate))) => {
                self.edit_graph_tab(target, cx, |editor| {
                    {
                        let source = &mut editor.source;
                        source.published = candidate;
                    }
                    editor.error = None;
                    editor.refresh_score_view();
                    editor.inspect_node(node);
                });
                self.refresh_working_scene_for(&owner, cx);
                self.commit_graph_score_for(owner, cx);
                self.refresh_score_graph_preview(target, cx);
            }
            Err(error) => self.edit_graph_tab(target, cx, |editor| editor.error = Some(error)),
            Ok(None) => (),
        }
    }

    pub(super) fn apply_score_graph_edit(
        &mut self,
        target: &Target,
        edit: p::GraphEdit,
        cx: &mut Context<Self>,
    ) {
        self.apply_score_graph_edits(target, vec![edit], cx);
    }

    pub(super) fn apply_score_graph_edits(
        &mut self,
        target: &Target,
        mut edits: Vec<p::GraphEdit>,
        cx: &mut Context<Self>,
    ) {
        if edits.is_empty() {
            return;
        }
        let Some(TabBody::Graph(editor)) = self.workspace.body(target) else {
            return;
        };
        let mut layout = editor.preserve_layout();
        layout.append(&mut edits);
        let edits = layout;
        let Some(definition) = editor.edited_definition() else {
            return;
        };
        let source = &editor.source;
        let owner = source.owner.clone();
        let score_id = source.score_id.clone();
        let root = source.root.clone();
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
        let added = edits.iter().find_map(|edit| match edit {
            p::GraphEdit::Add { id, .. } => Some(id.clone()),
            p::GraphEdit::AddInput { key, .. } => {
                Some(luma_lib::node_graph::lighting::input_node_id(key))
            }
            _ => None,
        });
        for edit in edits {
            if let Err(error) = candidate.edit_graph(&p::standard_library(), &definition, edit) {
                self.edit_graph_tab(target, cx, |editor| editor.error = Some(error.to_string()));
                return;
            }
        }
        if candidate == previous {
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
        let Some(TabBody::TrackEditor(timeline)) = self.workspace.body_mut(&owner) else {
            return;
        };
        let recorded = match &validation {
            Ok(()) => timeline.publish_graph_gesture(candidate.clone(), Some(&root)),
            Err(error) => timeline.record_graph_draft(
                &root,
                crate::track_editor::GraphDraft {
                    base: if had_draft { base } else { previous.clone() },
                    candidate: candidate.clone(),
                    error: error.clone(),
                },
            ),
        };
        if let Err(error) = recorded {
            self.edit_graph_tab(target, cx, |editor| editor.error = Some(error));
            return;
        }
        let valid = validation.is_ok();
        let changed = !previous.same_computation(&candidate) || had_draft;
        self.edit_graph_tab(target, cx, |editor| {
            let source = &mut editor.source;
            if source.draft.is_none() {
                source.published = previous.clone();
            }
            if valid {
                source.published = candidate;
                source.draft = None;
                editor.error = None;
            } else {
                source.draft = Some(candidate);
                editor.error = validation.err().map(|error| format!("Draft · {error}"));
            }
            editor.refresh_score_view();
            // Authored gestures keep the graph under the pointer.
            editor.fit = false;
            if let Some(id) = added {
                editor.selected = vec![id.into()];
                editor.selected_edge = None;
            }
        });
        if valid {
            self.refresh_working_scene_for(&owner, cx);
            self.commit_graph_score_for(owner, cx);
            if changed {
                self.refresh_score_graph_preview(target, cx);
            }
        } else {
            self.stop_graph_preview(target, cx);
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
        let source = &mut editor.source;
        let owner = source.owner.clone();
        let score_id = source.score_id.clone();
        let Some(TabBody::TrackEditor(timeline)) = self.workspace.body_mut(&owner) else {
            return;
        };
        if timeline.score_id() != Some(score_id.as_str()) || !timeline.step_graph_history(forward) {
            return;
        }
        self.refresh_working_scene_for(&owner, cx);
        self.commit_graph_score_for(owner, cx);
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
        let source = &editor.source;
        let Some(p::Definition {
            body: p::Body::Graph(graph),
            ..
        }) = source.library.definitions.get(&id)
        else {
            return;
        };
        if definition == "$input" {
            let mut index = 1;
            let mut key = format!("input_{index}");
            let parent = &source.library.definitions[&id];
            while graph.input_nodes.contains_key(&key) || parent.inputs.contains_key(&key) {
                index += 1;
                key = format!("input_{index}");
            }
            let position = source
                .catalog
                .as_ref()
                .map(|menu| menu.position)
                .unwrap_or([0., 0.]);
            self.edit_graph_tab(target, cx, |editor| {
                let source = &mut editor.source;
                source.catalog = None;
            });
            self.apply_score_graph_edit(
                target,
                p::GraphEdit::AddInput {
                    key,
                    name: "Input".into(),
                    position,
                },
                cx,
            );
            return;
        }
        let mut index = 1;
        let mut node = format!("{definition}_{index}");
        while graph.nodes.contains_key(&node) {
            index += 1;
            node = format!("{definition}_{index}");
        }
        let position = source.catalog.as_ref().map(|menu| menu.position);
        let mut edits = vec![p::GraphEdit::Add {
            id: node.clone(),
            definition: definition.into(),
        }];
        if let Some(position) = position {
            edits.push(p::GraphEdit::Move {
                positions: [(node, position)].into(),
            });
        }
        self.edit_graph_tab(target, cx, |editor| {
            let source = &mut editor.source;
            source.catalog = None;
        });
        self.apply_score_graph_edits(target, edits, cx);
    }
}
