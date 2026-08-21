use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use serde_json::{json, Map, Number, Value};
use uuid::Uuid;

use crate::models::node_graph::{BeatGrid, PatternArgDef, PatternArgType};
use crate::models::patterns::PatternSummary;
use crate::services::track_edits::{TrackClip, TrackDocument};

use super::error::{DslError, DslWarning};
use super::parser::{parse, parse_canonical, ParseOptions, ParseResult};
use super::serializer::{
    serialize_canonical, serialize_group_expression, SerializeError, SerializeOptions,
};
use super::trivia_merge::{merge_document_trivia, TriviaMergeConflict};
use super::types::{
    Annotation, Arg, ArgValue, BarRange, Document, GroupExpr, Layer, PatternArgument,
    PatternDefinition, PatternRegistry, Span, TimeUnit, Trivia, UnavailablePattern,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompileErrorCode {
    MissingClipId,
    DuplicateClipId,
    MissingPatternId,
    UnknownPattern,
    AmbiguousPattern,
    DuplicatePatternArgId,
    AmbiguousArg,
    DuplicateArg,
    InvalidArgumentObject,
    InvalidTimeRange,
    MissingBeatGrid,
    InvalidSelection,
    NonCanonicalLayer,
    NonCanonicalTime,
    NonCanonicalSelection,
    NonCanonicalArgument,
}

impl CompileErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingClipId => "missing_clip_id",
            Self::DuplicateClipId => "duplicate_clip_id",
            Self::MissingPatternId => "missing_pattern_id",
            Self::UnknownPattern => "unknown_pattern",
            Self::AmbiguousPattern => "ambiguous_pattern",
            Self::DuplicatePatternArgId => "duplicate_pattern_arg_id",
            Self::AmbiguousArg => "ambiguous_arg",
            Self::DuplicateArg => "duplicate_arg",
            Self::InvalidArgumentObject => "invalid_argument_object",
            Self::InvalidTimeRange => "invalid_time_range",
            Self::MissingBeatGrid => "missing_beat_grid",
            Self::InvalidSelection => "invalid_selection",
            Self::NonCanonicalLayer => "non_canonical_layer",
            Self::NonCanonicalTime => "non_canonical_time",
            Self::NonCanonicalSelection => "non_canonical_selection",
            Self::NonCanonicalArgument => "non_canonical_argument",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    pub code: CompileErrorCode,
    pub message: String,
    pub span: Option<Span>,
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for CompileError {}

#[derive(Debug)]
pub enum SourceCompileError {
    Syntax(Vec<DslError>),
    Semantic(CompileError),
}

impl fmt::Display for SourceCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(errors) => match errors.as_slice() {
                [] => formatter.write_str("score contains an unreported syntax error"),
                [error] => error.fmt(formatter),
                [first, ..] => write!(
                    formatter,
                    "score contains {} syntax errors; first: {first}",
                    errors.len()
                ),
            },
            Self::Semantic(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SourceCompileError {}

#[derive(Debug)]
pub enum ImportTrackDocumentError {
    Source(SourceCompileError),
    Serialize(SerializeError),
    Trivia(Vec<TriviaMergeConflict>),
}

impl fmt::Display for ImportTrackDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(error) => error.fmt(formatter),
            Self::Serialize(error) => error.fmt(formatter),
            Self::Trivia(conflicts) => write!(
                formatter,
                "score comments contain {} structured conflict(s)",
                conflicts.len()
            ),
        }
    }
}

impl std::error::Error for ImportTrackDocumentError {}

/// One fully compiled score import. `host_allocated_ids` is capability-like
/// provenance: only these stable UUIDs may be new when the document is
/// projected. Every ID supplied by the source must already belong to the exact
/// score being edited.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledTrackImport {
    pub document: TrackDocument,
    pub canonical_source: String,
    pub warnings: Vec<DslWarning>,
    pub host_allocated_ids: BTreeSet<String>,
}

/// Presentation names carried beside stable pattern IDs in canonical source.
/// Names make `score.luma` readable, but never participate in score semantics.
pub type PatternNames = BTreeMap<String, String>;

pub(crate) fn build_registry_with_unavailable(
    patterns: &[PatternSummary],
    pattern_args: &HashMap<String, Vec<PatternArgDef>>,
    unavailable: HashMap<String, String>,
) -> PatternRegistry {
    let definitions = patterns
        .iter()
        .filter(|pattern| !unavailable.contains_key(&pattern.id))
        .map(|pattern| {
            let args = pattern_args
                .get(&pattern.id)
                .into_iter()
                .flatten()
                .map(|argument| PatternArgument {
                    id: argument.id.clone(),
                    name: argument.name.clone(),
                    arg_type: argument.arg_type.clone(),
                    default_value: argument.default_value.clone(),
                })
                .collect();
            PatternDefinition {
                id: Some(pattern.id.clone()),
                name: pattern.name.clone(),
                args,
            }
        })
        .collect();
    let unavailable_by_id = patterns
        .iter()
        .filter_map(|pattern| {
            unavailable.get(&pattern.id).map(|reason| {
                (
                    pattern.id.clone(),
                    UnavailablePattern {
                        name: pattern.name.clone(),
                        reason: reason.clone(),
                    },
                )
            })
        })
        .collect();
    PatternRegistry::with_unavailable(definitions, unavailable_by_id)
}

