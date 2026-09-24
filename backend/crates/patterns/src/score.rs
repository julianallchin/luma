use crate::{Error, Frame, Library, PreparedGraph, Result, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    /// The form this clip plays, with its version, e.g. `color.chase@1`.
    pub graph: String,
    pub start: f64,
    pub duration: f64,
    pub seed: u64,
    /// Historical clips used independent random draws for resolving the
    /// selection (which side an `^` takes) and animating it. New clips use
    /// `seed` for both unless explicitly supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_seed: Option<u64>,
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

/// Canonical score document: clips keyed by their stable identity. Every clip
/// plays a shipped form.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Score {
    pub clips: BTreeMap<String, Clip>,
}
impl Score {
    pub fn validate(&self, base: &Library) -> Result<()> {
        if self.clips.len() > 2048 {
            return Err(Error("a score supports at most 2,048 clips".into()));
        }
        self.clips
            .iter()
            .try_for_each(|(id, clip)| Self::validate_clip(base, id, clip))
    }

    /// [`Score::validate`] for a write over `stored`: only the clips that
    /// differ from it are checked, so a stored clip that no longer passes
    /// does not block edits to the rest of the score.
    pub fn validate_changes(&self, base: &Library, stored: &Score) -> Result<()> {
        if self.clips.len() > 2048 {
            return Err(Error("a score supports at most 2,048 clips".into()));
        }
        self.clips
            .iter()
            .filter(|(id, clip)| stored.clips.get(*id) != Some(*clip))
            .try_for_each(|(id, clip)| Self::validate_clip(base, id, clip))
    }

    /// One clip's checks, so a player can leave out a clip that fails them
    /// and still play the rest of the score.
    pub fn validate_clip(base: &Library, id: &str, clip: &Clip) -> Result<()> {
        crate::graph::identity(id)?;
        clip.selection.validate()?;
        if !clip.start.is_finite()
            || !clip.duration.is_finite()
            || clip.duration <= 0.0
            || !(clip.start + clip.duration).is_finite()
        {
            return Err(Error(format!(
                "clip {id}: clip needs finite start and positive duration"
            )));
        }
        if !crate::forms::is_form(&clip.graph) {
            return Err(Error(format!("clip {id}: {} is not a form", clip.graph)));
        }
        let definition = base
            .definitions
            .get(&clip.graph)
            .ok_or_else(|| Error(format!("clip {id}: unknown form {}", clip.graph)))?;
        crate::forms::check_inputs(&clip.graph, definition, &clip.inputs)
            .map_err(|error| Error(format!("clip {id}: {error}")))?;
        // Aim always blends toward the aim under it by alpha.
        if crate::forms::replace_only(&clip.graph) && clip.blend_mode != crate::BlendMode::Replace {
            return Err(Error(format!(
                "clip {id}: an aim clip blends with replace only"
            )));
        }
        // Check fixed timing/value relationships without binding venue geometry.
        PreparedGraph::new_validated(
            base,
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
        let mut inputs = host_inputs.clone();
        inputs.extend(clip.inputs.clone());
        let program = PreparedGraph::new(
            base,
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
