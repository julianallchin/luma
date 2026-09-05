use crate::{Binding, Body, Definition, Error, Frame, Graph, Library, Node, Result, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A playable composition. Definition IDs refer to graph revisions. A pattern
/// lives either in its score document or in the reusable library, not a venue.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pattern {
    pub name: String,
    pub definition: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub pattern: String,
    pub start: f64,
    pub duration: f64,
    pub seed: u64,
    #[serde(default)]
    pub inputs: BTreeMap<String, Value>,
}
/// New score document format. Local definitions travel with the score. This is
/// deliberately not written into old SQL projections before migration exists.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Score {
    version: u32,
    pub definitions: BTreeMap<String, Definition>,
    pub patterns: BTreeMap<String, Pattern>,
    pub clips: BTreeMap<String, Clip>,
}
impl Default for Score {
    fn default() -> Self {
        Self {
            version: 1,
            definitions: BTreeMap::new(),
            patterns: BTreeMap::new(),
            clips: BTreeMap::new(),
        }
    }
}
impl Score {
    pub fn validate(&self, base: &Library) -> Result<()> {
        if self.version != 1 {
            return Err(Error(format!("unsupported score version {}", self.version)));
        }
        let library = self.library(base)?;
        for (id, definition) in &self.definitions {
            library.validate(id)?;
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
        for pattern in self.patterns.values() {
            let definition = library
                .definitions
                .get(&pattern.definition)
                .ok_or_else(|| Error(format!("unknown pattern graph {}", pattern.definition)))?;
            if !definition.playable() {
                return Err(Error("a pattern must produce Lighting".into()));
            }
        }
        for clip in self.clips.values() {
            if !clip.start.is_finite()
                || !clip.duration.is_finite()
                || clip.duration <= 0.0
                || !(clip.start + clip.duration).is_finite()
            {
                return Err(Error(
                    "clip needs finite start and positive duration".into(),
                ));
            }
            let pattern = self
                .patterns
                .get(&clip.pattern)
                .ok_or_else(|| Error(format!("unknown pattern {}", clip.pattern)))?;
            let definition = &library.definitions[&pattern.definition];
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
        if !start.is_finite() || !duration.is_finite() || duration <= 0.0 {
            return Err(Error(
                "clip needs finite start and positive duration".into(),
            ));
        }
        if self.patterns.contains_key(id)
            || self.definitions.contains_key(id)
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
        let node = Node {
            definition: effect.into(),
            inputs: definition
                .inputs
                .keys()
                .map(|key| (key.clone(), Binding::Input { input: key.clone() }))
                .collect(),
        };
        let wrapper = Definition {
            name: definition.name.clone(),
            inputs: definition.inputs.clone(),
            outputs: definition.outputs.clone(),
            body: Body::Graph(Graph {
                nodes: BTreeMap::from([("effect".into(), node)]),
                outputs: definition
                    .outputs
                    .keys()
                    .map(|name| {
                        (
                            name.clone(),
                            Binding::Connection {
                                node: "effect".into(),
                                output: name.clone(),
                            },
                        )
                    })
                    .collect(),
            }),
        };
        self.definitions.insert(id.into(), wrapper);
        self.patterns.insert(
            id.into(),
            Pattern {
                name: definition.name.clone(),
                definition: id.into(),
            },
        );
        self.clips.insert(
            id.into(),
            Clip {
                pattern: id.into(),
                start,
                duration,
                seed: 0,
                inputs: BTreeMap::new(),
            },
        );
        Ok(())
    }
    pub fn library(&self, base: &Library) -> Result<Library> {
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
    /// Host inputs resolve venue-relative values (for example Mapping). Clip
    /// overrides affect only their occurrence. They never mutate the definition.
    pub fn evaluate_clip(
        &self,
        base: &Library,
        clip_id: &str,
        host_inputs: &BTreeMap<String, Value>,
        beat: f64,
        cells: &[crate::Cell],
    ) -> Result<BTreeMap<String, Value>> {
        let clip = self
            .clips
            .get(clip_id)
            .ok_or_else(|| Error(format!("unknown clip {clip_id}")))?;
        if !beat.is_finite()
            || !clip.start.is_finite()
            || !clip.duration.is_finite()
            || clip.duration <= 0.0
        {
            return Err(Error("invalid clip time".into()));
        }
        let pattern = self
            .patterns
            .get(&clip.pattern)
            .ok_or_else(|| Error(format!("unknown pattern {}", clip.pattern)))?;
        let library = self.library(base)?;
        let mut inputs = host_inputs.clone();
        inputs.extend(clip.inputs.clone());
        if beat < clip.start || beat >= clip.start + clip.duration {
            return Ok(BTreeMap::new());
        }
        library.evaluate(
            &pattern.definition,
            &inputs,
            Frame {
                cells,
                beat,
                clip_start: clip.start,
                seed: clip.seed,
            },
        )
    }
}

fn authored_value(value: &Value) -> Result<()> {
    value.validate()?;
    if matches!(
        value,
        Value::Coordinates(_) | Value::Mask(_) | Value::Lighting(_)
    ) {
        return Err(Error(
            "resolved cell values belong to execution, not a saved score".into(),
        ));
    }
    Ok(())
}