pub fn clips_to_document(
    clips: &[TrackClip],
    beat_grid: &BeatGrid,
    registry: &PatternRegistry,
) -> Result<Document, CompileError> {
    validate_unique_clip_ids(clips)?;
    let mut layers: BTreeMap<i64, Vec<Annotation>> = BTreeMap::new();
    for clip in clips {
        if !clip.start_time.is_finite()
            || !clip.end_time.is_finite()
            || clip.end_time <= clip.start_time
        {
            return Err(compile_error(
                CompileErrorCode::InvalidTimeRange,
                format!(
                    "cannot export clip {:?}: invalid time range {}–{}",
                    clip.id, clip.start_time, clip.end_time
                ),
                None,
            ));
        }
        let pattern = registry.by_id(&clip.pattern_id).ok_or_else(|| {
            compile_error(
                CompileErrorCode::UnknownPattern,
                format!(
                    "cannot export score: pattern {:?} is unavailable",
                    clip.pattern_id
                ),
                None,
            )
        })?;
        let raw_args = clip.args.as_object().ok_or_else(|| {
            compile_error(
                CompileErrorCode::InvalidArgumentObject,
                format!(
                    "cannot export clip {:?}: args must be a JSON object",
                    clip.id
                ),
                None,
            )
        })?;

        let mut selection = None;
        let mut selection_spatial_reference = None;
        let mut represented_selection_key = None;
        for definition in &pattern.args {
            if definition.arg_type != PatternArgType::Selection {
                continue;
            }
            let Some(value) = raw_args.get(&definition.id) else {
                continue;
            };
            if let Some((expression, spatial_reference)) = canonical_selection(value) {
                selection = Some(expression);
                selection_spatial_reference = Some(spatial_reference);
                represented_selection_key = Some(definition.id.as_str());
                break;
            }
        }
        if !pattern
            .args
            .iter()
            .any(|definition| definition.arg_type == PatternArgType::Selection)
        {
            selection = Some(GroupExpr::Group {
                name: "all".to_owned(),
            });
        }

        let mut args = Vec::with_capacity(raw_args.len());
        for (key, value) in raw_args {
            if represented_selection_key == Some(key.as_str()) {
                continue;
            }
            let definition = pattern.args.iter().find(|definition| definition.id == *key);
            args.push(Arg {
                key: key.clone(),
                value: value_to_argument(definition.map(|value| &value.arg_type), value),
                span: Span::default(),
            });
        }

        let start_bar = exact_bar_position(clip.start_time, beat_grid, 4);
        let end_bar = exact_bar_position(clip.end_time, beat_grid, 4);
        let range = match (start_bar, end_bar) {
            (Some(start), Some(end)) => BarRange {
                start,
                end,
                unit: TimeUnit::Bars,
            },
            _ => BarRange {
                start: clip.start_time,
                end: clip.end_time,
                unit: TimeUnit::Seconds,
            },
        };
        layers.entry(clip.z_index).or_default().push(Annotation {
            id: Some(clip.id.clone()),
            pattern: pattern.name.clone(),
            pattern_id: Some(clip.pattern_id.clone()),
            selection,
            selection_spatial_reference,
            range,
            args,
            blend: clip.blend_mode,
            span: Span::default(),
            trivia: Trivia::default(),
        });
    }

    Ok(Document {
        layers: layers
            .into_iter()
            .map(|(z_index, mut annotations)| {
                annotations.sort_by(|left, right| {
                    left.range
                        .start
                        .total_cmp(&right.range.start)
                        .then_with(|| left.id.cmp(&right.id))
                });
                Layer {
                    z_index,
                    explicit_z: true,
                    annotations,
                    trivia: Trivia::default(),
                }
            })
            .collect(),
        trailing_comments: Vec::new(),
    })
}

