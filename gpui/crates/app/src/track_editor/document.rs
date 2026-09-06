//! The canonical graph score behind the native timeline. Seconds are a view of
//! its beat positions; graphs and clips are always saved in one document.
use super::*;
use luma_lib::models::node_graph::{PatternArgDef, PatternArgType};
use luma_lib::services::graph_scores::GraphScoreDocument;
use luma_patterns as p;
use std::collections::BTreeMap;

pub(super) const SELECTION_INPUT: &str = "@clip/selection";

pub(super) struct GraphState {
    pub base: GraphScoreDocument,
    pub definitions: Rc<BTreeMap<String, p::Definition>>,
    pub composited_definitions: Rc<BTreeMap<String, p::Definition>>,
}

impl GraphState {
    pub fn new(document: GraphScoreDocument) -> Self {
        let definitions = Rc::new(document.score.definitions.clone());
        Self {
            base: document,
            composited_definitions: definitions.clone(),
            definitions,
        }
    }

    pub fn library(&self) -> p::Result<p::Library> {
        let mut score = p::Score::default();
        score.definitions = (*self.definitions).clone();
        score.library(&p::standard_library())
    }
}

pub(super) fn wire_value(value: &p::Value) -> serde_json::Value {
    match value {
        p::Value::Color(color) => {
            serde_json::json!({"r":color[0]*255.0,"g":color[1]*255.0,"b":color[2]*255.0,"a":1.0})
        }
        // Preserve orientation, circle origin and grouping through unrelated
        // clip edits; reducing Mapping to its menu label loses those values.
        _ => serde_json::to_value(value).expect("typed value serializes")["value"].clone(),
    }
}

pub(super) fn resolve_document(
    document: &GraphScoreDocument,
    beats: Option<&BeatGrid>,
) -> Result<Rc<[Clip]>, String> {
    if document.score.clips.is_empty() {
        return Ok(Vec::new().into());
    }
    let clock = beats
        .ok_or("Waiting for the track's beat grid")?
        .timeline()
        .map_err(|error| error.to_string())?;
    let library = document
        .score
        .library(&p::standard_library())
        .map_err(|error| error.to_string())?;
    let rows = rows_by_z(document.score.clips.values().map(|clip| clip.z_index));
    document
        .score
        .clips
        .iter()
        .map(|(id, clip)| {
            Ok(Clip {
                id: id.clone().into(),
                pattern: clip.graph.clone().into(),
                label: library.display_name(&clip.graph).into(),
                color: ladder::pattern(&clip.graph),
                start: clock
                    .seconds_at(clip.start)
                    .map_err(|error| error.to_string())?,
                end: clock
                    .seconds_at(clip.start + clip.duration)
                    .map_err(|error| error.to_string())?,
                row: rows[&clip.z_index],
                z: clip.z_index,
                blend: clip.blend_mode,
                args: serde_json::Value::Object(
                    clip.inputs
                        .iter()
                        .map(|(key, value)| (key.clone(), wire_value(value)))
                        .collect(),
                ),
                core: Some(clip.clone()),
            })
        })
        .collect::<Result<Vec<_>, String>>()
        .map(Into::into)
}

impl Editor {
    pub(crate) fn score_id(&self) -> Option<&str> {
        self.score.as_ref().map(|score| score.id.as_str())
    }
    pub(super) fn install_contents(
        &mut self,
        contents: crate::library::ScoreContents,
    ) -> Result<(), String> {
        match contents {
            crate::library::ScoreContents::Graph { document, beats } => {
                self.clips = resolve_document(&document, beats.as_ref())?;
                self.beats = beats.map(Rc::new);
                self.graph_score = Some(GraphState::new(document));
                self.base = Vec::new().into();
            }
            crate::library::ScoreContents::Legacy(clips) => {
                let clips: Vec<TrackClip> = clips.iter().map(TrackClip::from).collect();
                self.clips = resolve(&clips, &self.patterns);
                self.base = clips.into();
                self.graph_score = None;
            }
        }
        self.sheet.invalidate_defs();
        self.previews.borrow_mut().clear();
        self.preview_errors.clear();
        self.composited = Some(self.clips.clone());
        Ok(())
    }

