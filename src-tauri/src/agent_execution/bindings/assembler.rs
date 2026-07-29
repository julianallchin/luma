//! Assembles a `BindingManifest` from independent domain providers.
//!
//! Providers (track, audio, features, venue, score, graph_run, …) each write
//! into one builder using dotted paths; the builder owns the invariants that no
//! single provider can see: no two providers may claim the same path, and every
//! tensor must be structurally consistent with the artifact it points at.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::agent_execution::artifacts::{ArtifactDescriptor, ArtifactEncoding};
use crate::agent_execution::bindings::manifest::{
    AgentKind, AnalysisScope, ArtifactId, AxisSpec, BindingManifest, BindingRevision, BindingValue,
    Coordinates, Provenance, TensorRef, SCHEMA_VERSION,
};
use crate::agent_execution::error::{err, Result};

/// The `pcm_f32` header is 18 bytes; a tensor into a PCM artifact must start at
/// or after it (contract C1).
pub const PCM_HEADER_LEN: u64 = 18;

#[derive(Debug)]
pub struct BindingBuilder {
    revision: BindingRevision,
    agent_kind: AgentKind,
    scope: AnalysisScope,
    root: BindingValue,
    artifacts: BTreeMap<ArtifactId, ArtifactDescriptor>,
    claimed: BTreeSet<String>,
}

impl BindingBuilder {
    pub fn new(agent_kind: AgentKind, scope: AnalysisScope) -> Self {
        Self {
            revision: BindingRevision::new(),
            agent_kind,
            scope,
            root: BindingValue::record(),
            artifacts: BTreeMap::new(),
            claimed: BTreeSet::new(),
        }
    }

    /// Pin the revision id (used when replaying or testing).
    pub fn with_revision(mut self, revision: BindingRevision) -> Self {
        self.revision = revision;
        self
    }

    pub fn revision(&self) -> &BindingRevision {
        &self.revision
    }

    /// Make an artifact resolvable by tensors in this revision.
    pub fn artifact(&mut self, descriptor: ArtifactDescriptor) -> Result<&mut Self> {
        if let Some(existing) = self.artifacts.get(&descriptor.id) {
            if existing != &descriptor {
                return err(format!(
                    "artifact {} registered twice with different descriptors",
                    descriptor.id
                ));
            }
            return Ok(self);
        }
        self.artifacts.insert(descriptor.id.clone(), descriptor);
        Ok(self)
    }

    pub fn artifacts<I: IntoIterator<Item = ArtifactDescriptor>>(
        &mut self,
        descriptors: I,
    ) -> Result<&mut Self> {
        for d in descriptors {
            self.artifact(d)?;
        }
        Ok(self)
    }

    /// Attach a small serializable value (metadata, lists, scalars) inline.
    pub fn inline<T: Serialize>(&mut self, path: &str, value: T) -> Result<&mut Self> {
        let value = BindingValue::from_serializable(&value)?;
        self.set(path, value)
    }

    /// Attach a tensor reference. The artifact must be registered before `build`.
    pub fn tensor(&mut self, path: &str, tensor: TensorRef) -> Result<&mut Self> {
        self.set(path, BindingValue::tensor(tensor))
    }

    /// Attach a whole pre-assembled subtree.
    pub fn record(&mut self, path: &str, value: BindingValue) -> Result<&mut Self> {
        self.set(path, value)
    }

    /// Mark a branch as existing-but-unavailable. Distinct from empty (§9.7).
    pub fn unavailable(&mut self, path: &str, reason: impl Into<String>) -> Result<&mut Self> {
        self.set(path, BindingValue::unavailable(reason))
    }

    /// As `unavailable`, keeping the source's provenance so the agent can see
    /// which processor failed.
    pub fn unavailable_with_provenance(
        &mut self,
        path: &str,
        reason: impl Into<String>,
        provenance: Provenance,
    ) -> Result<&mut Self> {
        self.set(
            path,
            BindingValue::Unavailable {
                reason: reason.into(),
                provenance: Some(provenance),
            },
        )
    }

    fn set(&mut self, path: &str, value: BindingValue) -> Result<&mut Self> {
        let segments = parse_path(path)?;
        self.claim(path, &segments)?;

        let mut cursor = &mut self.root;
        for (i, segment) in segments.iter().enumerate() {
            let is_leaf = i + 1 == segments.len();
            let map = match cursor.as_record_mut() {
                Some(m) => m,
                // Unreachable: `claim` rejects any path whose prefix is a leaf.
                None => return err(format!("binding path '{path}' descends into a non-record")),
            };
            if is_leaf {
                map.insert(segment.clone(), value);
                return Ok(self);
            }
            cursor = map
                .entry(segment.clone())
                .or_insert_with(BindingValue::record);
        }
        unreachable!("segments is non-empty")
    }