/// Build the stable authored AST. Unlike the human presentation form above, this
/// representation is deliberately context-free: time is exact seconds and
/// every argument remains an explicit JSON value under its stable key. Pattern
/// names are display-only labels paired with the IDs that carry identity.
pub fn clips_to_canonical_document(
    clips: &[TrackClip],
    pattern_names: &PatternNames,
) -> Result<Document, CompileError> {
    validate_unique_clip_ids(clips)?;
    let mut layers: BTreeMap<i64, Vec<Annotation>> = BTreeMap::new();
    for clip in clips {
        if !clip.start_time.is_finite()
            || !clip.end_time.is_finite()
            || clip.end_time <= clip.start_time
        {
            return Err(compile_error(
                CompileErrorCode::InvalidTimeRange,
                format!(
                    "cannot export clip {:?}: invalid time range {}–{}",
                    clip.id, clip.start_time, clip.end_time
                ),
                None,
            ));
        }
        let pattern = pattern_names.get(&clip.pattern_id).ok_or_else(|| {
            compile_error(
                CompileErrorCode::UnknownPattern,
                format!(
                    "cannot export score: pattern {:?} has no display name",
                    clip.pattern_id
                ),
                None,
            )
        })?;
        let raw_args = clip.args.as_object().ok_or_else(|| {
            compile_error(
                CompileErrorCode::InvalidArgumentObject,
                format!(
                    "cannot export clip {:?}: args must be a JSON object",
                    clip.id
                ),
                None,
            )
        })?;
        let args = raw_args
            .iter()
            .map(|(key, value)| Arg {
                key: key.clone(),
                value: ArgValue::Json(value.clone()),
                span: Span::default(),
            })
            .collect();
        layers.entry(clip.z_index).or_default().push(Annotation {
            id: Some(clip.id.clone()),
            pattern: pattern.clone(),
            pattern_id: Some(clip.pattern_id.clone()),
            selection: None,
            selection_spatial_reference: None,
            range: BarRange {
                start: clip.start_time,
                end: clip.end_time,
                unit: TimeUnit::Seconds,
            },
            args,
            blend: clip.blend_mode,
            span: Span::default(),
            trivia: Trivia::default(),
        });
    }

    Ok(Document {
        layers: layers
            .into_iter()
            .map(|(z_index, mut annotations)| {
                annotations.sort_by(|left, right| {
                    left.range
                        .start
                        .total_cmp(&right.range.start)
                        .then_with(|| left.id.cmp(&right.id))
                });
                Layer {
                    z_index,
                    explicit_z: true,
                    annotations,
                    trivia: Trivia::default(),
                }
            })
            .collect(),
        trailing_comments: Vec::new(),
    })
}

pub fn pattern_names_from_document(document: &Document) -> PatternNames {
    document
        .layers
        .iter()
        .flat_map(|layer| &layer.annotations)
        .filter_map(|annotation| {
            annotation
                .pattern_id
                .as_ref()
                .map(|id| (id.clone(), annotation.pattern.clone()))
        })
        .collect()
}

fn validate_unique_clip_ids(clips: &[TrackClip]) -> Result<(), CompileError> {
    let mut ids = HashSet::new();
    for clip in clips {
        if !ids.insert(clip.id.as_str()) {
            return Err(compile_error(
                CompileErrorCode::DuplicateClipId,
                format!("cannot export score with duplicate clip id {:?}", clip.id),
                None,
            ));
        }
    }
    Ok(())
}