    pub(crate) fn graph_candidate(&self) -> Result<p::Score, String> {
        let graph = self
            .graph_score
            .as_ref()
            .ok_or("this score uses legacy patterns")?;
        let mut score = p::Score::default();
        score.definitions = (*graph.definitions).clone();
        if self.clips.is_empty() {
            return Ok(score);
        }
        let clock = self
            .beats
            .as_ref()
            .ok_or("Waiting for the track's beat grid")?
            .timeline()
            .map_err(|error| error.to_string())?;
        let library = score
            .library(&p::standard_library())
            .map_err(|error| error.to_string())?;
        for clip in self.clips.iter() {
            let mut authored = clip.core.clone().ok_or("legacy clip in a graph score")?;
            // Preserve the exact authored beat when a gesture changed only
            // color/selection. Beat-to-second round trips need not be bit exact.
            if clock
                .seconds_at(authored.start)
                .map_err(|error| error.to_string())?
                != clip.start
            {
                authored.start = clock
                    .beat_at(clip.start)
                    .map_err(|error| error.to_string())?;
            }
            let old_end = clip.core.as_ref().unwrap().start + clip.core.as_ref().unwrap().duration;
            let end = if clock
                .seconds_at(old_end)
                .map_err(|error| error.to_string())?
                == clip.end
            {
                old_end
            } else {
                clock.beat_at(clip.end).map_err(|error| error.to_string())?
            };
            authored.duration = end - authored.start;
            authored.graph = clip.pattern.to_string();
            authored.z_index = clip.z;
            authored.blend_mode = clip.blend;
            let definition = library
                .definitions
                .get(&authored.graph)
                .ok_or("clip graph is missing")?;
            authored.inputs = clip
                .args
                .as_object()
                .ok_or("clip inputs must be an object")?
                .iter()
                .map(|(key, value)| {
                    let input = definition
                        .inputs
                        .get(key)
                        .ok_or_else(|| format!("unknown input {key}"))?;
                    Ok((
                        key.clone(),
                        luma_lib::node_graph::lighting::decode(input.value_type, value)?,
                    ))
                })
                .collect::<Result<_, String>>()?;
            score.clips.insert(clip.id.to_string(), authored);
        }
        score
            .validate(&p::standard_library())
            .map_err(|error| error.to_string())?;
        Ok(score)
    }

    pub(crate) fn graph_label(&self, id: &str) -> Option<String> {
        Some(self.graph_score.as_ref()?.library().ok()?.display_name(id))
    }

    pub(super) fn graph_input_defs(&self, id: &str) -> Option<Vec<PatternArgDef>> {
        let library = self.graph_score.as_ref()?.library().ok()?;
        let definition = library.definitions.get(id)?;
        let mut inputs = vec![PatternArgDef {
            id: SELECTION_INPUT.into(),
            name: "Selection".into(),
            arg_type: PatternArgType::Selection,
            default_value: p::Selection::all().to_value(),
        }];
        for (id, input) in &definition.inputs {
            let arg_type = match input.value_type {
                p::ValueType::Number => PatternArgType::Scalar,
                p::ValueType::Beats => PatternArgType::Beats,
                p::ValueType::Proportion => PatternArgType::Proportion,
                p::ValueType::Position => PatternArgType::Position,
                p::ValueType::Color => PatternArgType::Color,
                p::ValueType::Gradient => PatternArgType::Gradient,
                p::ValueType::AudioSource => PatternArgType::AudioSource,
                p::ValueType::Drum => PatternArgType::Drum,
                p::ValueType::Mapping => PatternArgType::Mapping,
                p::ValueType::Boundary => PatternArgType::Boundary,
                p::ValueType::Envelope => PatternArgType::Envelope,
                p::ValueType::Boolean => PatternArgType::Boolean,
                _ => continue,
            };
            let name = match input.value_type {
                p::ValueType::Beats => format!("{} (beats)", input.name),
                p::ValueType::Proportion => format!("{} (0–1)", input.name),
                _ => input.name.clone(),
            };
            inputs.push(PatternArgDef {
                id: id.clone(),
                name,
                arg_type,
                default_value: input
                    .default
                    .as_ref()
                    .map(wire_value)
                    .unwrap_or(serde_json::Value::Null),
            });
        }
        Some(inputs)
    }
}

