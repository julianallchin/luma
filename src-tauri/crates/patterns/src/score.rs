use crate::{Binding, Body, Definition, Error, Frame, Library, PreparedGraph, Result, Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    /// A score-local graph or an immutable built-in node definition.
    pub graph: String,
    pub start: f64,
    pub duration: f64,
    pub seed: u64,
    /// Group expression resolved by the host; never physical fixture ids.
    #[serde(default = "all_selection")]
    pub selection: crate::Selection,
    #[serde(default)]
    pub z_index: i64,
    #[serde(default = "replace_blend")]
    pub blend_mode: crate::BlendMode,
    #[serde(default)]
    pub inputs: BTreeMap<String, Value>,
}
fn all_selection() -> crate::Selection {
    crate::Selection::all()
}
fn replace_blend() -> crate::BlendMode {
    crate::BlendMode::Replace
}

/// New score document format. Local definitions travel with the score. This is
/// deliberately not written into old SQL projections before migration exists.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Score {
    version: u32,
    pub definitions: BTreeMap<String, Definition>,
    pub clips: BTreeMap<String, Clip>,
}
impl Default for Score {
    fn default() -> Self {
        Self {
            version: 2,
            definitions: BTreeMap::new(),
            clips: BTreeMap::new(),
        }
    }
}
impl Score {
    pub fn validate(&self, base: &Library) -> Result<()> {
        if self.version != 2 {
            return Err(Error(format!("unsupported score version {}", self.version)));
        }
        let library = self.library(base)?;
        for (id, clip) in &self.clips {
            if !library.definitions.contains_key(&clip.graph) {
                return Err(Error(format!(
                    "clip {id}: unknown clip graph {}",
                    clip.graph
                )));
            }
        }
        library.validate_many(
            self.definitions
                .keys()
                .map(String::as_str)
                .chain(self.clips.values().map(|clip| clip.graph.as_str())),
        )?;
        for definition in self.definitions.values() {
            if matches!(definition.body, Body::Primitive(_)) {
                return Err(Error("score-local definitions must be graphs".into()));
            }
            for input in definition.inputs.values() {
                if let Some(value) = &input.default {
                    authored_value(value)?;
                }
            }
            if let Body::Graph(graph) = &definition.body {
                for binding in graph
                    .nodes
                    .values()
                    .flat_map(|n| n.inputs.values())
                    .chain(graph.outputs.values())
                {
                    if let Binding::Value { value } = binding {
                        authored_value(value)?;
                    }
                }
            }
        }
        for (id, clip) in &self.clips {
            crate::graph::identity(id)?;
            clip.selection.validate()?;
            if !clip.start.is_finite()
                || !clip.duration.is_finite()
                || clip.duration <= 0.0
                || !(clip.start + clip.duration).is_finite()
            {
                return Err(Error(
                    "clip needs finite start and positive duration".into(),
                ));
            }
            let definition = library
                .definitions
                .get(&clip.graph)
                .ok_or_else(|| Error(format!("unknown clip graph {}", clip.graph)))?;
            if !definition.playable() {
                return Err(Error("a clip graph must produce fixture output".into()));
            }
            for (key, input) in &definition.inputs {
                if input.default.is_none() && !clip.inputs.contains_key(key) {
                    return Err(Error(format!("missing clip input {key}")));
                }
            }
            for (key, value) in &clip.inputs {
                authored_value(value)?;
                let input = definition
                    .inputs
                    .get(key)
                    .ok_or_else(|| Error(format!("unknown clip input {key}")))?;
                if input.value_type != value.value_type() {
                    return Err(Error(format!("clip input {key} has the wrong type")));
                }
            }
            // Check fixed timing/value relationships without binding venue geometry.
            // Actual domain requirements (e.g. a solved circle) are host checks.
            PreparedGraph::new_validated(
                &library,
                &clip.graph,
                &clip.inputs,
                Frame {
                    features: None,
                    cells: &[],
                    beat: clip.start,
                    clip_start: clip.start,
                    clip_duration: clip.duration,
                    seed: clip.seed,
                },
            )
            .map_err(|error| Error(format!("clip {id}: {error}")))?;
        }
        Ok(())
    }
    pub fn to_json(&self, base: &Library) -> Result<String> {
        self.validate(base)?;
        serde_json::to_string_pretty(self).map_err(|error| Error(error.to_string()))
    }
    pub fn from_json(base: &Library, json: &str) -> Result<Self> {
        let score: Self = serde_json::from_str(json).map_err(|error| Error(error.to_string()))?;
        score.validate(base)?;
        Ok(score)
    }
    /// The score picker inserts an ordinary one-node graph and exposes the
    /// selected node's interface. IDs are caller-owned for deterministic edits.
    pub fn insert_effect(
        &mut self,
        library: &Library,
        effect: &str,
        id: &str,
        start: f64,
        duration: f64,
    ) -> Result<()> {
        if !start.is_finite()
            || !duration.is_finite()
            || duration <= 0.0
            || !(start + duration).is_finite()
        {
            return Err(Error(
                "clip needs finite start and positive duration".into(),
            ));
        }
        crate::graph::identity(id)?;
        if self.definitions.contains_key(id)
            || self.clips.contains_key(id)
            || library.definitions.contains_key(id)
        {
            return Err(Error(format!("identity {id} already exists")));
        }
        library.validate(effect)?;
        let definition = &library.definitions[effect];
        if !definition.playable() {
            return Err(Error(
                "only Lighting outputs can be placed on the score".into(),
            ));
        }
        let wrapper = definition.instance(effect);
        self.definitions.insert(id.into(), wrapper);
        self.clips.insert(
            id.into(),
            Clip {
                graph: id.into(),
                start,
                duration,
                seed: 0,
                selection: all_selection(),
                z_index: 0,
                blend_mode: replace_blend(),
                inputs: BTreeMap::new(),
            },
        );
        Ok(())
    }
    /// Detach one clip, including every reachable score-local subgraph. Built-in
    /// definitions remain shared and immutable. Validate before replacing self.
    pub fn make_independent(&mut self, base: &Library, clip_id: &str, new_id: &str) -> Result<()> {
        self.validate(base)?;
        let clip = self
            .clips
            .get(clip_id)
            .ok_or_else(|| Error(format!("unknown clip {clip_id}")))?;
        let library = self.library(base)?;
        let mut reachable = BTreeSet::new();
        let mut pending = vec![clip.graph.clone()];
        while let Some(id) = pending.pop() {
            if !reachable.insert(id.clone()) {
                continue;
            }
            if let Body::Graph(graph) = &library.definitions[&id].body {
                pending.extend(
                    graph
                        .nodes
                        .values()
                        .filter(|n| self.definitions.contains_key(&n.definition))
                        .map(|n| n.definition.clone()),
                );
            }
        }
        let mut remap = BTreeMap::from([(clip.graph.clone(), new_id.to_owned())]);
        for (index, id) in reachable.iter().filter(|id| **id != clip.graph).enumerate() {
            remap.insert(id.clone(), format!("{new_id}/{index}"));
        }
        for id in remap.values() {
            if id.is_empty() || library.definitions.contains_key(id) {
                return Err(Error(format!(
                    "graph identity {id} already exists or is empty"
                )));
            }
        }
        let mut candidate = self.clone();
        for (old, new) in &remap {
            let original = &library.definitions[old];
            let mut definition = if matches!(original.body, Body::Primitive(_)) {
                original.instance(old)
            } else {
                original.clone()
            };
            if let Body::Graph(graph) = &mut definition.body {
                for node in graph.nodes.values_mut() {
                    if self.definitions.contains_key(&node.definition) {
                        if let Some(id) = remap.get(&node.definition) {
                            node.definition = id.clone();
                        }
                    }
                }
            }
            candidate.definitions.insert(new.clone(), definition);
        }
        candidate.clips.get_mut(clip_id).unwrap().graph = new_id.into();
        candidate.validate(base)?;
        *self = candidate;
        Ok(())
    }

