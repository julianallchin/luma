//! The canonical score behind the native timeline. Seconds are a view of its
//! beat positions.
use super::*;
use luma_lib::models::node_graph::{PatternArgDef, PatternArgType};
use luma_patterns as p;
use std::collections::BTreeMap;

pub(super) const SELECTION_INPUT: &str = "@clip/selection";

/// Is `stored` the document `ours` wrote, as far as storage can tell?
///
/// Storage does not give floats back bit for bit: a beat comes back with 15
/// significant digits, an envelope handle one ulp off. Exact equality would
/// call every stored copy changed, and each sync event would reinstall the
/// score — rebuilding every preview and dropping the undo history.
fn same_document(ours: &p::Score, stored: &p::Score) -> bool {
    ours == stored
        || match (serde_json::to_value(ours), serde_json::to_value(stored)) {
            (Ok(a), Ok(b)) => close(&a, &b),
            _ => false,
        }
}

/// Equal JSON, up to the float noise storage adds.
pub(super) fn close(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) if a.is_f64() || b.is_f64() => {
                (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.)
            }
            _ => a == b,
        },
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| close(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| close(a, b)))
        }
        _ => a == b,
    }
}

/// A shipped form's definition. `standard_library()` hands back a copy of all
/// of it, so the forms are read from it once.
pub(super) fn form_definition(id: &str) -> Option<&'static p::Definition> {
    static FORMS: std::sync::OnceLock<BTreeMap<&'static str, p::Definition>> =
        std::sync::OnceLock::new();
    FORMS
        .get_or_init(|| {
            let library = p::standard_library();
            p::FORMS
                .iter()
                .map(|form| (*form, library.definitions[*form].clone()))
                .collect()
        })
        .get(id)
}

pub(super) fn wire_value(value: &p::Value) -> serde_json::Value {
    luma_lib::node_graph::lighting::wire_value(value)
}

pub(super) fn resolve_document(
    score: &p::Score,
    beats: Option<&BeatGrid>,
) -> Result<Rc<[Clip]>, String> {
    if score.clips.is_empty() {
        return Ok(Vec::new().into());
    }
    let clock = beats
        .ok_or("Waiting for the track's beat grid")?
        .timeline()
        .map_err(|error| error.to_string())?;
    let library = p::standard_library();
    let mut clips = score
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
                row: 0,
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
        .collect::<Result<Vec<_>, String>>()?;
    assign_rows(&mut clips);
    Ok(clips.into())
}

impl Editor {
    pub(crate) fn score_id(&self) -> Option<&str> {
        self.score.as_ref().map(|score| score.id.as_str())
    }
    pub(super) fn install_contents(
        &mut self,
        contents: crate::library::ScoreContents,
    ) -> Result<(), String> {
        let crate::library::ScoreContents { score, beats } = contents;
        self.clips = resolve_document(&score, beats.as_ref())?;
        self.beats = beats.map(Rc::new);
        self.graph_score = Some(score);
        self.sheet.invalidate_defs();
        self.previews.borrow_mut().clear();
        self.preview_errors.clear();
        self.composited = Some(self.clips.clone());
        Ok(())
    }

