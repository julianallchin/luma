use crate::clip_graph::ClipGraph;
use crate::{Error, Library, PreparedGraph, Result, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    /// What the clip is called. The checker requires one.
    #[serde(default)]
    pub name: String,
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
    /// The clip's own graph ([`crate::clip_graph`]).
    pub graph: ClipGraph,
}
fn all_selection() -> crate::Selection {
    crate::Selection::all()
}
fn replace_blend() -> crate::BlendMode {
    crate::BlendMode::Replace
}

/// Canonical score document: clips keyed by their stable identity. Every clip
/// owns one graph.
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
    pub fn validate_clip(_base: &Library, id: &str, clip: &Clip) -> Result<()> {
        crate::graph::identity(id)?;
        clip.selection.validate()?;
        crate::clip_graph::check_clip(clip).map_err(|error| {
            let name = clip.name.trim();
            if name.is_empty() {
                Error(format!("clip ({id}): {error}"))
            } else {
                Error(format!("clip {name} ({id}): {error}"))
            }
        })
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
        // Phase 1 of clip-graphs: the graph lowering lands in phase 2.
        let _ = (host_inputs, cells, clip);
        Err(Error(format!(
            "clip {clip_id}: clip graphs do not play yet (lowering lands in phase 2)"
        )))
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