    pub fn library(&self, base: &Library) -> Result<Library> {
        if self.definitions.len() > 512 || self.clips.len() > 2048 {
            return Err(Error(
                "a score supports at most 512 local definitions and 2,048 clips".into(),
            ));
        }
        let mut library = base.clone();
        for (id, definition) in &self.definitions {
            if library
                .definitions
                .insert(id.clone(), definition.clone())
                .is_some()
            {
                return Err(Error(format!(
                    "local definition shadows library revision {id}"
                )));
            }
        }
        Ok(library)
    }
    /// Bind clip overrides once. The resulting program owns its inputs and
    /// geometry and can render any frame, including a backwards seek.
    pub fn prepare_clip(
        &self,
        base: &Library,
        clip_id: &str,
        host_inputs: &BTreeMap<String, Value>,
        cells: &[crate::Cell],
    ) -> Result<PreparedClip> {
        self.validate(base)?;
        let clip = self
            .clips
            .get(clip_id)
            .ok_or_else(|| Error(format!("unknown clip {clip_id}")))?;
        let library = self.library(base)?;
        let mut inputs = host_inputs.clone();
        inputs.extend(clip.inputs.clone());
        let program = PreparedGraph::new(
            &library,
            &clip.graph,
            &inputs,
            Frame {
                features: None,
                cells,
                beat: clip.start,
                clip_start: clip.start,
                clip_duration: clip.duration,
                seed: clip.seed,
            },
        )?;
        Ok(PreparedClip {
            program,
            start: clip.start,
            end: clip.start + clip.duration,
        })
    }
    pub fn evaluate_clip(
        &self,
        base: &Library,
        clip_id: &str,
        host_inputs: &BTreeMap<String, Value>,
        beat: f64,
        cells: &[crate::Cell],
    ) -> Result<BTreeMap<String, Value>> {
        self.prepare_clip(base, clip_id, host_inputs, cells)?
            .evaluate(beat)
    }
}

fn authored_value(value: &Value) -> Result<()> {
    value.validate()?;
    if matches!(
        value,
        Value::Coordinates(_)
            | Value::ColorField(_)
            | Value::Field(_)
            | Value::Mask(_)
            | Value::Lighting(_)
    ) {
        return Err(Error(
            "resolved cell values belong to execution, not a saved score".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct PreparedClip {
    program: PreparedGraph,
    start: f64,
    end: f64,
}
impl PreparedClip {
    pub fn evaluate(&self, beat: f64) -> Result<BTreeMap<String, Value>> {
        if !beat.is_finite() {
            return Err(Error("musical time must be finite".into()));
        }
        if beat < self.start || beat >= self.end {
            return Ok(BTreeMap::new());
        }
        self.program.evaluate(beat)
    }
}
