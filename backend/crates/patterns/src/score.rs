use crate::clip_graph::ClipGraph;
use crate::{Error, Frame, Library, PreparedGraph, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    /// What the clip is called. The checker requires one.
    #[serde(default)]
    pub name: String,
    /// The handle an agent gave the clip to find it again, unique among its
    /// score's clips. The clip's identity is its id; most clips have no label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub start: f64,
    pub duration: f64,
    pub seed: u64,
    /// The venue's groups, resolved by the host; never physical fixture ids.
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

/// Canonical score document: clips keyed by their id, a random UUID the
/// writer generated. Every clip owns one graph.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Score {
    pub clips: BTreeMap<String, Clip>,
}
impl Score {
    pub fn validate(&self, base: &Library) -> Result<()> {
        self.validate_whole()?;
        self.clips
            .iter()
            .try_for_each(|(id, clip)| Self::validate_clip(base, id, clip))
    }

    /// What holds across clips: the size limit, and that a label names one
    /// clip.
    fn validate_whole(&self) -> Result<()> {
        if self.clips.len() > 2048 {
            return Err(Error("a score supports at most 2,048 clips".into()));
        }
        self.unique_labels()
    }

    /// Refuse a label two clips carry. Storage checks this on every save,
    /// whoever writes: the server holds one live clip per label and score.
    pub fn unique_labels(&self) -> Result<()> {
        let mut labels = BTreeSet::new();
        for label in self.clips.values().filter_map(|clip| clip.label.as_deref()) {
            if !labels.insert(label) {
                return Err(Error(format!(
                    "two clips are labelled `{label}`; a label names one clip in its score"
                )));
            }
        }
        Ok(())
    }

    /// [`Score::validate`] for a write over `stored`: only the clips that
    /// differ from it are checked, so a stored clip that no longer passes
    /// does not block edits to the rest of the score.
    pub fn validate_changes(&self, base: &Library, stored: &Score) -> Result<()> {
        self.validate_whole()?;
        self.clips
            .iter()
            .filter(|(id, clip)| stored.clips.get(*id) != Some(*clip))
            .try_for_each(|(id, clip)| Self::validate_clip(base, id, clip))
    }

    /// One clip's checks, so a player can leave out a clip that fails them
    /// and still play the rest of the score.
    pub fn validate_clip(base: &Library, id: &str, clip: &Clip) -> Result<()> {
        crate::graph::identity(id)?;
        let name = clip.name.trim();
        let prefix = |error: Error| {
            if name.is_empty() {
                Error(format!("clip ({id}): {error}"))
            } else {
                Error(format!("clip {name} ({id}): {error}"))
            }
        };
        if let Some(label) = clip.label.as_deref() {
            crate::graph::identity(label)
                .map_err(|error| prefix(Error(format!("label: {error}"))))?;
        }
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
}

/// One writer's changes to a score, against the copy it read: each clip it
/// added or changed, whole, and each id it removed.
///
/// Applied over whatever is stored now, so a clip the writer did not touch
/// stays as storage has it — another writer's new clip survives, and a clip
/// another writer deleted stays deleted. A clip both changed is the later
/// writer's, whole.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoreEdit {
    #[serde(default)]
    pub clips: BTreeMap<String, Clip>,
    #[serde(default)]
    pub removed: BTreeSet<String>,
}
impl ScoreEdit {
    /// What turns `base` into `candidate`.
    #[must_use]
    pub fn between(base: &Score, candidate: &Score) -> Self {
        Self {
            clips: candidate
                .clips
                .iter()
                .filter(|(id, clip)| base.clips.get(*id) != Some(*clip))
                .map(|(id, clip)| (id.clone(), clip.clone()))
                .collect(),
            removed: base
                .clips
                .keys()
                .filter(|id| !candidate.clips.contains_key(*id))
                .cloned()
                .collect(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty() && self.removed.is_empty()
    }

    /// `score` with this edit on it.
    #[must_use]
    pub fn applied_to(&self, score: &Score) -> Score {
        let mut clips = score.clips.clone();
        clips.retain(|id, _| !self.removed.contains(id));
        clips.extend(self.clips.clone());
        Score { clips }
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
                        label: None,
                        start,
                        duration: end - start,
                        seed: 0,
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
    fn a_label_names_one_clip() {
        let library = crate::standard_library();
        let mut labelled = score(&[("a", 0., 2., 0), ("b", 2., 4., 0)]);
        labelled.clips.get_mut("a").unwrap().label = Some("drop".into());
        let stored = labelled.clone();
        labelled.clips.get_mut("b").unwrap().label = Some("drop".into());
        let error = labelled.validate_changes(&library, &stored).unwrap_err();
        assert!(error.to_string().contains("`drop`"), "{error}");
        // The label goes with the clip, so it frees up when the clip does.
        labelled.clips.remove("a");
        assert!(labelled.unique_labels().is_ok());
    }

    /// A stored clip without a label reads, and writes back without the key,
    /// so documents saved before labels compare equal.
    #[test]
    fn an_unlabelled_clip_has_no_label_key() {
        let plain = score(&[("a", 0., 2., 0)]);
        let json = serde_json::to_value(&plain).unwrap();
        assert!(json["clips"]["a"].get("label").is_none());
        assert_eq!(serde_json::from_value::<Score>(json).unwrap(), plain);
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