impl Editor {
    /// Install a locally edited document without replacing the saved CAS base.
    pub(super) fn edit_graph_score(&mut self, score: p::Score) -> Result<(), String> {
        let document = GraphScoreDocument {
            revision: String::new(),
            score,
        };
        let clips = resolve_document(&document, self.beats.as_deref())?;
        let state = self
            .graph_score
            .as_mut()
            .ok_or("this score uses legacy patterns")?;
        state.definitions = Rc::new(document.score.definitions);
        self.sheet.invalidate_defs();
        self.replace_clips(clips.to_vec());
        Ok(())
    }
}

impl Luma {
    pub(super) fn make_clips_independent(&mut self, cx: &mut Context<Self>) {
        self.track_command(
            |editor| {
                let result = (|| {
                    let mut score = editor.graph_candidate()?;
                    for id in &editor.selected {
                        score
                            .make_independent(
                                &p::standard_library(),
                                id,
                                &uuid::Uuid::new_v4().to_string(),
                            )
                            .map_err(|error| error.to_string())?;
                    }
                    editor.edit_graph_score(score)
                })();
                if let Err(error) = result {
                    editor.error = Some(error);
                }
            },
            cx,
        );
    }

    pub(crate) fn commit_graph_score_for(&mut self, target: Target, cx: &mut Context<Self>) {
        self.sync_score_graph_tabs(&target, cx);
        let Some(Body::TrackEditor(editor)) = self.workspace.body_mut(&target) else {
            return;
        };
        if editor.saving || !editor.dirty || !editor.writable() {
            return;
        }
        let Some(score_id) = editor.score.as_ref().map(|score| score.id.clone()) else {
            return;
        };
        let candidate = match editor.graph_candidate() {
            Ok(score) => score,
            Err(error) => {
                editor.error = Some(error);
                editor.dirty = false;
                cx.notify();
                return;
            }
        };
        let graph = editor.graph_score.as_ref().unwrap();
        editor.dirty = false;
        if candidate == graph.base.score {
            return;
        }
        let pending = self.library.apply_score_document(
            &score_id,
            &candidate,
            &graph.base.revision,
            &uuid::Uuid::new_v4().to_string(),
        );
        editor.saving = true;
        editor.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let mut again = false;
                let mut reload = false;
                let mut previews = Vec::new();
                this.edit_track_tab(&target, cx, |editor| {
                    if editor.score.as_ref().map(|score| &score.id) != Some(&score_id) { return; }
                    editor.saving = false;
                    let Some(graph) = editor.graph_score.as_mut() else { return; };
                    match result {
                        Ok(saved) => {
                            let luma_lib::models::authored_state::AuthoredProjectedDocument::TrackScore { revision } = saved.document else {
                                editor.error = Some("The saved document was not a score".into()); return;
                            };
                            let definitions_changed = graph.base.score.definitions != candidate.definitions;
                            previews = candidate.clips.iter().filter(|(id, clip)| {
                                definitions_changed || graph.base.score.clips.get(*id) != Some(*clip)
                            }).map(|(id, _)| SharedString::from(id.clone())).collect();
                            editor.previews.borrow_mut().retain(|id, _| candidate.clips.contains_key(id.as_ref()));
                            graph.base = GraphScoreDocument { revision, score: candidate };
                        }
                        Err(error) => {
                            reload = matches!(error.command(), Some(CommandError::Conflict { .. }));
                            editor.error = Some(if reload { WRITE_CONFLICT.into() } else { error.to_string() });
                        }
                    }
                    if reload { editor.dirty = false; }
                    again = editor.dirty;
                });
                for id in previews { this.refresh_clip_preview_for(target.clone(), id, cx); }
                if reload { this.reload_score_contents(target, score_id, cx); }
                else if again { this.commit_graph_score_for(target, cx); }
            }).ok();
        }).detach();
    }

    pub(super) fn reload_score_contents(
        &mut self,
        target: Target,
        score_id: String,
        cx: &mut Context<Self>,
    ) {
        let Target::TrackEditor { track, .. } = &target else {
            return;
        };
        let pending = self.library.score_contents(&score_id, track);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let mut previews = Vec::new();
                this.edit_track_tab(&target, cx, |editor| {
                    if editor.score.as_ref().map(|score| &score.id) != Some(&score_id) {
                        return;
                    }
                    match result {
                        Ok(contents) => {
                            if let Err(error) = editor.install_contents(contents) {
                                editor.error = Some(error);
                                return;
                            }
                            editor.history = History::default();
                            editor.selected.clear();
                            editor.clipboard = None;
                            editor.previews.borrow_mut().clear();
                            previews = editor.clips.iter().map(|clip| clip.id.clone()).collect();
                        }
                        Err(error) => editor.error = Some(error.to_string()),
                    }
                });
                for id in previews {
                    this.refresh_clip_preview_for(target.clone(), id, cx);
                }
            })
            .ok();
        })
        .detach();
    }
}

