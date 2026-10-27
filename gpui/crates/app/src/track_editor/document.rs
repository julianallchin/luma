//! The canonical score behind the native timeline. Seconds are a view of its
//! beat positions.
use super::*;
use luma_patterns as p;

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

pub(super) fn resolve_document(
    score: &p::Score,
    beats: Option<&BeatGrid>,
) -> Result<Vec<Clip>, String> {
    if score.clips.is_empty() {
        return Ok(Vec::new());
    }
    let clock = beats
        .ok_or("Waiting for the track's beat grid")?
        .timeline()
        .map_err(|error| error.to_string())?;
    let mut clips = score
        .clips
        .iter()
        .map(|(id, clip)| {
            let mut resolved = Clip {
                id: id.clone().into(),
                output: SharedString::default(),
                label: SharedString::default(),
                summary: SharedString::default(),
                color: ladder::pattern(""),
                start: clock
                    .seconds_at(clip.start)
                    .map_err(|error| error.to_string())?,
                end: clock
                    .seconds_at(clip.start + clip.duration)
                    .map_err(|error| error.to_string())?,
                row: 0,
                z: clip.z_index,
                blend: clip.blend_mode,
                core: Some(clip.clone()),
            };
            resolved.refresh();
            Ok(resolved)
        })
        .collect::<Result<Vec<_>, String>>()?;
    assign_rows(&mut clips);
    Ok(clips)
}

/// A stored score laid out for the timeline. Pure, and for a score of
/// hundreds of clips not cheap, so it is made off the UI thread.
pub(super) struct Resolved {
    pub(super) contents: crate::library::ScoreContents,
    clips: Result<Vec<Clip>, String>,
    split: bool,
}

impl Resolved {
    pub(super) fn of(contents: crate::library::ScoreContents) -> Self {
        // One row is one track. A stored score whose clips overlap in one
        // z_index is shown split into tracks, and the split is written back
        // once. The saved base stays the stored score, so the write sees it.
        let mut tracks = contents.score.clone();
        let split = tracks.separate_tracks();
        let clips = resolve_document(&tracks, contents.beats.as_ref());
        Self {
            contents,
            clips,
            split,
        }
    }
}

impl Editor {
    pub(super) fn install_contents(&mut self, resolved: Resolved) -> Result<(), String> {
        let Resolved {
            contents: crate::library::ScoreContents { score, beats },
            clips,
            split,
        } = resolved;
        self.clips = clips?.into();
        self.beats = beats.map(Rc::new);
        self.graph_score = Some(score);
        self.dirty = split && self.writable();
        self.sheet.invalidate();
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
            let core = clip.core.as_ref().unwrap();
            let old_end = core.start + core.duration;
            // An untouched span keeps its exact duration: `end - start` need
            // not give it back, and a save sends every clip that differs.
            if clock
                .seconds_at(old_end)
                .map_err(|error| error.to_string())?
                != clip.end
            {
                authored.duration =
                    clock.beat_at(clip.end).map_err(|error| error.to_string())? - authored.start;
            } else if authored.start != core.start {
                authored.duration = old_end - authored.start;
            }
            authored.z_index = clip.z;
            authored.blend_mode = clip.blend;
            score.clips.insert(clip.id.to_string(), authored);
        }
        // Not checked as a whole: one stale clip must not stop the rest from
        // playing, previewing or saving. The backend checks each clip it
        // saves, and a scene leaves out a clip that fails.
        Ok(score)
    }
}