    /// Reject duplicate and overlapping claims before mutating the tree.
    fn claim(&mut self, path: &str, segments: &[String]) -> Result<()> {
        if self.claimed.contains(path) {
            return err(format!("duplicate binding path: '{path}'"));
        }
        // A strict ancestor already holds a value here.
        let mut prefix = String::new();
        for segment in &segments[..segments.len() - 1] {
            if prefix.is_empty() {
                prefix.push_str(segment);
            } else {
                prefix.push('.');
                prefix.push_str(segment);
            }
            if self.claimed.contains(&prefix) {
                return err(format!(
                    "binding path '{path}' conflicts with existing binding '{prefix}'"
                ));
            }
        }
        // A descendant already holds a value under here.
        let child_prefix = format!("{path}.");
        if let Some(existing) = self
            .claimed
            .range(child_prefix.clone()..)
            .next()
            .filter(|c| c.starts_with(&child_prefix))
        {
            return err(format!(
                "binding path '{path}' conflicts with existing binding '{existing}'"
            ));
        }
        self.claimed.insert(path.to_string());
        Ok(())
    }

    /// Validate the whole tree and freeze it into an immutable revision.
    pub fn build(self) -> Result<BindingManifest> {
        let mut tensors = Vec::new();
        self.root.visit_tensors(&mut tensors);
        for (path, tensor) in &tensors {
            validate_tensor(path, tensor, &self.artifacts)?;
        }

        // Only artifacts actually referenced are published to the worker.
        let referenced: BTreeSet<ArtifactId> =
            tensors.iter().map(|(_, t)| t.artifact_id.clone()).collect();
        let artifacts = self
            .artifacts
            .into_iter()
            .filter(|(id, _)| referenced.contains(id))
            .collect();

        Ok(BindingManifest {
            schema_version: SCHEMA_VERSION,
            revision: self.revision,
            agent_kind: self.agent_kind,
            scope: self.scope,
            root: self.root,
            artifacts,
        })
    }
}

fn parse_path(path: &str) -> Result<Vec<String>> {
    if path.is_empty() {
        return err("binding path must not be empty");
    }
    let segments: Vec<String> = path.split('.').map(|s| s.to_string()).collect();
    if segments.iter().any(|s| s.is_empty()) {
        return err(format!("binding path '{path}' has an empty segment"));
    }
    if segments
        .iter()
        .any(|s| s.chars().any(|c| c.is_whitespace() || c == '$'))
    {
        return err(format!("binding path '{path}' has an invalid segment"));
    }
    Ok(segments)
}