pub fn document_to_clips(
    document: &Document,
    beat_grid: &BeatGrid,
    registry: &PatternRegistry,
) -> Result<Vec<TrackClip>, CompileError> {
    document_to_clips_with_identity(document, beat_grid, registry, MissingIdPolicy::Reject)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MissingIdPolicy {
    Reject,
    Draft,
}

fn document_to_clips_with_identity(
    document: &Document,
    beat_grid: &BeatGrid,
    registry: &PatternRegistry,
    missing_id_policy: MissingIdPolicy,
) -> Result<Vec<TrackClip>, CompileError> {
    let mut clips = Vec::new();
    let mut reserved_ids: std::collections::HashSet<String> = document
        .layers
        .iter()
        .flat_map(|layer| &layer.annotations)
        .filter_map(|annotation| annotation.id.clone())
        .collect();
    let mut next_draft_id = 0usize;
    for layer in &document.layers {
        for annotation in &layer.annotations {
            let id = match (&annotation.id, missing_id_policy) {
                (Some(id), _) => id.clone(),
                (None, MissingIdPolicy::Reject) => {
                    return Err(compile_error(
                        CompileErrorCode::MissingClipId,
                        format!(
                            "cannot compile clip for pattern {:?} without a stable clip id",
                            annotation.pattern
                        ),
                        Some(annotation.span),
                    ));
                }
                (None, MissingIdPolicy::Draft) => loop {
                    let candidate = format!("new:dsl-{next_draft_id}");
                    next_draft_id += 1;
                    if reserved_ids.insert(candidate.clone()) {
                        break candidate;
                    }
                },
            };
            let pattern = resolve_pattern(annotation, registry)?;
            let args = compile_arguments(annotation, pattern)?;
            let (start_time, end_time) = match annotation.range.unit {
                TimeUnit::Seconds => (annotation.range.start, annotation.range.end),
                TimeUnit::Bars if beat_grid.downbeats.is_empty() => {
                    return Err(compile_error(
                        CompileErrorCode::MissingBeatGrid,
                        format!("cannot compile bar-timed clip {id:?} without a beat grid"),
                        Some(annotation.span),
                    ));
                }
                TimeUnit::Bars => (
                    bar_to_time(annotation.range.start, beat_grid),
                    bar_to_time(annotation.range.end, beat_grid),
                ),
            };
            if !start_time.is_finite() || !end_time.is_finite() || end_time <= start_time {
                return Err(compile_error(
                    CompileErrorCode::InvalidTimeRange,
                    format!("clip {id:?} has invalid time range {start_time}–{end_time}"),
                    Some(annotation.span),
                ));
            }
            clips.push(TrackClip {
                id,
                pattern_id: pattern
                    .id
                    .clone()
                    .expect("resolved installed pattern has an id"),
                start_time,
                end_time,
                z_index: layer.z_index,
                blend_mode: annotation.blend,
                args: Value::Object(args),
            });
        }
    }
    clips.sort_by(|left, right| {
        left.start_time
            .total_cmp(&right.start_time)
            .then(left.z_index.cmp(&right.z_index))
            .then(left.id.cmp(&right.id))
    });
    Ok(clips)
}

fn canonical_document_to_clips(document: &Document) -> Result<Vec<TrackClip>, CompileError> {
    let mut clips = Vec::new();
    let mut clip_ids = HashSet::new();
    for layer in &document.layers {
        if !layer.explicit_z {
            return Err(compile_error(
                CompileErrorCode::NonCanonicalLayer,
                "canonical score layers must declare their exact z index".to_owned(),
                layer.annotations.first().map(|annotation| annotation.span),
            ));
        }
        for annotation in &layer.annotations {
            let id = annotation.id.clone().ok_or_else(|| {
                compile_error(
                    CompileErrorCode::MissingClipId,
                    format!(
                        "canonical clip for pattern {:?} is missing its stable clip id",
                        annotation.pattern
                    ),
                    Some(annotation.span),
                )
            })?;
            if !clip_ids.insert(id.clone()) {
                return Err(compile_error(
                    CompileErrorCode::DuplicateClipId,
                    format!("canonical score contains duplicate clip id {id:?}"),
                    Some(annotation.span),
                ));
            }
            let pattern_id = annotation.pattern_id.clone().ok_or_else(|| {
                compile_error(
                    CompileErrorCode::MissingPatternId,
                    format!("canonical clip {id:?} is missing its stable pattern id"),
                    Some(annotation.span),
                )
            })?;
            if annotation.selection.is_some() || annotation.selection_spatial_reference.is_some() {
                return Err(compile_error(
                    CompileErrorCode::NonCanonicalSelection,
                    format!(
                        "canonical clip {id:?} must store Selection values as explicit raw arguments"
                    ),
                    Some(annotation.span),
                ));
            }
            if annotation.range.unit != TimeUnit::Seconds {
                return Err(compile_error(
                    CompileErrorCode::NonCanonicalTime,
                    format!("canonical clip {id:?} must use exact seconds"),
                    Some(annotation.span),
                ));
            }
            let start_time = annotation.range.start;
            let end_time = annotation.range.end;
            if !start_time.is_finite() || !end_time.is_finite() || end_time <= start_time {
                return Err(compile_error(
                    CompileErrorCode::InvalidTimeRange,
                    format!("clip {id:?} has invalid time range {start_time}–{end_time}"),
                    Some(annotation.span),
                ));
            }
            let mut args = Map::new();
            for argument in &annotation.args {
                if !matches!(argument.value, ArgValue::Json(_)) {
                    return Err(compile_error(
                        CompileErrorCode::NonCanonicalArgument,
                        format!(
                            "canonical clip {id:?} arg {:?} must use an explicit JSON value",
                            argument.key
                        ),
                        Some(argument.span),
                    ));
                }
                if args
                    .insert(
                        argument.key.clone(),
                        argument_to_value(&argument.value, None),
                    )
                    .is_some()
                {
                    return Err(compile_error(
                        CompileErrorCode::DuplicateArg,
                        format!(
                            "cannot compile canonical clip {id:?}: arg {:?} is assigned more than once",
                            argument.key
                        ),
                        Some(argument.span),
                    ));
                }
            }
            clips.push(TrackClip {
                id,
                pattern_id,
                start_time,
                end_time,
                z_index: layer.z_index,
                blend_mode: annotation.blend,
                args: Value::Object(args),
            });
        }
    }
    clips.sort_by(|left, right| {
        left.start_time
            .total_cmp(&right.start_time)
            .then(left.z_index.cmp(&right.z_index))
            .then(left.id.cmp(&right.id))
    });
    Ok(clips)
}

/// Decode one canonical authored blob into its complete semantic score without any
/// database, beat-analysis, or pattern-interface input.
pub fn decode_canonical_track_document(
    source: &str,
) -> Result<(TrackDocument, Vec<DslWarning>), SourceCompileError> {
    let (document, warnings) = match parse_canonical(source) {
        ParseResult::Success { document, warnings } => (document, warnings),
        ParseResult::Failure { errors, .. } => return Err(SourceCompileError::Syntax(errors)),
    };
    let clips = canonical_document_to_clips(&document).map_err(SourceCompileError::Semantic)?;
    Ok((
        TrackDocument {
            revision: crate::services::track_edits::revision_for_clips(&clips),
            clips,
        },
        warnings,
    ))
}

pub fn track_document_to_canonical_dsl(
    track: &TrackDocument,
    pattern_names: &PatternNames,
) -> Result<String, TrackDocumentSerializeError> {
    let document = clips_to_canonical_document(&track.clips, pattern_names)
        .map_err(TrackDocumentSerializeError::Compile)?;
    serialize_canonical(&document).map_err(TrackDocumentSerializeError::Serialize)
}

/// Deterministic model-example source optimized for musical authoring. Unlike
/// the self-contained authored form, it may use bar and typed argument sugar because
/// it is compiled immediately against the current score context.
pub fn track_document_to_exemplar_dsl(
    track: &TrackDocument,
    beat_grid: &BeatGrid,
    registry: &PatternRegistry,
) -> Result<String, TrackDocumentSerializeError> {
    let document = clips_to_document(&track.clips, beat_grid, registry)
        .map_err(TrackDocumentSerializeError::Compile)?;
    super::serializer::serialize_exemplar(
        &document,
        registry,
        SerializeOptions {
            beats_per_bar: beat_grid.beats_per_bar.max(1) as u32,
            subdivisions_per_beat: 4,
        },
    )
    .map_err(TrackDocumentSerializeError::Serialize)
}

#[derive(Debug)]
pub enum TrackDocumentSerializeError {
    Compile(CompileError),
    Serialize(SerializeError),
}

impl fmt::Display for TrackDocumentSerializeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compile(error) => error.fmt(formatter),
            Self::Serialize(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TrackDocumentSerializeError {}

/// Compile human/model-authored source for validation or import. Omitted clip
/// identities become reserved draft correlation IDs; the authoritative track
/// transaction allocates durable UUIDs. Supplied identities are left intact so
/// that transaction can prove they already belong to the exact score.
pub fn compile_draft_track_document(
    source: &str,
    revision: String,
    beat_grid: &BeatGrid,
    registry: &PatternRegistry,
) -> Result<(TrackDocument, Vec<DslWarning>), SourceCompileError> {
    let parsed = parse(
        source,
        registry,
        ParseOptions {
            beats_per_bar: beat_grid.beats_per_bar.max(1) as u32,
            subdivisions_per_beat: 4,
        },
    );
    let (document, warnings) = match parsed {
        ParseResult::Success { document, warnings } => (document, warnings),
        ParseResult::Failure { errors, .. } => return Err(SourceCompileError::Syntax(errors)),
    };
    let clips =
        document_to_clips_with_identity(&document, beat_grid, registry, MissingIdPolicy::Draft)
            .map_err(SourceCompileError::Semantic)?;
    Ok((TrackDocument { revision, clips }, warnings))
}

/// Compile source for an authoritative authored import and emit the canonical file
/// from the very same AST. Human/model source may omit clip IDs; when enabled,
/// Rust allocates those identities before serialization so the revision and
/// relational projection can never disagree about row identity.
pub fn compile_import_track_document(
    source: &str,
    context: &super::workflow::ScoreDslContext,
    allocate_missing_ids: bool,
) -> Result<CompiledTrackImport, ImportTrackDocumentError> {
    let options = ParseOptions {
        beats_per_bar: context.beat_grid.beats_per_bar.max(1) as u32,
        subdivisions_per_beat: 4,
    };
    let parsed = parse(source, &context.registry, options);
    let (mut document, warnings) = match parsed {
        ParseResult::Success { document, warnings } => (document, warnings),
        ParseResult::Failure { errors, .. } => {
            return Err(ImportTrackDocumentError::Source(
                SourceCompileError::Syntax(errors),
            ));
        }
    };
    let mut host_allocated_ids = BTreeSet::new();
    if allocate_missing_ids {
        for annotation in document
            .layers
            .iter_mut()
            .flat_map(|layer| layer.annotations.iter_mut())
        {
            if annotation.id.is_none() {
                let id = Uuid::new_v4().to_string();
                host_allocated_ids.insert(id.clone());
                annotation.id = Some(id);
            }
        }
    }
    let clips = document_to_clips(&document, &context.beat_grid, &context.registry)
        .map_err(|error| ImportTrackDocumentError::Source(SourceCompileError::Semantic(error)))?;
    let pattern_names = context
        .registry
        .definitions()
        .iter()
        .filter_map(|pattern| {
            pattern
                .id
                .as_ref()
                .map(|id| (id.clone(), pattern.name.clone()))
        })
        .collect();
    let semantic = clips_to_canonical_document(&clips, &pattern_names)
        .map_err(|error| ImportTrackDocumentError::Source(SourceCompileError::Semantic(error)))?;
    let canonical = merge_document_trivia(&document, &document, &document, semantic)
        .into_result()
        .map_err(ImportTrackDocumentError::Trivia)?;
    let canonical_source =
        serialize_canonical(&canonical).map_err(ImportTrackDocumentError::Serialize)?;
    Ok(CompiledTrackImport {
        document: TrackDocument {
            revision: crate::services::track_edits::revision_for_clips(&clips),
            clips,
        },
        canonical_source,
        warnings,
        host_allocated_ids,
    })
}

fn resolve_pattern<'a>(
    annotation: &Annotation,
    registry: &'a PatternRegistry,
) -> Result<&'a PatternDefinition, CompileError> {
    if let Some(pattern_id) = &annotation.pattern_id {
        return registry.by_id(pattern_id).ok_or_else(|| {
            compile_error(
                CompileErrorCode::UnknownPattern,
                format!("cannot compile clip: pattern id {pattern_id:?} is unavailable"),
                Some(annotation.span),
            )
        });
    }
    let matches = registry.by_name(&annotation.pattern);
    match matches.as_slice() {
        [] => Err(compile_error(
            CompileErrorCode::UnknownPattern,
            format!(
                "cannot compile clip: pattern {:?} is unavailable",
                annotation.pattern
            ),
            Some(annotation.span),
        )),
        [pattern] => Ok(*pattern),
        _ => Err(compile_error(
            CompileErrorCode::AmbiguousPattern,
            format!(
                "cannot compile clip: pattern name {:?} is ambiguous",
                annotation.pattern
            ),
            Some(annotation.span),
        )),
    }
}

