//! Three-way merge for the presentation text in canonical score documents.
//!
//! Score semantics are merged separately. This module overlays comments onto
//! that semantic result by identities that survive reordering and movement:
//! annotation UUIDs, layer z-indices, and the document itself. Source spans
//! are deliberately excluded because they describe an input file, not
//! authored meaning.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{Comment, Document, Layer, Span, Trivia};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriviaMergeInput {
    Base,
    Ours,
    Theirs,
    Semantic,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriviaField {
    LeadingComments,
    TrailingComment,
    DocumentTrailingComments,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TriviaMergePathSegment {
    Input(TriviaMergeInput),
    Document,
    Layer(i64),
    Annotation(String),
    Field(TriviaField),
}

/// Stable, machine-readable location of a comment conflict.
#[derive(Clone, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TriviaMergePath(pub Vec<TriviaMergePathSegment>);

impl TriviaMergePath {
    fn annotation(id: String) -> Self {
        Self(vec![TriviaMergePathSegment::Annotation(id)])
    }

    fn layer(z_index: i64) -> Self {
        Self(vec![TriviaMergePathSegment::Layer(z_index)])
    }

    fn document() -> Self {
        Self(vec![TriviaMergePathSegment::Document])
    }

    fn child(&self, segment: TriviaMergePathSegment) -> Self {
        let mut segments = self.0.clone();
        segments.push(segment);
        Self(segments)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriviaMergeConflictKind {
    ConcurrentEdit,
    DeleteModify,
    DuplicateKey,
    InvalidInput,
}

/// `Missing` means that the entity carrying the comment does not exist in
/// that input. `Present([])` means that it exists and has no comment in this
/// field; that distinction is required for delete/modify conflicts.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "state", content = "comments", rename_all = "snake_case")]
pub enum TriviaMergeValue {
    Missing,
    Present(Vec<String>),
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TriviaMergeConflict {
    pub path: TriviaMergePath,
    pub kind: TriviaMergeConflictKind,
    pub base: TriviaMergeValue,
    pub ours: TriviaMergeValue,
    pub theirs: TriviaMergeValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A conflicted merge exposes no partially merged document. Callers may show
/// the structured conflicts and retry after the author resolves them.
#[derive(Clone, Debug, PartialEq)]
pub struct TriviaMergeOutcome {
    pub merged: Option<Document>,
    pub conflicts: Vec<TriviaMergeConflict>,
}

impl TriviaMergeOutcome {
    pub fn into_result(self) -> Result<Document, Vec<TriviaMergeConflict>> {
        self.merged.ok_or(self.conflicts)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TextTrivia {
    leading: Vec<String>,
    trailing: Vec<String>,
}

impl From<&Trivia> for TextTrivia {
    fn from(value: &Trivia) -> Self {
        Self {
            leading: comment_texts(&value.leading_comments),
            trailing: value
                .trailing_comment
                .iter()
                .map(|comment| comment.text.clone())
                .collect(),
        }
    }
}

impl TextTrivia {
    fn into_trivia(self) -> Trivia {
        Trivia {
            leading_comments: comments(self.leading),
            trailing_comment: self.trailing.into_iter().next().map(comment),
        }
    }

    fn is_empty(&self) -> bool {
        self.leading.is_empty() && self.trailing.is_empty()
    }
}

#[derive(Default)]
struct TriviaIndex {
    annotations: BTreeMap<String, TextTrivia>,
    layers: BTreeMap<i64, TextTrivia>,
}

#[derive(Clone, Copy)]
enum ConflictPolicy {
    Reject,
    LaterWins,
}

/// Overlay a losslessly merged set of comments onto an already semantically
/// merged score document.
///
/// Annotation comments follow stable annotation IDs even when a clip moves to
/// another layer or time. Layer comments follow z-index. A one-sided comment
/// edit is selected, identical edits coalesce, and incompatible concurrent
/// edits are returned as structured conflicts. The returned document uses
/// default spans because all source locations become stale after canonical
/// serialization.
pub fn merge_document_trivia(
    base: &Document,
    ours: &Document,
    theirs: &Document,
    semantic: Document,
) -> TriviaMergeOutcome {
    merge_document_trivia_with_policy(base, ours, theirs, semantic, ConflictPolicy::Reject)
}

/// Total comment merge for server-ordered device convergence. Independent
/// comment edits compose by stable annotation/layer identity; when both sides
/// edit the same comment field, the later server proposal wins. Invalid or
/// ambiguously keyed input still returns structured errors so the caller can
/// take the whole-proposal terminal fallback.
pub fn merge_document_trivia_later_wins(
    base: &Document,
    current: &Document,
    proposal: &Document,
    semantic: Document,
) -> Result<Document, Vec<TriviaMergeConflict>> {
    merge_document_trivia_with_policy(base, current, proposal, semantic, ConflictPolicy::LaterWins)
        .into_result()
}

fn merge_document_trivia_with_policy(
    base: &Document,
    ours: &Document,
    theirs: &Document,
    mut semantic: Document,
    policy: ConflictPolicy,
) -> TriviaMergeOutcome {
    let mut conflicts = Vec::new();
    let base_index = index_document(TriviaMergeInput::Base, base, &mut conflicts);
    let ours_index = index_document(TriviaMergeInput::Ours, ours, &mut conflicts);
    let theirs_index = index_document(TriviaMergeInput::Theirs, theirs, &mut conflicts);
    let semantic_index = index_document(TriviaMergeInput::Semantic, &semantic, &mut conflicts);

    let annotation_ids: BTreeSet<String> = base_index
        .annotations
        .keys()
        .chain(ours_index.annotations.keys())
        .chain(theirs_index.annotations.keys())
        .cloned()
        .collect();
    let mut annotation_trivia = BTreeMap::new();
    for id in annotation_ids {
        let path = TriviaMergePath::annotation(id.clone());
        if let Some(trivia) = merge_trivia(
            base_index.annotations.get(&id),
            ours_index.annotations.get(&id),
            theirs_index.annotations.get(&id),
            &path,
            policy,
            &mut conflicts,
        ) {
            annotation_trivia.insert(id, trivia);
        }
    }

    let z_indices: BTreeSet<i64> = base_index
        .layers
        .keys()
        .chain(ours_index.layers.keys())
        .chain(theirs_index.layers.keys())
        .cloned()
        .collect();
    let mut layer_trivia = BTreeMap::new();
    for z_index in z_indices {
        let path = TriviaMergePath::layer(z_index);
        if let Some(trivia) = merge_trivia(
            base_index.layers.get(&z_index),
            ours_index.layers.get(&z_index),
            theirs_index.layers.get(&z_index),
            &path,
            policy,
            &mut conflicts,
        ) {
            layer_trivia.insert(z_index, trivia);
        }
    }

    let base_trailing_comments = comment_texts(&base.trailing_comments);
    let ours_trailing_comments = comment_texts(&ours.trailing_comments);
    let theirs_trailing_comments = comment_texts(&theirs.trailing_comments);
    let trailing_comments = merge_field(
        Some(&base_trailing_comments),
        Some(&ours_trailing_comments),
        Some(&theirs_trailing_comments),
        &TriviaMergePath::document().child(TriviaMergePathSegment::Field(
            TriviaField::DocumentTrailingComments,
        )),
        policy,
        &mut conflicts,
    )
    .unwrap_or_default();

    // Validate before mutation so malformed semantic input cannot produce a
    // superficially clean but ambiguously addressed result.
    if !conflicts.is_empty() {
        return finish(semantic, conflicts);
    }

    for layer in &mut semantic.layers {
        layer.trivia = layer_trivia
            .remove(&layer.z_index)
            .unwrap_or_default()
            .into_trivia();
        for annotation in &mut layer.annotations {
            let Some(id) = annotation.id.as_ref() else {
                // `index_document` already emitted the corresponding invalid
                // input conflict; this branch is only a defensive guard.
                continue;
            };
            annotation.trivia = annotation_trivia
                .remove(id)
                .unwrap_or_default()
                .into_trivia();
        }
    }

    // A layer can exist solely to carry authored notes. Keeping a clean,
    // one-sided trivia-only layer is semantically inert and avoids losing it
    // when the semantic merger materializes layers only from clips.
    for (z_index, trivia) in layer_trivia {
        if !trivia.is_empty() && !semantic_index.layers.contains_key(&z_index) {
            semantic.layers.push(Layer {
                z_index,
                explicit_z: true,
                annotations: Vec::new(),
                trivia: trivia.into_trivia(),
            });
        }
    }
    semantic
        .layers
        .sort_by(|left, right| left.z_index.cmp(&right.z_index));
    semantic.trailing_comments = comments(trailing_comments);
    finish(semantic, conflicts)
}

fn index_document(
    input: TriviaMergeInput,
    document: &Document,
    conflicts: &mut Vec<TriviaMergeConflict>,
) -> TriviaIndex {
    let mut index = TriviaIndex::default();
    let mut missing_id = false;
    for layer in &document.layers {
        if index
            .layers
            .insert(layer.z_index, TextTrivia::from(&layer.trivia))
            .is_some()
        {
            conflicts.push(structural_conflict(
                TriviaMergePath(vec![
                    TriviaMergePathSegment::Input(input),
                    TriviaMergePathSegment::Layer(layer.z_index),
                ]),
                TriviaMergeConflictKind::DuplicateKey,
                format!("duplicate layer z-index {}", layer.z_index),
            ));
        }
        for annotation in &layer.annotations {
            let Some(id) = annotation.id.as_ref() else {
                missing_id = true;
                continue;
            };
            if index
                .annotations
                .insert(id.clone(), TextTrivia::from(&annotation.trivia))
                .is_some()
            {
                conflicts.push(structural_conflict(
                    TriviaMergePath(vec![
                        TriviaMergePathSegment::Input(input),
                        TriviaMergePathSegment::Annotation(id.clone()),
                    ]),
                    TriviaMergeConflictKind::DuplicateKey,
                    format!("duplicate annotation id {id}"),
                ));
            }
        }
    }
    if missing_id {
        conflicts.push(structural_conflict(
            TriviaMergePath(vec![TriviaMergePathSegment::Input(input)]),
            TriviaMergeConflictKind::InvalidInput,
            "every annotation must have a stable id",
        ));
    }
    index
}

fn merge_trivia(
    base: Option<&TextTrivia>,
    ours: Option<&TextTrivia>,
    theirs: Option<&TextTrivia>,
    path: &TriviaMergePath,
    policy: ConflictPolicy,
    conflicts: &mut Vec<TriviaMergeConflict>,
) -> Option<TextTrivia> {
    let leading = merge_field(
        base.map(|trivia| trivia.leading.as_slice()),
        ours.map(|trivia| trivia.leading.as_slice()),
        theirs.map(|trivia| trivia.leading.as_slice()),
        &path.child(TriviaMergePathSegment::Field(TriviaField::LeadingComments)),
        policy,
        conflicts,
    );
    let trailing = merge_field(
        base.map(|trivia| trivia.trailing.as_slice()),
        ours.map(|trivia| trivia.trailing.as_slice()),
        theirs.map(|trivia| trivia.trailing.as_slice()),
        &path.child(TriviaMergePathSegment::Field(TriviaField::TrailingComment)),
        policy,
        conflicts,
    );
    match (leading, trailing) {
        (None, None) => None,
        (leading, trailing) => Some(TextTrivia {
            leading: leading.unwrap_or_default(),
            trailing: trailing.unwrap_or_default(),
        }),
    }
}

fn merge_field(
    base: Option<&[String]>,
    ours: Option<&[String]>,
    theirs: Option<&[String]>,
    path: &TriviaMergePath,
    policy: ConflictPolicy,
    conflicts: &mut Vec<TriviaMergeConflict>,
) -> Option<Vec<String>> {
    match (base, ours, theirs) {
        (None, None, None) => None,
        (None, Some(ours), None) => Some(ours.to_vec()),
        (None, None, Some(theirs)) => Some(theirs.to_vec()),
        (None, Some(ours), Some(theirs)) if ours == theirs => Some(ours.to_vec()),
        // Two semantically identical additions share an implicit empty trivia
        // baseline, so a comment authored on only one side remains one-sided.
        (None, Some([]), Some(theirs)) => Some(theirs.to_vec()),
        (None, Some(ours), Some([])) => Some(ours.to_vec()),
        (None, Some(ours), Some(theirs)) => {
            if matches!(policy, ConflictPolicy::LaterWins) {
                return Some(theirs.to_vec());
            }
            conflicts.push(field_conflict(
                path.clone(),
                TriviaMergeConflictKind::ConcurrentEdit,
                None,
                Some(ours),
                Some(theirs),
            ));
            Some(ours.to_vec())
        }
        (Some(_base), None, None) => None,
        (Some(base), None, Some(theirs)) if theirs == base => None,
        (Some(base), Some(ours), None) if ours == base => None,
        (Some(base), None, Some(theirs)) => {
            if matches!(policy, ConflictPolicy::LaterWins) {
                return Some(theirs.to_vec());
            }
            conflicts.push(field_conflict(
                path.clone(),
                TriviaMergeConflictKind::DeleteModify,
                Some(base),
                None,
                Some(theirs),
            ));
            None
        }
        (Some(base), Some(ours), None) => {
            if matches!(policy, ConflictPolicy::LaterWins) {
                return None;
            }
            conflicts.push(field_conflict(
                path.clone(),
                TriviaMergeConflictKind::DeleteModify,
                Some(base),
                Some(ours),
                None,
            ));
            None
        }
        (Some(_base), Some(ours), Some(theirs)) if ours == theirs => Some(ours.to_vec()),
        (Some(base), Some(ours), Some(theirs)) if ours == base => Some(theirs.to_vec()),
        (Some(base), Some(ours), Some(theirs)) if theirs == base => Some(ours.to_vec()),
        (Some(base), Some(ours), Some(theirs)) => {
            if matches!(policy, ConflictPolicy::LaterWins) {
                return Some(theirs.to_vec());
            }
            conflicts.push(field_conflict(
                path.clone(),
                TriviaMergeConflictKind::ConcurrentEdit,
                Some(base),
                Some(ours),
                Some(theirs),
            ));
            Some(ours.to_vec())
        }
    }
}

fn field_conflict(
    path: TriviaMergePath,
    kind: TriviaMergeConflictKind,
    base: Option<&[String]>,
    ours: Option<&[String]>,
    theirs: Option<&[String]>,
) -> TriviaMergeConflict {
    TriviaMergeConflict {
        path,
        kind,
        base: merge_value(base),
        ours: merge_value(ours),
        theirs: merge_value(theirs),
        detail: None,
    }
}

fn structural_conflict(
    path: TriviaMergePath,
    kind: TriviaMergeConflictKind,
    detail: impl Into<String>,
) -> TriviaMergeConflict {
    TriviaMergeConflict {
        path,
        kind,
        base: TriviaMergeValue::Missing,
        ours: TriviaMergeValue::Missing,
        theirs: TriviaMergeValue::Missing,
        detail: Some(detail.into()),
    }
}

fn merge_value(value: Option<&[String]>) -> TriviaMergeValue {
    match value {
        Some(value) => TriviaMergeValue::Present(value.to_vec()),
        None => TriviaMergeValue::Missing,
    }
}

fn comment_texts(comments: &[Comment]) -> Vec<String> {
    comments
        .iter()
        .map(|comment| comment.text.clone())
        .collect()
}

fn comments(texts: Vec<String>) -> Vec<Comment> {
    texts.into_iter().map(comment).collect()
}

fn comment(text: String) -> Comment {
    Comment {
        text,
        span: Span::default(),
    }
}

fn finish(document: Document, mut conflicts: Vec<TriviaMergeConflict>) -> TriviaMergeOutcome {
    conflicts.sort();
    conflicts.dedup();
    if conflicts.is_empty() {
        TriviaMergeOutcome {
            merged: Some(document),
            conflicts,
        }
    } else {
        TriviaMergeOutcome {
            merged: None,
            conflicts,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::models::node_graph::BlendMode;

    use super::super::types::{Annotation, BarRange, Loc, TimeUnit};
    use super::*;

    fn c(text: &str, offset: usize) -> Comment {
        Comment {
            text: text.to_owned(),
            span: Span {
                start: Loc {
                    line: offset + 1,
                    column: offset + 2,
                    offset,
                },
                end: Loc {
                    line: offset + 1,
                    column: offset + 3,
                    offset: offset + 1,
                },
            },
        }
    }

    fn trivia(leading: &[&str], trailing: Option<&str>, offset: usize) -> Trivia {
        Trivia {
            leading_comments: leading
                .iter()
                .enumerate()
                .map(|(index, text)| c(text, offset + index))
                .collect(),
            trailing_comment: trailing.map(|text| c(text, offset + leading.len())),
        }
    }

    fn annotation(id: &str, trivia: Trivia) -> Annotation {
        Annotation {
            id: Some(id.to_owned()),
            pattern: "solid_color".to_owned(),
            pattern_id: Some("pattern-id".to_owned()),
            selection: None,
            range: BarRange {
                start: 0.0,
                end: 1.0,
                unit: TimeUnit::Seconds,
            },
            args: Vec::new(),
            blend: BlendMode::Replace,
            span: Span::default(),
            trivia,
        }
    }

    fn layer(z_index: i64, annotations: Vec<Annotation>, trivia: Trivia) -> Layer {
        Layer {
            z_index,
            explicit_z: true,
            annotations,
            trivia,
        }
    }

    fn document(layers: Vec<Layer>, trailing: &[&str]) -> Document {
        Document {
            layers,
            trailing_comments: trailing
                .iter()
                .enumerate()
                .map(|(index, text)| c(text, 100 + index))
                .collect(),
        }
    }

    fn annotation_trivia<'a>(document: &'a Document, id: &str) -> &'a Trivia {
        &document
            .layers
            .iter()
            .flat_map(|layer| &layer.annotations)
            .find(|annotation| annotation.id.as_deref() == Some(id))
            .unwrap()
            .trivia
    }

    #[test]
    fn server_ordered_merge_composes_independent_comment_edits() {
        let base = document(
            vec![layer(
                0,
                vec![
                    annotation("a", Trivia::default()),
                    annotation("b", Trivia::default()),
                ],
                Trivia::default(),
            )],
            &[],
        );
        let mut current = base.clone();
        current.layers[0].annotations[0].trivia = trivia(&["current a"], None, 0);
        let mut proposal = base.clone();
        proposal.layers[0].annotations[1].trivia = trivia(&["proposal b"], None, 1);

        let merged =
            merge_document_trivia_later_wins(&base, &current, &proposal, base.clone()).unwrap();
        assert_eq!(
            comment_texts(&annotation_trivia(&merged, "a").leading_comments),
            vec!["current a"]
        );
        assert_eq!(
            comment_texts(&annotation_trivia(&merged, "b").leading_comments),
            vec!["proposal b"]
        );
    }

    #[test]
    fn server_ordered_merge_uses_later_comment_on_overlap() {
        let base = document(
            vec![layer(
                0,
                vec![annotation("a", trivia(&["base"], None, 0))],
                Trivia::default(),
            )],
            &[],
        );
        let mut current = base.clone();
        current.layers[0].annotations[0].trivia = trivia(&["current"], None, 1);
        let mut proposal = base.clone();
        proposal.layers[0].annotations[0].trivia = trivia(&["proposal"], None, 2);

        let merged =
            merge_document_trivia_later_wins(&base, &current, &proposal, base.clone()).unwrap();
        assert_eq!(
            comment_texts(&annotation_trivia(&merged, "a").leading_comments),
            vec!["proposal"]
        );
    }

    #[test]
    fn independent_comment_only_edits_merge_by_stable_annotation_id() {
        let base = document(
            vec![layer(
                0,
                vec![
                    annotation("a", trivia(&["a base"], None, 0)),
                    annotation("b", trivia(&[], Some("b base"), 1)),
                ],
                Trivia::default(),
            )],
            &[],
        );
        let mut ours = base.clone();
        ours.layers[0].annotations.reverse();
        ours.layers[0].annotations[1].trivia = trivia(&["a ours"], None, 10);
        let mut theirs = base.clone();
        theirs.layers[0].annotations[1].trivia = trivia(&[], Some("b theirs"), 20);
        let semantic = document(
            vec![layer(
                0,
                vec![
                    annotation("b", Trivia::default()),
                    annotation("a", Trivia::default()),
                ],
                Trivia::default(),
            )],
            &[],
        );

        let merged = merge_document_trivia(&base, &ours, &theirs, semantic)
            .into_result()
            .unwrap();
        assert_eq!(
            comment_texts(&annotation_trivia(&merged, "a").leading_comments),
            ["a ours"]
        );
        assert_eq!(
            annotation_trivia(&merged, "b")
                .trailing_comment
                .as_ref()
                .map(|comment| comment.text.as_str()),
            Some("b theirs")
        );
    }

    #[test]
    fn equal_comment_text_ignores_source_spans_and_resets_merged_spans() {
        let base = document(
            vec![layer(
                0,
                vec![annotation("a", trivia(&["base"], None, 0))],
                Trivia::default(),
            )],
            &[],
        );
        let mut ours = base.clone();
        ours.layers[0].annotations[0].trivia = trivia(&["same"], None, 10);
        let mut theirs = base.clone();
        theirs.layers[0].annotations[0].trivia = trivia(&["same"], None, 999);
        let semantic = document(
            vec![layer(
                0,
                vec![annotation("a", Trivia::default())],
                Trivia::default(),
            )],
            &[],
        );

        let merged = merge_document_trivia(&base, &ours, &theirs, semantic)
            .into_result()
            .unwrap();
        let comment = &annotation_trivia(&merged, "a").leading_comments[0];
        assert_eq!(comment.text, "same");
        assert_eq!(comment.span, Span::default());
    }

    #[test]
    fn comments_on_one_sided_additions_survive_and_deleted_clips_disappear() {
        let base = document(
            vec![layer(
                0,
                vec![annotation("deleted", trivia(&["old note"], None, 0))],
                Trivia::default(),
            )],
            &[],
        );
        let ours = document(Vec::new(), &[]);
        let mut theirs = base.clone();
        theirs.layers.push(layer(
            1,
            vec![annotation("added", Trivia::default())],
            Trivia::default(),
        ));
        let mut ours_with_addition = ours.clone();
        ours_with_addition.layers.push(layer(
            1,
            vec![
                annotation("added", trivia(&["new clip note"], Some("inline"), 20)),
                annotation("ours-only", trivia(&["one-sided addition"], None, 30)),
            ],
            Trivia::default(),
        ));
        // The identical semantic addition exists on both sides, but only ours
        // authored its comments. The deleted clip is absent from the result.
        let semantic = document(
            vec![layer(
                1,
                vec![
                    annotation("added", Trivia::default()),
                    annotation("ours-only", Trivia::default()),
                ],
                Trivia::default(),
            )],
            &[],
        );

        let merged = merge_document_trivia(&base, &ours_with_addition, &theirs, semantic)
            .into_result()
            .unwrap();
        assert!(merged.layers.iter().all(|layer| layer
            .annotations
            .iter()
            .all(|annotation| annotation.id.as_deref() != Some("deleted"))));
        assert_eq!(
            comment_texts(&annotation_trivia(&merged, "added").leading_comments),
            ["new clip note"]
        );
        assert_eq!(
            comment_texts(&annotation_trivia(&merged, "ours-only").leading_comments),
            ["one-sided addition"]
        );
    }

    #[test]
    fn comment_edit_against_clip_deletion_is_a_delete_modify_conflict() {
        let base = document(
            vec![layer(
                0,
                vec![annotation("clip", trivia(&["base"], None, 0))],
                Trivia::default(),
            )],
            &[],
        );
        let ours = document(Vec::new(), &[]);
        let mut theirs = base.clone();
        theirs.layers[0].annotations[0].trivia = trivia(&["edited"], None, 10);

        let outcome = merge_document_trivia(&base, &ours, &theirs, Document::default());
        assert!(outcome.merged.is_none());
        assert_eq!(outcome.conflicts.len(), 1);
        assert_eq!(
            outcome.conflicts[0].kind,
            TriviaMergeConflictKind::DeleteModify
        );
        assert_eq!(
            outcome.conflicts[0].path,
            TriviaMergePath(vec![
                TriviaMergePathSegment::Annotation("clip".to_owned()),
                TriviaMergePathSegment::Field(TriviaField::LeadingComments),
            ])
        );
        assert_eq!(outcome.conflicts[0].ours, TriviaMergeValue::Missing);
    }

    #[test]
    fn one_sided_layer_and_document_trailing_comments_merge() {
        let base = document(
            vec![layer(
                4,
                vec![annotation("clip", Trivia::default())],
                trivia(&["layer base"], None, 0),
            )],
            &["tail base"],
        );
        let mut ours = base.clone();
        ours.layers[0].trivia = trivia(&["layer ours"], Some("layer inline"), 10);
        let mut theirs = base.clone();
        theirs.trailing_comments = vec![c("tail theirs", 20)];
        let semantic = document(
            vec![layer(
                4,
                vec![annotation("clip", Trivia::default())],
                Trivia::default(),
            )],
            &[],
        );

        let merged = merge_document_trivia(&base, &ours, &theirs, semantic)
            .into_result()
            .unwrap();
        assert_eq!(
            comment_texts(&merged.layers[0].trivia.leading_comments),
            ["layer ours"]
        );
        assert_eq!(
            merged.layers[0]
                .trivia
                .trailing_comment
                .as_ref()
                .map(|comment| comment.text.as_str()),
            Some("layer inline")
        );
        assert_eq!(comment_texts(&merged.trailing_comments), ["tail theirs"]);
    }

    #[test]
    fn comment_only_layer_without_clips_is_preserved() {
        let base = Document::default();
        let ours = document(
            vec![layer(8, Vec::new(), trivia(&["future layer"], None, 0))],
            &[],
        );
        let theirs = Document::default();

        let merged = merge_document_trivia(&base, &ours, &theirs, Document::default())
            .into_result()
            .unwrap();
        assert_eq!(merged.layers.len(), 1);
        assert_eq!(merged.layers[0].z_index, 8);
        assert!(merged.layers[0].annotations.is_empty());
        assert_eq!(
            comment_texts(&merged.layers[0].trivia.leading_comments),
            ["future layer"]
        );
    }

    #[test]
    fn divergent_annotation_layer_and_document_edits_have_stable_paths() {
        let base = document(
            vec![layer(
                3,
                vec![annotation("clip", trivia(&["clip base"], None, 0))],
                trivia(&[], Some("layer base"), 1),
            )],
            &["document base"],
        );
        let mut ours = base.clone();
        ours.layers[0].annotations[0].trivia = trivia(&["clip ours"], None, 10);
        ours.layers[0].trivia = trivia(&[], Some("layer ours"), 11);
        ours.trailing_comments = vec![c("document ours", 12)];
        let mut theirs = base.clone();
        theirs.layers[0].annotations[0].trivia = trivia(&["clip theirs"], None, 20);
        theirs.layers[0].trivia = trivia(&[], Some("layer theirs"), 21);
        theirs.trailing_comments = vec![c("document theirs", 22)];

        let outcome = merge_document_trivia(&base, &ours, &theirs, base.clone());
        assert!(outcome.merged.is_none());
        assert_eq!(outcome.conflicts.len(), 3);
        assert_eq!(
            outcome
                .conflicts
                .iter()
                .map(|conflict| conflict.path.clone())
                .collect::<Vec<_>>(),
            vec![
                TriviaMergePath(vec![
                    TriviaMergePathSegment::Document,
                    TriviaMergePathSegment::Field(TriviaField::DocumentTrailingComments),
                ]),
                TriviaMergePath(vec![
                    TriviaMergePathSegment::Layer(3),
                    TriviaMergePathSegment::Field(TriviaField::TrailingComment),
                ]),
                TriviaMergePath(vec![
                    TriviaMergePathSegment::Annotation("clip".to_owned()),
                    TriviaMergePathSegment::Field(TriviaField::LeadingComments),
                ]),
            ]
        );
        assert!(outcome
            .conflicts
            .iter()
            .all(|conflict| conflict.kind == TriviaMergeConflictKind::ConcurrentEdit));
    }

    #[test]
    fn malformed_duplicate_or_unidentified_keys_are_structured_conflicts() {
        let duplicate = annotation("same", Trivia::default());
        let mut unidentified = annotation("temporary", Trivia::default());
        unidentified.id = None;
        let base = document(
            vec![layer(
                0,
                vec![duplicate.clone(), duplicate, unidentified],
                Trivia::default(),
            )],
            &[],
        );

        let outcome = merge_document_trivia(
            &base,
            &Document::default(),
            &Document::default(),
            Document::default(),
        );
        assert!(outcome.merged.is_none());
        assert_eq!(outcome.conflicts.len(), 2);
        assert_eq!(
            outcome
                .conflicts
                .iter()
                .map(|conflict| conflict.kind)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                TriviaMergeConflictKind::DuplicateKey,
                TriviaMergeConflictKind::InvalidInput,
            ])
        );
    }
}