impl Editor {
    pub(super) fn insert_graph(
        &mut self,
        menu: InsertMenu,
        choice: &InsertChoice,
    ) -> Result<(), String> {
        let clock = self
            .beats
            .as_ref()
            .ok_or("Waiting for the track's beat grid")?
            .timeline()
            .map_err(|e| e.to_string())?;
        let start = clock.beat_at(menu.start).map_err(|e| e.to_string())?;
        let duration = clock.beat_at(menu.end).map_err(|e| e.to_string())? - start;
        let mut score = self.graph_candidate()?;
        let id = uuid::Uuid::new_v4().to_string();
        let z = row_to_z(&z_ladder(&self.clips), menu.row as i32 - 1);
        if menu.insert {
            for clip in score.clips.values_mut() {
                if clip.z_index >= z {
                    clip.z_index += 1;
                }
            }
        }
        match choice {
            InsertChoice::Node { effect, .. } => score
                .insert_effect(&p::standard_library(), effect, &id, start, duration)
                .map_err(|e| e.to_string())?,
            InsertChoice::Graph { id: graph, .. } => {
                score.clips.insert(
                    id.clone(),
                    p::Clip {
                        graph: graph.clone(),
                        start,
                        duration,
                        seed: 0,
                        selection: p::Selection::all(),
                        z_index: z,
                        blend_mode: p::BlendMode::Replace,
                        inputs: BTreeMap::new(),
                    },
                );
            }
            InsertChoice::Pattern(_) => return Err("Use a node or a graph from this score".into()),
        }
        let clip = score.clips.get_mut(&id).unwrap();
        clip.z_index = z;
        clip.seed = uuid::Uuid::new_v4().as_u64_pair().0;
        self.edit_graph_score(score)?;
        self.menu = None;
        self.selected = vec![id.into()];
        self.cursor = Some(Cursor {
            row: menu.row.max(1),
            row_end: None,
            start: menu.start,
            end: Some(menu.end),
        });
        Ok(())
    }
}

impl Editor {
    pub(crate) fn publish_graph_edit(&mut self, score: p::Score) -> Result<(), String> {
        if !self.writable() {
            return Err("This score is read only".into());
        }
        if self.graph_candidate()? == score {
            return Ok(());
        }
        self.checkpoint();
        if let Err(error) = self.edit_graph_score(score) {
            self.abandon_checkpoint();
            return Err(error);
        }
        Ok(())
    }
    pub(crate) fn step_graph_history(&mut self, forward: bool) -> bool {
        if !self.writable() {
            return false;
        }
        if forward {
            self.redo()
        } else {
            self.undo()
        }
    }
}

impl Luma {
    pub(crate) fn refresh_working_scene_for(&mut self, target: &Target, cx: &mut Context<Self>) {
        self.sync_score_graph_tabs(target, cx);
        if let Some(Body::TrackEditor(editor)) = self.workspace.body_mut(target) {
            super::sync_composite(editor, cx);
        }
    }
}