fn compile_arguments(
    annotation: &Annotation,
    pattern: &PatternDefinition,
) -> Result<Map<String, Value>, CompileError> {
    let mut definitions_by_id = HashMap::new();
    for definition in &pattern.args {
        if definitions_by_id
            .insert(definition.id.as_str(), definition)
            .is_some()
        {
            return Err(compile_error(
                CompileErrorCode::DuplicatePatternArgId,
                format!(
                    "cannot compile clip {:?}: pattern interface contains duplicate arg id {:?}",
                    annotation.id.as_deref().unwrap_or(&annotation.pattern),
                    definition.id
                ),
                Some(annotation.span),
            ));
        }
    }

    let mut args = Map::new();
    for argument in &annotation.args {
        let definition = resolve_argument(annotation, &argument.key, pattern, &definitions_by_id)?;
        let key = definition.map_or(argument.key.as_str(), |definition| definition.id.as_str());
        if args.contains_key(key) {
            return Err(compile_error(
                CompileErrorCode::DuplicateArg,
                format!(
                    "cannot compile clip {:?}: arg {key:?} is assigned more than once",
                    annotation.id.as_deref().unwrap_or(&annotation.pattern)
                ),
                Some(argument.span),
            ));
        }
        args.insert(
            key.to_owned(),
            argument_to_value(&argument.value, definition.map(|value| &value.arg_type)),
        );
    }

    if let Some(selection) = &annotation.selection {
        if let Some(definition) = pattern.args.iter().find(|definition| {
            definition.arg_type == PatternArgType::Selection && !args.contains_key(&definition.id)
        }) {
            args.insert(
                definition.id.clone(),
                json!({
                    "expression": serialize_group_expression(selection),
                    "spatialReference": annotation
                        .selection_spatial_reference
                        .as_deref()
                        .unwrap_or("global"),
                }),
            );
        }
    }
    Ok(args)
}