fn validate_tensor(
    path: &str,
    tensor: &TensorRef,
    artifacts: &BTreeMap<ArtifactId, ArtifactDescriptor>,
) -> Result<()> {
    if tensor.axes.len() != tensor.shape.len() {
        return err(format!(
            "tensor '{path}': {} axes for a rank-{} shape {:?}",
            tensor.axes.len(),
            tensor.shape.len(),
            tensor.shape
        ));
    }
    for (i, axis) in tensor.axes.iter().enumerate() {
        let dim = tensor.shape[i];
        match axis {
            AxisSpec::Coordinates {
                values: Coordinates::Tensor(inner),
                name,
                ..
            } => {
                if inner.shape.len() != 1 || inner.shape[0] != dim {
                    return err(format!(
                        "tensor '{path}': coordinate axis '{name}' has shape {:?}, expected [{dim}]",
                        inner.shape
                    ));
                }
            }
            other => {
                let len = other.len().unwrap_or(dim);
                if len != dim {
                    return err(format!(
                        "tensor '{path}': axis '{}' covers {len} entries but dimension {i} is {dim}",
                        other.name()
                    ));
                }
            }
        }
    }

    let Some(descriptor) = artifacts.get(&tensor.artifact_id) else {
        return err(format!(
            "tensor '{path}' references unknown artifact '{}'",
            tensor.artifact_id
        ));
    };

    match descriptor.encoding {
        ArtifactEncoding::Npy if tensor.byte_offset != 0 => {
            return err(format!(
                "tensor '{path}': npy artifacts must use byte_offset 0, got {}",
                tensor.byte_offset
            ));
        }
        ArtifactEncoding::PcmF32 if tensor.byte_offset < PCM_HEADER_LEN => {
            return err(format!(
                "tensor '{path}': pcm_f32 byte_offset {} is inside the {PCM_HEADER_LEN}-byte header",
                tensor.byte_offset
            ));
        }
        ArtifactEncoding::Png | ArtifactEncoding::Utf8 => {
            return err(format!(
                "tensor '{path}': artifact '{}' is encoded as {} and cannot back a tensor",
                tensor.artifact_id,
                descriptor.encoding.as_str()
            ));
        }
        _ => {}
    }

    let end = tensor
        .byte_offset
        .checked_add(tensor.byte_size())
        .ok_or_else(|| {
            crate::agent_execution::error::DataPlaneError::new(format!(
                "tensor '{path}': byte range overflows"
            ))
        })?;
    if end > descriptor.byte_len {
        return err(format!(
            "tensor '{path}': needs bytes {}..{end} but artifact '{}' is {} bytes",
            tensor.byte_offset, tensor.artifact_id, descriptor.byte_len
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_execution::artifacts::ArtifactKind;

    fn artifact(id: &str, encoding: ArtifactEncoding, byte_len: u64) -> ArtifactDescriptor {
        ArtifactDescriptor {
            id: ArtifactId::from_string(id),
            kind: ArtifactKind::Tensor,
            encoding,
            rel_path: format!("inputs/{id}.bin"),
            byte_len,
            content_hash: None,
            sample_rate_hz: None,
            channels: None,
        }
    }

    fn beats(id: &str, count: usize) -> TensorRef {
        TensorRef::new(
            ArtifactId::from_string(id),
            DTYPE,
            vec![count],
            vec![AxisSpec::index("event", count)],
            Provenance::new("beat_this"),
        )
    }

    const DTYPE: crate::agent_execution::bindings::manifest::DType =
        crate::agent_execution::bindings::manifest::DType::F32;

    fn builder() -> BindingBuilder {
        BindingBuilder::new(AgentKind::TrackCopilot, AnalysisScope::default())
    }

    #[test]
    fn dotted_paths_create_nested_records() {
        let mut b = builder();
        b.artifact(artifact("a-1", ArtifactEncoding::RawLe, 16))
            .unwrap();
        b.inline("track.title", "Hex").unwrap();
        b.inline("track.bpm", 128.0).unwrap();
        b.tensor("features.beats", beats("a-1", 4)).unwrap();
        b.unavailable("audio.stems", "stem preprocessing has not completed")
            .unwrap();
        let manifest = b.build().unwrap();
        let json = serde_json::to_value(&manifest.root).unwrap();
        assert_eq!(json["track"]["title"], "Hex");
        assert_eq!(json["track"]["bpm"], 128.0);
        assert_eq!(json["features"]["beats"]["$kind"], "tensor");
        assert_eq!(json["audio"]["stems"]["$kind"], "unavailable");
    }

    #[test]
    fn providers_merge_deterministically() {
        let build = |order: usize| {
            let mut b = builder().with_revision(BindingRevision::parse("r-fixed").unwrap());
            let track = |b: &mut BindingBuilder| b.inline("track.title", "Hex").map(|_| ());
            let venue = |b: &mut BindingBuilder| b.inline("venue.name", "Basement").map(|_| ());
            if order == 0 {
                track(&mut b).unwrap();
                venue(&mut b).unwrap();
            } else {
                venue(&mut b).unwrap();
                track(&mut b).unwrap();
            }
            b.build().unwrap().to_json().unwrap()
        };
        assert_eq!(build(0), build(1));
    }

    #[test]
    fn duplicate_paths_are_rejected() {
        let mut b = builder();
        b.inline("track.title", "Hex").unwrap();
        let e = b.inline("track.title", "Other").unwrap_err();
        assert!(e.message().contains("duplicate binding path"), "{e}");
    }

    #[test]
    fn a_leaf_cannot_be_shadowed_by_a_child_path() {
        let mut b = builder();
        b.inline("track", "Hex").unwrap();
        let e = b.inline("track.title", "Other").unwrap_err();
        assert!(e.message().contains("conflicts"), "{e}");
    }

    #[test]
    fn a_subtree_cannot_be_overwritten_by_an_ancestor_path() {
        let mut b = builder();
        b.inline("track.title", "Hex").unwrap();
        let e = b.inline("track", "Other").unwrap_err();
        assert!(e.message().contains("conflicts"), "{e}");
    }

    #[test]
    fn malformed_paths_are_rejected() {
        let mut b = builder();
        assert!(b.inline("", 1).is_err());
        assert!(b.inline("track..title", 1).is_err());
        assert!(b.inline("track.$kind", 1).is_err());
        assert!(b.inline("track title", 1).is_err());
    }

    #[test]
    fn axis_count_must_match_rank() {
        let mut b = builder();
        b.artifact(artifact("a-1", ArtifactEncoding::RawLe, 64))
            .unwrap();
        b.tensor(
            "features.mel",
            TensorRef::new(
                ArtifactId::from_string("a-1"),
                DTYPE,
                vec![4, 4],
                vec![AxisSpec::index("frequency", 4)],
                Provenance::new("mel"),
            ),
        )
        .unwrap();
        let e = b.build().unwrap_err();
        assert!(e.message().contains("axes for a rank-2 shape"), "{e}");
    }

    #[test]
    fn axis_length_must_match_its_dimension() {
        let mut b = builder();
        b.artifact(artifact("a-1", ArtifactEncoding::RawLe, 64))
            .unwrap();
        b.tensor(
            "graph.view",
            TensorRef::new(
                ArtifactId::from_string("a-1"),
                DTYPE,
                vec![3],
                vec![AxisSpec::labels("channel", vec!["r".into(), "g".into()])],
                Provenance::new("graph_run"),
            ),
        )
        .unwrap();
        let e = b.build().unwrap_err();
        assert!(e.message().contains("covers 2 entries"), "{e}");
    }

    #[test]
    fn coordinate_axis_lengths_are_checked_inline_and_by_tensor() {
        let mut b = builder();
        b.artifact(artifact("a-1", ArtifactEncoding::RawLe, 64))
            .unwrap();
        b.tensor(
            "features.bar_intensity",
            TensorRef::new(
                ArtifactId::from_string("a-1"),
                DTYPE,
                vec![2],
                vec![AxisSpec::coordinates(
                    "bar",
                    vec![0.0, 1.0, 2.0],
                    Some("s".into()),
                )],
                Provenance::new("bars"),
            ),
        )
        .unwrap();
        assert!(b
            .build()
            .unwrap_err()
            .message()
            .contains("covers 3 entries"));

        let mut b = builder();
        b.artifact(artifact("a-1", ArtifactEncoding::RawLe, 64))
            .unwrap();
        b.artifact(artifact("a-t", ArtifactEncoding::RawLe, 64))
            .unwrap();
        let coords = TensorRef::new(
            ArtifactId::from_string("a-t"),
            DTYPE,
            vec![5],
            vec![AxisSpec::index("frame", 5)],
            Provenance::new("graph_run"),
        );
        b.tensor(
            "graph.view",
            TensorRef::new(
                ArtifactId::from_string("a-1"),
                DTYPE,
                vec![2],
                vec![AxisSpec::coordinate_tensor(
                    "time",
                    coords,
                    Some("s".into()),
                )],
                Provenance::new("graph_run"),
            ),
        )
        .unwrap();
        let e = b.build().unwrap_err();
        assert!(e.message().contains("expected [2]"), "{e}");
    }

    #[test]
    fn axis_coordinate_tensors_are_validated_against_their_own_artifact() {
        let mut b = builder();
        b.artifact(artifact("a-1", ArtifactEncoding::RawLe, 8))
            .unwrap();
        // Coordinate tensor points at an artifact nobody registered.
        let coords = TensorRef::new(
            ArtifactId::from_string("a-missing"),
            DTYPE,
            vec![2],
            vec![AxisSpec::index("frame", 2)],
            Provenance::new("graph_run"),
        );
        b.tensor(
            "graph.view",
            TensorRef::new(
                ArtifactId::from_string("a-1"),
                DTYPE,
                vec![2],
                vec![AxisSpec::coordinate_tensor(
                    "time",
                    coords,
                    Some("s".into()),
                )],
                Provenance::new("graph_run"),
            ),
        )
        .unwrap();
        let e = b.build().unwrap_err();
        assert!(e.message().contains("a-missing"), "{e}");
    }

    #[test]
    fn missing_artifacts_are_rejected() {
        let mut b = builder();
        b.tensor("features.beats", beats("a-nope", 2)).unwrap();
        let e = b.build().unwrap_err();
        assert!(e.message().contains("unknown artifact"), "{e}");
    }

    #[test]
    fn byte_overrun_is_rejected() {
        let mut b = builder();
        b.artifact(artifact("a-1", ArtifactEncoding::RawLe, 8))
            .unwrap();
        b.tensor("features.beats", beats("a-1", 3)).unwrap();
        let e = b.build().unwrap_err();
        assert!(e.message().contains("is 8 bytes"), "{e}");
    }

    #[test]
    fn byte_offset_is_included_in_the_bounds_check() {
        let mut b = builder();
        b.artifact(artifact("a-1", ArtifactEncoding::RawLe, 8))
            .unwrap();
        b.tensor("features.beats", beats("a-1", 2).with_offset(4))
            .unwrap();
        assert!(b.build().is_err());

        let mut b = builder();
        b.artifact(artifact("a-1", ArtifactEncoding::RawLe, 12))
            .unwrap();
        b.tensor("features.beats", beats("a-1", 2).with_offset(4))
            .unwrap();
        assert!(b.build().is_ok());
    }

    #[test]
    fn pcm_tensors_must_start_after_the_header() {
        let mut b = builder();
        b.artifact(artifact("a-pcm", ArtifactEncoding::PcmF32, 1024))
            .unwrap();
        b.tensor("audio.mix", beats("a-pcm", 4)).unwrap();
        let e = b.build().unwrap_err();
        assert!(e.message().contains("18-byte header"), "{e}");

        let mut b = builder();
        b.artifact(artifact("a-pcm", ArtifactEncoding::PcmF32, 1024))
            .unwrap();
        b.tensor("audio.mix", beats("a-pcm", 4).with_offset(18))
            .unwrap();
        assert!(b.build().is_ok());
    }

    #[test]
    fn npy_tensors_must_start_at_zero() {
        let mut b = builder();
        b.artifact(artifact("a-npy", ArtifactEncoding::Npy, 1024))
            .unwrap();
        b.tensor("features.mert", beats("a-npy", 4).with_offset(128))
            .unwrap();
        let e = b.build().unwrap_err();
        assert!(e.message().contains("byte_offset 0"), "{e}");
    }

    #[test]
    fn png_artifacts_cannot_back_a_tensor() {
        let mut b = builder();
        let mut d = artifact("a-png", ArtifactEncoding::Png, 1024);
        d.kind = ArtifactKind::Figure;
        b.artifact(d).unwrap();
        b.tensor("figures.first", beats("a-png", 4)).unwrap();
        let e = b.build().unwrap_err();
        assert!(e.message().contains("cannot back a tensor"), "{e}");
    }

    #[test]
    fn unreferenced_artifacts_are_not_published() {
        let mut b = builder();
        b.artifact(artifact("a-1", ArtifactEncoding::RawLe, 8))
            .unwrap();
        b.artifact(artifact("a-unused", ArtifactEncoding::RawLe, 8))
            .unwrap();
        b.tensor("features.beats", beats("a-1", 2)).unwrap();
        let manifest = b.build().unwrap();
        assert_eq!(manifest.artifacts.len(), 1);
        assert!(manifest
            .artifacts
            .contains_key(&ArtifactId::from_string("a-1")));
    }

    #[test]
    fn re_registering_an_artifact_with_a_different_descriptor_is_rejected() {
        let mut b = builder();
        b.artifact(artifact("a-1", ArtifactEncoding::RawLe, 8))
            .unwrap();
        b.artifact(artifact("a-1", ArtifactEncoding::RawLe, 8))
            .unwrap();
        let e = b
            .artifact(artifact("a-1", ArtifactEncoding::RawLe, 16))
            .unwrap_err();
        assert!(e.message().contains("registered twice"), "{e}");
    }

    #[test]
    fn empty_tensors_and_empty_records_survive_validation() {
        let mut b = builder();
        b.artifact(artifact("a-1", ArtifactEncoding::RawLe, 0))
            .unwrap();
        b.tensor("features.drum_onsets.kick", beats("a-1", 0))
            .unwrap();
        b.record("features.chords", BindingValue::record()).unwrap();
        b.unavailable("features.mert", "MERT embedding not computed")
            .unwrap();
        let manifest = b.build().unwrap();
        let json = serde_json::to_value(&manifest.root).unwrap();
        assert_eq!(json["features"]["drum_onsets"]["kick"]["shape"][0], 0);
        assert!(json["features"]["chords"].as_object().unwrap().is_empty());
        assert_eq!(json["features"]["mert"]["$kind"], "unavailable");
    }

    #[test]
    fn provenance_is_preserved_through_the_builder() {
        let mut b = builder();
        b.artifact(artifact("a-1", ArtifactEncoding::RawLe, 8))
            .unwrap();
        let mut t = beats("a-1", 2);
        t.provenance = Provenance::new("beat_this")
            .with_version("2026.03")
            .with_note("downbeats excluded");
        b.tensor("features.beats", t).unwrap();
        let manifest = b.build().unwrap();
        let json = serde_json::to_value(&manifest.root).unwrap();
        assert_eq!(
            json["features"]["beats"]["provenance"]["processor_version"],
            "2026.03"
        );
        assert_eq!(
            json["features"]["beats"]["provenance"]["note"],
            "downbeats excluded"
        );
    }
}