impl Editor {
    /// Install a locally edited document without replacing the saved base.
    pub(super) fn edit_graph_score(&mut self, score: p::Score) -> Result<(), String> {
        let clips = resolve_document(&score, self.beats.as_deref())?;
        self.graph_score.as_ref().ok_or("no score is open")?;
        self.replace_clips(clips);
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
        let score_id = editor.score.id.clone();
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
        let Some(base) = editor.graph_score.as_ref() else {
            return;
        };
        let edit = p::ScoreEdit::between(base, &candidate);
        if edit.is_empty() {
            return;
        }
        let pending = self.library.edit_score_document(&score_id, &edit);
        editor.saving = true;
        editor.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                let mut again = false;
                let mut previews = Vec::new();
                this.edit_track_tab(&target, cx, |editor| {
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
                this.refresh_clip_previews_for(target.clone(), previews, cx);
                if again {
                    this.commit_graph_score_for(target, cx);
                } else {
                    // A re-read this write held off.
                    this.refresh_agent_tabs(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Re-read a score tab's document after a change this tab did not make.
    pub(crate) fn reload_score_contents(&mut self, target: Target, cx: &mut Context<Self>) {
        let Target::Score { track, score, .. } = &target else {
            return;
        };
        let Some(Body::TrackEditor(editor)) = self.workspace.body(&target) else {
            return;
        };
        let writes = editor.writes;
        let pending = self.library.score_contents(score, track);
        cx.spawn(async move |this, cx| {
            let result = match pending.await {
                Ok(contents) => Ok(cx
                    .background_spawn(async move { Resolved::of(contents) })
                    .await),
                Err(error) => Err(error),
            };
            this.update(cx, |this, cx| {
                let mut previews = Vec::new();
                let mut changed = false;
                let mut split = false;
                let mut held = None;
                this.edit_track_tab(&target, cx, |editor| {
                    // A read that overlapped a local edit or save may be
                    // older than it: installing it would drop the selection
                    // and can put back the old value. Storage still moved, so
                    // read again once the editor's own write is in.
                    if editor.dirty || editor.saving || editor.writes != writes {
                        held = Some(editor.dirty || editor.saving);
                        return;
                    }
                    match result {
                        Ok(resolved) => {
                            // Agent commits invalidate several tabs. An unchanged
                            // document must not discard this song's local edits,
                            // selection, previews, or undo history.
                            if editor
                                .graph_score
                                .as_ref()
                                .is_some_and(|base| same_document(base, &resolved.contents.score))
                            {
                                return;
                            }
                            // Installing a read initializes its scene baseline. Keep
                            // the actual installed baseline so the rig recompiles
                            // changed clips.
                            let composited = editor.composited.clone();
                            if let Err(error) = editor.install_contents(resolved) {
                                editor.error = Some(error);
                                return;
                            }
                            changed = true;
                            split = editor.dirty;
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
                this.refresh_clip_previews_for(target.clone(), previews, cx);
                if changed {
                    this.refresh_working_scene_for(&target, cx);
                }
                match held {
                    // The write's return reads it.
                    Some(true) => {
                        if !this.agent_stale_tabs.contains(&target) {
                            this.agent_stale_tabs.push(target);
                        }
                    }
                    Some(false) => this.reload_score_contents(target, cx),
                    None if split => this.commit_graph_score_for(target, cx),
                    None => {}
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
        score.clips.insert(id.clone(), choice.clip(start, duration));
        let clip = score.clips.get_mut(&id).unwrap();
        clip.z_index = z;
        clip.selection = selection;
        clip.seed = uuid::Uuid::new_v4().as_u64_pair().0;
        p::Score::validate_clip(&p::standard_library(), &id, clip)
            .map_err(|error| error.to_string())?;
        self.edit_graph_score(score)?;
        let id = SharedString::from(id);
        if let Some(clips) = lanes::settle(&self.clips, std::slice::from_ref(&id), &[]) {
            self.replace_clips(clips);
        }
        self.menu = None;
        self.selected = vec![id];
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
    const CLIP: &str = r#"{"name": "Chase", "start": 73.0, "duration": 0.99988652, "seed": 1029648076447695423, "selection": {"expression": "led_bars_vertical"}, "z_index": 1, "blend_mode": "replace", "graph": {"version": 2, "nodes": {"time1": {"kind": "time"}, "curve1": {"kind": "curve", "settings": {"kind": "number"}, "inputs": {"x": {"node": "time1"}, "shape": {"points": [[0.0, 0.0, [0.4920748472213745, 0.0070618391036987305, 0.4920748472213745, 0.9999237060546875]], [1.0, 1.0]]}}}, "color1": {"kind": "color", "inputs": {"color": [0.38823529411764707, 0.38823529411764707, 0.38823529411764707], "brightness": {"node": "curve1"}}}}}}"#;

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
        stored["graph"]["nodes"]["curve1"]["inputs"]["shape"]["points"][0][2][3] =
            serde_json::json!(0.9999237060546876);
        stored["duration"] = serde_json::json!(ours["duration"].as_f64().unwrap() + 1e-15);
        assert!(same_document(&score(ours.clone()), &score(stored)));

        let mut moved = ours.clone();
        moved["start"] = serde_json::json!(73.5);
        assert!(!same_document(&score(ours), &score(moved)));
    }
}