fn resolve_argument<'a>(
    annotation: &Annotation,
    key: &str,
    pattern: &'a PatternDefinition,
    definitions_by_id: &HashMap<&str, &'a PatternArgument>,
) -> Result<Option<&'a PatternArgument>, CompileError> {
    if let Some(exact) = definitions_by_id.get(key) {
        return Ok(Some(*exact));
    }
    let aliases: Vec<&PatternArgument> = pattern
        .args
        .iter()
        .filter(|definition| definition.name == key)
        .collect();
    if aliases.len() > 1 {
        let ids = aliases
            .iter()
            .map(|definition| format!("{:?}", definition.id))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(compile_error(
            CompileErrorCode::AmbiguousArg,
            format!(
                "cannot compile clip {:?}: arg name {key:?} is ambiguous; use a stable arg id ({ids})",
                annotation.id.as_deref().unwrap_or(&annotation.pattern)
            ),
            Some(annotation.span),
        ));
    }
    Ok(aliases.into_iter().next())
}

fn value_to_argument(arg_type: Option<&PatternArgType>, value: &Value) -> ArgValue {
    match arg_type {
        Some(PatternArgType::Color) if exactly_hex_representable_color(value) => {
            let object = value.as_object().expect("validated color is an object");
            let r = object["r"].as_f64().expect("validated channel");
            let g = object["g"].as_f64().expect("validated channel");
            let b = object["b"].as_f64().expect("validated channel");
            let a = object["a"].as_f64().expect("validated channel");
            ArgValue::Color(rgba_to_hex(r, g, b, a))
        }
        Some(PatternArgType::Scalar) if scalar_sugar_is_lossless(value) => {
            ArgValue::Number(value.as_f64().expect("validated scalar"))
        }
        _ => ArgValue::Json(value.clone()),
    }
}

fn scalar_sugar_is_lossless(value: &Value) -> bool {
    let Some(number) = value.as_f64().filter(|value| value.is_finite()) else {
        return false;
    };
    serde_json::to_string(value).ok().as_deref() == Some(number.to_string().as_str())
}

fn argument_to_value(value: &ArgValue, arg_type: Option<&PatternArgType>) -> Value {
    match (value, arg_type) {
        (ArgValue::Color(value), Some(PatternArgType::Color)) => hex_to_rgba(value),
        (ArgValue::Number(value), Some(PatternArgType::Scalar)) => number_value(*value),
        (ArgValue::Json(value), _) => value.clone(),
        (ArgValue::Number(value), _) => number_value(*value),
        (ArgValue::Color(value), _) | (ArgValue::Identifier(value), _) => {
            Value::String(value.clone())
        }
    }
}

