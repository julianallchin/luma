use crate::clip_graph::ClipGraph;
use crate::{Error, Frame, Library, PreparedGraph, Result, Value};
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

/// How far, in beats, one clip may run past the start of the next on its
/// track and still count as ending where it starts. Stored beats do not come
/// back bit for bit.
const TOUCH: f64 = 1e-6;

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
    pub fn validate_clip(base: &Library, id: &str, clip: &Clip) -> Result<()> {
        crate::graph::identity(id)?;
        clip.selection.validate()?;
        let name = clip.name.trim();
        let prefix = |error: Error| {
            if name.is_empty() {
                Error(format!("clip ({id}): {error}"))
            } else {
                Error(format!("clip {name} ({id}): {error}"))
            }
        };
        crate::clip_graph::check_clip(clip).map_err(prefix)?;
        // Lower it without venue geometry: fixed timing and values fail here.
        PreparedGraph::new(
            base,
            &clip.graph,
            Frame {
                features: None,
                cells: &[],
                beat: clip.start,
                clip_start: clip.start,
                clip_duration: clip.duration,
                seed: clip.seed,
            },
        )
        .map_err(prefix)?;
        Ok(())
    }
    /// Make each z_index one track: no two clips at one z_index overlap in
    /// time. Clips that only share a boundary do not overlap.
    ///
    /// Where clips at one z_index overlap, they move onto new z_index values
    /// directly above it. At equal z_index the clip with the greater id paints
    /// on top, so it gets the higher value, and playback stays the same. The
    /// order of all z_index values stays; a value moves up only as far as it
    /// must. Returns whether any clip changed.
    pub fn separate_tracks(&mut self) -> bool {
        // In id order. A clip's level in its z_index is one above the highest
        // level of the clips with a smaller id that it overlaps.
        let spans: Vec<(i64, f64, f64)> = self
            .clips
            .values()
            .map(|clip| (clip.z_index, clip.start, clip.start + clip.duration))
            .collect();
        let mut levels = vec![0usize; spans.len()];
        for (index, &(z, start, end)) in spans.iter().enumerate() {
            levels[index] = spans[..index]
                .iter()
                .zip(&levels)
                .filter(|((other, from, to), _)| {
                    *other == z && start < to - TOUCH && *from < end - TOUCH
                })
                .map(|(_, level)| level + 1)
                .max()
                .unwrap_or(0);
        }
        let mut tracks: Vec<(i64, usize)> = spans
            .iter()
            .zip(&levels)
            .map(|(span, level)| (span.0, *level))
            .collect();
        tracks.sort_unstable();
        tracks.dedup();
        let mut renumbered = BTreeMap::new();
        let mut below: Option<i64> = None;
        for track in tracks {
            let z = below.map_or(track.0, |below| track.0.max(below + 1));
            renumbered.insert(track, z);
            below = Some(z);
        }
        let mut changed = false;
        for ((clip, span), level) in self.clips.values_mut().zip(&spans).zip(&levels) {
            let z = renumbered[&(span.0, *level)];
            changed |= clip.z_index != z;
            clip.z_index = z;
        }
        changed
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
    /// Prepare one clip over `cells`. The program owns its geometry and can
    /// render any frame, including a backwards seek.
    pub fn prepare_clip(
        &self,
        base: &Library,
        clip_id: &str,
        cells: &[crate::Cell],
    ) -> Result<PreparedClip> {
        let clip = self
            .clips
            .get(clip_id)
            .ok_or_else(|| Error(format!("unknown clip {clip_id}")))?;
        Self::validate_clip(base, clip_id, clip)?;
        let program = PreparedGraph::new(
            base,
            &clip.graph,
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
        beat: f64,
        cells: &[crate::Cell],
    ) -> Result<BTreeMap<String, Value>> {
        self.prepare_clip(base, clip_id, cells)?.evaluate(beat)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn score(clips: &[(&str, f64, f64, i64)]) -> Score {
        Score {
            clips: clips
                .iter()
                .map(|&(id, start, end, z)| {
                    let clip = Clip {
                        name: id.into(),
                        start,
                        duration: end - start,
                        seed: 0,
                        selection_seed: None,
                        selection: crate::Selection::all(),
                        z_index: z,
                        blend_mode: crate::BlendMode::Replace,
                        graph: ClipGraph::default(),
                    };
                    (id.to_owned(), clip)
                })
                .collect(),
        }
    }

    fn zs(score: &Score) -> Vec<(&str, i64)> {
        score
            .clips
            .iter()
            .map(|(id, clip)| (id.as_str(), clip.z_index))
            .collect()
    }

    #[test]
    fn separate_tracks_leaves_a_score_without_overlaps_alone() {
        // The end comes back from storage a hair past the next start.
        let mut separate = score(&[
            ("a", 0., 2.000_000_000_001, 0),
            ("b", 2., 4., 0),
            ("c", 0., 4., 3),
        ]);
        let before = separate.clone();
        assert!(!separate.separate_tracks());
        assert_eq!(separate, before);
    }

    #[test]
    fn the_clip_that_paints_on_top_gets_the_higher_track() {
        // At one z the greater id paints on top.
        let mut overlapping = score(&[
            ("a", 0., 8., 0),
            ("b", 2., 4., 0),
            ("c", 4., 6., 0),
            ("d", 0., 8., 1),
            ("e", 0., 8., 5),
        ]);
        assert!(overlapping.separate_tracks());
        assert_eq!(
            zs(&overlapping),
            vec![("a", 0), ("b", 1), ("c", 1), ("d", 2), ("e", 5)]
        );
        assert!(!overlapping.separate_tracks());
    }

    #[test]
    fn a_smaller_id_on_top_of_the_time_stays_below() {
        let mut overlapping = score(&[("a", 2., 4., 0), ("b", 0., 8., 0), ("c", 3., 5., 0)]);
        overlapping.separate_tracks();
        // b paints over a, and c over both.
        assert_eq!(zs(&overlapping), vec![("a", 0), ("b", 1), ("c", 2)]);
    }
}