    pub(crate) fn graph_candidate(&self) -> Result<p::Score, String> {
        self.graph_score.as_ref().ok_or("no score is open")?;
        let mut score = p::Score::default();
        if self.clips.is_empty() {
            return Ok(score);
        }
        let clock = self
            .beats
            .as_ref()
            .ok_or("Waiting for the track's beat grid")?
            .timeline()
            .map_err(|error| error.to_string())?;
        for clip in self.clips.iter() {
            let mut authored = clip.core.clone().ok_or("clip has no authored body")?;
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
            let definition = form_definition(&authored.graph).ok_or("clip graph is not a form")?;
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
        // Not checked as a whole: one stale clip must not stop the rest from
        // playing, previewing or saving. The backend checks each clip it
        // saves, and a scene leaves out a clip that fails.
        Ok(score)
    }

    pub(super) fn graph_input_defs(&self, id: &str) -> Option<Vec<PatternArgDef>> {
        let definition = form_definition(id)?;
        let mut inputs = vec![PatternArgDef {
            id: SELECTION_INPUT.into(),
            name: "Selection".into(),
            arg_type: PatternArgType::Selection,
            default_value: p::Selection::all().to_value(),
        }];
        // A form lists its inputs in its own order, under the engine's names.
        let order = p::input_order(id);
        let mut entries: Vec<_> = definition.inputs.iter().collect();
        if let Some(order) = order {
            entries.sort_by_key(|(key, _)| order.iter().position(|at| at == key));
        }
        for (id, input) in entries {
            let Some(arg_type) = luma_lib::node_graph::lighting::arg_type(input.value_type) else {
                continue;
            };
            let name = if order.is_some() {
                input.name.clone()
            } else {
                luma_lib::node_graph::lighting::input_label(input)
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
    /// Install a locally edited document without replacing the saved base.
    pub(super) fn edit_graph_score(&mut self, score: p::Score) -> Result<(), String> {
        let clips = resolve_document(&score, self.beats.as_deref())?;
        self.graph_score.as_ref().ok_or("no score is open")?;
        self.replace_clips(clips.to_vec());
        Ok(())
    }
}

impl Luma {
    pub(crate) fn commit_graph_score_for(&mut self, target: Target, cx: &mut Context<Self>) {
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
        editor.dirty = false;
        if Some(&candidate) == editor.graph_score.as_ref() {
            return;
        }
        let pending = self.library.apply_score_document(&score_id, &candidate);
        editor.saving = true;
        editor.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let mut again = false;
                let mut previews = Vec::new();
                this.edit_track_tab(&target, cx, |editor| {
                    if editor.score.as_ref().map(|score| &score.id) != Some(&score_id) {
                        return;
                    }
                    editor.saving = false;
                    editor.writes += 1;
                    let Some(base) = editor.graph_score.as_mut() else {
                        return;
                    };
                    match result {
                        Ok(()) => {
                            previews = candidate
                                .clips
                                .iter()
                                .filter(|(id, clip)| base.clips.get(*id) != Some(*clip))
                                .map(|(id, _)| SharedString::from(id.clone()))
                                .collect();
                            editor
                                .previews
                                .borrow_mut()
                                .retain(|id, _| candidate.clips.contains_key(id.as_ref()));
                            *base = candidate;
                        }
                        Err(error) => editor.error = Some(error.to_string()),
                    }
                    again = editor.dirty;
                });
                for id in previews {
                    this.refresh_clip_preview_for(target.clone(), id, cx);
                }
                if again {
                    this.commit_graph_score_for(target, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn reload_score_contents(
        &mut self,
        target: Target,
        score_id: String,
        cx: &mut Context<Self>,
    ) {
        let Target::TrackEditor { track, .. } = &target else {
            return;
        };
        let Some(Body::TrackEditor(editor)) = self.workspace.body(&target) else {
            return;
        };
        let writes = editor.writes;
        let pending = self.library.score_contents(&score_id, track);
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let mut previews = Vec::new();
                let mut changed = false;
                this.edit_track_tab(&target, cx, |editor| {
                    if editor.score.as_ref().map(|score| &score.id) != Some(&score_id) {
                        return;
                    }
                    // The sync layer announces every write to a synced table,
                    // our own included. A read that overlapped a local edit or
                    // save is that save, or older than it: installing it would
                    // drop the selection and can put back the old value.
                    if editor.dirty || editor.saving || editor.writes != writes {
                        return;
                    }
                    match result {
                        Ok(contents) => {
                            // Agent commits invalidate several tabs. An unchanged
                            // document must not discard this song's local edits,
                            // selection, previews, or undo history.
                            if editor
                                .graph_score
                                .as_ref()
                                .is_some_and(|base| same_document(base, &contents.score))
                            {
                                return;
                            }
                            // Installing a read initializes its scene baseline. Keep
                            // the actual installed baseline so the rig recompiles
                            // changed clips.
                            let composited = editor.composited.clone();
                            if let Err(error) = editor.install_contents(contents) {
                                editor.error = Some(error);
                                return;
                            }
                            changed = true;
                            editor.composited = composited;
                            editor.history = History::default();
                            // A clip that is still there stays selected: the
                            // sheet is the selection, and a remote edit to
                            // another clip must not close it.
                            let clips = &editor.clips;
                            editor
                                .selected
                                .retain(|id| clips.iter().any(|clip| &clip.id == id));
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
                if changed {
                    this.refresh_working_scene_for(&target, cx);
                }
            })
            .ok();
        })
        .detach();
    }
}

impl Editor {
    /// Place a preset as a new clip on `selection`, at the menu's span and
    /// lane.
    pub(super) fn insert_preset(
        &mut self,
        menu: InsertMenu,
        choice: InsertChoice,
        selection: p::Selection,
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
        score
            .clips
            .insert(id.clone(), choice.0.clip(start, duration));
        let clip = score.clips.get_mut(&id).unwrap();
        clip.z_index = z;
        clip.selection = selection;
        clip.seed = uuid::Uuid::new_v4().as_u64_pair().0;
        p::Score::validate_clip(&p::standard_library(), &id, clip)
            .map_err(|error| error.to_string())?;
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

impl Luma {
    pub(crate) fn refresh_working_scene_for(&mut self, target: &Target, cx: &mut Context<Self>) {
        if let Some(Body::TrackEditor(editor)) = self.workspace.body_mut(target) {
            super::sync_composite(editor, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{p, same_document};

    /// A clip as the editor wrote it, from a session that blinked.
    const CLIP: &str = r#"{"graph": "267688a9-e19d-4e62-aad8-a7d4e31e4097", "start": 73.0, "duration": 0.99988652, "seed": 1029648076447695423, "selection": {"expression": "led_bars_vertical"}, "z_index": 1, "blend_mode": "replace", "inputs": {"color": {"type": "color", "value": [0.38823529411764707, 0.38823529411764707, 0.38823529411764707]}, "mapping": {"type": "mapping", "value": {"source": {"kind": "z"}, "per_group": false, "reverse": true}}, "path": {"type": "envelope", "value": {"points": [[0.0, 0.0, [0.4920748472213745, 0.0070618391036987305, 0.4920748472213745, 0.9999237060546875]], [1.0, 1.0]]}}, "travel": {"type": "beats", "value": 0.75}}}"#;

    fn score(clip: serde_json::Value) -> p::Score {
        serde_json::from_value(serde_json::json!({
            "clips": { "c": clip }
        }))
        .unwrap()
    }

    #[test]
    fn storage_float_noise_is_the_same_document() {
        let ours: serde_json::Value = serde_json::from_str(CLIP).unwrap();
        let mut stored = ours.clone();
        // What storage handed back: one ulp on a handle, a last digit on a beat.
        stored["inputs"]["path"]["value"]["points"][0][2][3] =
            serde_json::json!(0.9999237060546876);
        stored["duration"] = serde_json::json!(ours["duration"].as_f64().unwrap() + 1e-15);
        assert!(same_document(&score(ours.clone()), &score(stored)));

        let mut moved = ours.clone();
        moved["start"] = serde_json::json!(73.5);
        assert!(!same_document(&score(ours), &score(moved)));
    }
}