fn number_value(value: f64) -> Value {
    if value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
        Value::Number(Number::from(value as i64))
    } else {
        Value::Number(
            Number::from_f64(value).expect("parser and exporter reject non-finite values"),
        )
    }
}

fn canonical_selection(value: &Value) -> Option<(GroupExpr, String)> {
    let object = value.as_object()?;
    if object.len() != 2
        || !object.contains_key("expression")
        || !object.contains_key("spatialReference")
    {
        return None;
    }
    let expression = object.get("expression")?.as_str()?;
    let spatial_reference = object.get("spatialReference")?.as_str()?;
    if expression.trim().is_empty()
        || !expression.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '_' | '~' | '&' | '|' | '^' | '>' | '(' | ')')
                || character.is_whitespace()
        })
        || !is_identifier(spatial_reference)
    {
        return None;
    }
    let parsed = parse_group_expression(expression).ok()?;
    if serialize_group_expression(&parsed) != expression {
        return None;
    }
    Some((parsed, spatial_reference.to_owned()))
}

pub fn parse_group_expression(source: &str) -> Result<GroupExpr, CompileError> {
    SelectionParser::new(source).parse()
}

struct SelectionParser<'a> {
    source: &'a str,
    position: usize,
}

impl<'a> SelectionParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            position: 0,
        }
    }

    fn parse(mut self) -> Result<GroupExpr, CompileError> {
        let result = self.fallback()?;
        self.whitespace();
        if self.position != self.source.len() {
            return Err(self.error(format!(
                "unexpected selection syntax at offset {}",
                self.position
            )));
        }
        Ok(result)
    }

    fn fallback(&mut self) -> Result<GroupExpr, CompileError> {
        let mut left = self.or()?;
        self.whitespace();
        while self.consume('>') {
            self.whitespace();
            left = GroupExpr::Fallback {
                left: Box::new(left),
                right: Box::new(self.or()?),
            };
            self.whitespace();
        }
        Ok(left)
    }

    fn or(&mut self) -> Result<GroupExpr, CompileError> {
        let mut left = self.xor()?;
        self.whitespace();
        while self.consume('|') {
            self.whitespace();
            left = GroupExpr::Or {
                left: Box::new(left),
                right: Box::new(self.xor()?),
            };
            self.whitespace();
        }
        Ok(left)
    }

    fn xor(&mut self) -> Result<GroupExpr, CompileError> {
        let mut left = self.and()?;
        self.whitespace();
        while self.consume('^') {
            self.whitespace();
            left = GroupExpr::Xor {
                left: Box::new(left),
                right: Box::new(self.and()?),
            };
            self.whitespace();
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<GroupExpr, CompileError> {
        let mut left = self.unary()?;
        self.whitespace();
        while self.consume('&') {
            self.whitespace();
            left = GroupExpr::And {
                left: Box::new(left),
                right: Box::new(self.unary()?),
            };
            self.whitespace();
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<GroupExpr, CompileError> {
        self.whitespace();
        if self.consume('~') {
            return Ok(GroupExpr::Not {
                operand: Box::new(self.unary()?),
            });
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<GroupExpr, CompileError> {
        self.whitespace();
        if self.consume('(') {
            let inner = self.fallback()?;
            self.whitespace();
            if !self.consume(')') {
                return Err(self.error("unclosed group expression".to_owned()));
            }
            return Ok(GroupExpr::Paren {
                inner: Box::new(inner),
            });
        }
        let start = self.position;
        while self
            .peek()
            .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            self.advance();
        }
        let name = &self.source[start..self.position];
        if !is_identifier(name) {
            return Err(self.error("expected group name".to_owned()));
        }
        Ok(GroupExpr::Group {
            name: name.to_owned(),
        })
    }

    fn whitespace(&mut self) {
        while self.peek() == Some(' ') {
            self.advance();
        }
    }

    fn peek(&self) -> Option<char> {
        self.source.get(self.position..)?.chars().next()
    }

    fn advance(&mut self) {
        if let Some(character) = self.peek() {
            self.position += character.len_utf8();
        }
    }

    fn consume(&mut self, expected: char) -> bool {
        if self.peek() != Some(expected) {
            return false;
        }
        self.advance();
        true
    }

    fn error(&self, message: String) -> CompileError {
        compile_error(CompileErrorCode::InvalidSelection, message, None)
    }
}

fn time_to_bar(time: f64, beat_grid: &BeatGrid) -> f64 {
    if beat_grid.downbeats.is_empty() {
        return 1.0;
    }
    let fallback_duration = (60.0 / f64::from(beat_grid.bpm)) * f64::from(beat_grid.beats_per_bar);
    let first = f64::from(beat_grid.downbeats[0]);
    if time < first - 1e-6 {
        let duration = beat_grid
            .downbeats
            .get(1)
            .map_or(fallback_duration, |next| f64::from(*next) - first);
        return 1.0 - (first - time) / duration;
    }
    let mut bar_index = 0;
    for index in (0..beat_grid.downbeats.len()).rev() {
        if time >= f64::from(beat_grid.downbeats[index]) - 1e-6 {
            bar_index = index;
            break;
        }
    }
    let start = f64::from(beat_grid.downbeats[bar_index]);
    let end = beat_grid
        .downbeats
        .get(bar_index + 1)
        .map_or(start + fallback_duration, |next| f64::from(*next));
    let duration = end - start;
    if duration <= 0.0 {
        return bar_index as f64 + 1.0;
    }
    bar_index as f64 + 1.0 + (time - start) / duration
}

fn bar_to_time(bar: f64, beat_grid: &BeatGrid) -> f64 {
    let total_bars = beat_grid.downbeats.len();
    let whole_bar = bar.floor() as i64;
    let fraction = bar - whole_bar as f64;
    let index = whole_bar - 1;
    let fallback_duration = (60.0 / f64::from(beat_grid.bpm)) * f64::from(beat_grid.beats_per_bar);
    let duration = if total_bars >= 2 {
        f64::from(beat_grid.downbeats[total_bars - 1])
            - f64::from(beat_grid.downbeats[total_bars - 2])
    } else {
        fallback_duration
    };
    let start = if index < 0 {
        f64::from(beat_grid.downbeats[0]) + index as f64 * duration
    } else if (index as usize) < total_bars {
        f64::from(beat_grid.downbeats[index as usize])
    } else {
        f64::from(beat_grid.downbeats[total_bars - 1])
            + (index as usize - (total_bars - 1)) as f64 * duration
    };
    if fraction == 0.0 {
        return start;
    }
    let next_index = whole_bar;
    let end = if next_index >= 0 && (next_index as usize) < total_bars {
        f64::from(beat_grid.downbeats[next_index as usize])
    } else {
        start + duration
    };
    start + fraction * (end - start)
}

fn exact_bar_position(time: f64, beat_grid: &BeatGrid, subdivisions_per_beat: u32) -> Option<f64> {
    if beat_grid.downbeats.is_empty() {
        return None;
    }
    let raw = time_to_bar(time, beat_grid);
    let beats_per_bar = beat_grid.beats_per_bar.max(1) as u32;
    let positions_per_bar = f64::from(beats_per_bar * subdivisions_per_beat);
    let snapped = (raw * positions_per_bar).round() / positions_per_bar;
    let bar = snapped.floor();
    let subdivision_index = ((snapped - bar) * positions_per_bar).round() as u32;
    let beat = subdivision_index / subdivisions_per_beat + 1;
    let subdivision = subdivision_index % subdivisions_per_beat + 1;
    let represented = bar
        + f64::from(beat - 1) / f64::from(beats_per_bar)
        + f64::from(subdivision - 1) / positions_per_bar;
    (bar_to_time(represented, beat_grid) == time).then_some(represented)
}

fn exactly_hex_representable_color(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.len() != 4
        || !["r", "g", "b", "a"]
            .iter()
            .all(|key| object.contains_key(*key))
    {
        return false;
    }
    let channels = ["r", "g", "b"].map(|key| object[key].as_f64());
    if channels.iter().any(|value| {
        value.is_none_or(|value| {
            !value.is_finite() || value.fract() != 0.0 || !(0.0..=255.0).contains(&value)
        })
    }) {
        return false;
    }
    let Some(alpha) = object["a"].as_f64() else {
        return false;
    };
    if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
        return false;
    }
    let encoded = rgba_to_hex(
        channels[0].unwrap(),
        channels[1].unwrap(),
        channels[2].unwrap(),
        alpha,
    );
    let decoded = hex_to_rgba(&encoded);
    let decoded = decoded.as_object().expect("decoded color is an object");
    decoded["r"].as_f64() == channels[0]
        && decoded["g"].as_f64() == channels[1]
        && decoded["b"].as_f64() == channels[2]
        && decoded["a"].as_f64() == Some(alpha)
}

fn rgba_to_hex(r: f64, g: f64, b: f64, alpha: f64) -> String {
    let byte = |value: f64| value.clamp(0.0, 255.0).round() as u8;
    if (alpha - 1.0).abs() <= 1e-6 {
        format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b))
    } else {
        format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            byte(r),
            byte(g),
            byte(b),
            byte(alpha.clamp(0.0, 1.0) * 255.0)
        )
    }
}

fn hex_to_rgba(value: &str) -> Value {
    let clean = value.strip_prefix('#').unwrap_or(value);
    let channel = |start: usize| u8::from_str_radix(&clean[start..start + 2], 16).unwrap_or(0);
    let alpha = if clean.len() >= 8 {
        f64::from(channel(6)) / 255.0
    } else {
        1.0
    };
    let mut color = Map::new();
    color.insert("r".to_owned(), Value::Number(Number::from(channel(0))));
    color.insert("g".to_owned(), Value::Number(Number::from(channel(2))));
    color.insert("b".to_owned(), Value::Number(Number::from(channel(4))));
    color.insert("a".to_owned(), number_value(alpha));
    Value::Object(color)
}

fn is_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn compile_error(code: CompileErrorCode, message: String, span: Option<Span>) -> CompileError {
    CompileError {
        code,
        message,
        span,
    }
}
