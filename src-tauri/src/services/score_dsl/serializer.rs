use std::cmp::Ordering;
use std::collections::HashSet;
use std::fmt;

use crate::models::node_graph::BlendMode;

use super::parser::blend_mode_name;
use super::types::{
    Annotation, Arg, ArgValue, BarRange, Comment, Document, GroupExpr, Layer, PatternArgument,
    PatternDefinition, PatternRegistry, TimeUnit,
};
use super::version::canonical_header;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SerializeOptions {
    pub beats_per_bar: u32,
    pub subdivisions_per_beat: u32,
}

impl Default for SerializeOptions {
    fn default() -> Self {
        Self {
            beats_per_bar: 4,
            subdivisions_per_beat: 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SerializeError {
    NonFiniteNumber,
    MissingClipId { pattern: String },
    DuplicateClipId { clip_id: String },
    MissingPatternId { clip_id: String },
    UnknownPatternId { pattern_id: String },
    CanonicalBarTime { clip_id: String },
    CanonicalSelectionSugar { clip_id: String },
    CanonicalArgumentSugar { clip_id: String, key: String },
    DuplicateArgument { clip_id: String, key: String },
    InvalidSpatialReference { value: String },
    UnrepresentableBarPosition { value: String },
    InvalidJsonNumber,
}

impl fmt::Display for SerializeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteNumber => {
                write!(formatter, "Luma DSL cannot serialize a non-finite number")
            }
            Self::MissingClipId { pattern } => {
                write!(
                    formatter,
                    "canonical clip for pattern {pattern:?} has no stable id"
                )
            }
            Self::DuplicateClipId { clip_id } => {
                write!(
                    formatter,
                    "canonical score contains duplicate clip id {clip_id:?}"
                )
            }
            Self::MissingPatternId { clip_id } => {
                write!(
                    formatter,
                    "canonical clip {clip_id:?} has no stable pattern id"
                )
            }
            Self::UnknownPatternId { pattern_id } => {
                write!(
                    formatter,
                    "canonical clip references unavailable pattern id {pattern_id:?}"
                )
            }
            Self::CanonicalBarTime { clip_id } => write!(
                formatter,
                "canonical clip {clip_id:?} must store its exact time in seconds"
            ),
            Self::CanonicalSelectionSugar { clip_id } => write!(
                formatter,
                "canonical clip {clip_id:?} must store Selection values as explicit raw arguments"
            ),
            Self::CanonicalArgumentSugar { clip_id, key } => write!(
                formatter,
                "canonical clip {clip_id:?} arg {key:?} must use an explicit JSON value"
            ),
            Self::DuplicateArgument { clip_id, key } => write!(
                formatter,
                "canonical clip {clip_id:?} assigns arg {key:?} more than once"
            ),
            Self::InvalidSpatialReference { value } => write!(
                formatter,
                "selection spatial reference {value:?} is not a DSL identifier"
            ),
            Self::UnrepresentableBarPosition { value } => write!(
                formatter,
                "bar position {value} is not representable on the configured subdivision"
            ),
            Self::InvalidJsonNumber => write!(formatter, "Luma DSL arguments must be JSON values"),
        }
    }
}

impl std::error::Error for SerializeError {}

pub fn format_number(value: f64) -> Result<String, SerializeError> {
    if !value.is_finite() {
        return Err(SerializeError::NonFiniteNumber);
    }
    Ok(value.to_string())
}

/// Serialize a parsed, human-authored AST. Stable IDs remain optional and
/// comments remain attached, but whitespace is normalized.
pub fn serialize(
    document: &Document,
    registry: &PatternRegistry,
    options: SerializeOptions,
) -> Result<String, SerializeError> {
    serialize_document(document, Some(registry), options, CanonicalMode::Human)
}

/// Serialize the deterministic authored-workspace form. Every clip and pattern must
/// have a stable identity; layers and ordering are explicit.
pub fn serialize_canonical(document: &Document) -> Result<String, SerializeError> {
    let body = serialize_document(
        document,
        None,
        SerializeOptions::default(),
        CanonicalMode::Stable,
    )?;
    let header = canonical_header();
    if body.is_empty() {
        Ok(header)
    } else {
        Ok(format!("{header}\n{body}"))
    }
}

/// Deterministic canonical score syntax with only the persisted clip UUIDs
/// omitted. Model exemplars use this form so they cannot copy identities from
/// another track while still seeing the exact score grammar and values.
pub fn serialize_exemplar(
    document: &Document,
    registry: &PatternRegistry,
    options: SerializeOptions,
) -> Result<String, SerializeError> {
    serialize_document(document, Some(registry), options, CanonicalMode::Exemplar)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CanonicalMode {
    Human,
    Stable,
    Exemplar,
}

impl CanonicalMode {
    fn is_canonical(self) -> bool {
        self != Self::Human
    }
}

fn serialize_document(
    document: &Document,
    registry: Option<&PatternRegistry>,
    options: SerializeOptions,
    mode: CanonicalMode,
) -> Result<String, SerializeError> {
    let canonical = mode.is_canonical();
    let mut lines = Vec::new();
    let mut layers: Vec<&Layer> = document.layers.iter().collect();
    if canonical {
        layers.sort_by(|left, right| left.z_index.cmp(&right.z_index));
    }
    let force_explicit_layers = canonical || document.layers.iter().any(|layer| layer.explicit_z);
    let mut clip_ids = HashSet::new();

    for (layer_index, layer) in layers.into_iter().enumerate() {
        if layer_index > 0 {
            lines.push(String::new());
        }
        emit_comments(&mut lines, &layer.trivia.leading_comments);
        if force_explicit_layers {
            let mut line = format!("layer {}:", layer.z_index);
            append_inline_comment(&mut line, layer.trivia.trailing_comment.as_ref());
            lines.push(line);
        }

        let mut annotations: Vec<&Annotation> = layer.annotations.iter().collect();
        if canonical {
            annotations.sort_by(|left, right| {
                left.range
                    .start
                    .total_cmp(&right.range.start)
                    .then_with(|| option_string_cmp(&left.id, &right.id))
            });
        }
        for annotation in annotations {
            if mode == CanonicalMode::Stable {
                if let Some(id) = &annotation.id {
                    if !clip_ids.insert(id.as_str()) {
                        return Err(SerializeError::DuplicateClipId {
                            clip_id: id.clone(),
                        });
                    }
                }
            }
            emit_comments(&mut lines, &annotation.trivia.leading_comments);
            let mut line = serialize_annotation(annotation, registry, options, mode)?;
            append_inline_comment(&mut line, annotation.trivia.trailing_comment.as_ref());
            lines.push(line);
        }
    }
    emit_comments(&mut lines, &document.trailing_comments);
    Ok(lines.join("\n"))
}

fn emit_comments(lines: &mut Vec<String>, comments: &[Comment]) {
    lines.extend(comments.iter().map(|comment| {
        if comment.text.is_empty() {
            "#".to_owned()
        } else {
            format!("# {}", comment.text)
        }
    }));
}

fn append_inline_comment(line: &mut String, comment: Option<&Comment>) {
    if let Some(comment) = comment {
        line.push_str(" #");
        if !comment.text.is_empty() {
            line.push(' ');
            line.push_str(&comment.text);
        }
    }
}

fn serialize_annotation(
    annotation: &Annotation,
    registry: Option<&PatternRegistry>,
    options: SerializeOptions,
    mode: CanonicalMode,
) -> Result<String, SerializeError> {
    let canonical = mode.is_canonical();
    let mut parts = Vec::new();
    if mode == CanonicalMode::Stable && annotation.id.is_none() {
        return Err(SerializeError::MissingClipId {
            pattern: annotation.pattern.clone(),
        });
    }
    if mode != CanonicalMode::Exemplar {
        if let Some(id) = &annotation.id {
            parts.push(format!("{}:", json_string(id)));
        }
    }

    let (pattern_name, pattern_definition) = if mode == CanonicalMode::Stable {
        let clip_id = annotation
            .id
            .clone()
            .expect("stable mode checked the clip id above");
        if annotation.pattern_id.is_none() {
            return Err(SerializeError::MissingPatternId { clip_id });
        }
        (annotation.pattern.as_str(), None)
    } else if mode == CanonicalMode::Exemplar {
        let clip_id = annotation
            .id
            .clone()
            .unwrap_or_else(|| "(exemplar clip)".to_owned());
        let pattern_id = annotation
            .pattern_id
            .as_ref()
            .ok_or(SerializeError::MissingPatternId { clip_id })?;
        let definition = registry
            .expect("exemplar serialization requires a registry")
            .by_id(pattern_id)
            .ok_or_else(|| SerializeError::UnknownPatternId {
                pattern_id: pattern_id.clone(),
            })?;
        (definition.name.as_str(), Some(definition))
    } else {
        let registry = registry.expect("human serialization requires a registry");
        (
            annotation.pattern.as_str(),
            find_pattern(
                registry,
                &annotation.pattern,
                annotation.pattern_id.as_deref(),
            ),
        )
    };

    let display_name = if is_identifier(pattern_name) {
        pattern_name.to_owned()
    } else {
        json_string(pattern_name)
    };
    let pattern_reference = annotation
        .pattern_id
        .as_ref()
        .map_or(display_name.clone(), |id| {
            format!("{display_name}[{}]", json_string(id))
        });
    if mode == CanonicalMode::Stable && annotation.range.unit != TimeUnit::Seconds {
        return Err(SerializeError::CanonicalBarTime {
            clip_id: annotation.id.clone().expect("checked above"),
        });
    }
    if mode == CanonicalMode::Stable
        && (annotation.selection.is_some() || annotation.selection_spatial_reference.is_some())
    {
        return Err(SerializeError::CanonicalSelectionSugar {
            clip_id: annotation.id.clone().expect("checked above"),
        });
    }
    let selection = if let Some(selection) = &annotation.selection {
        let expression = serialize_group_expression(selection);
        if annotation
            .selection_spatial_reference
            .as_deref()
            .is_none_or(|value| value == "global")
        {
            expression
        } else {
            let spatial_reference = annotation
                .selection_spatial_reference
                .as_deref()
                .expect("checked above");
            if !is_identifier(spatial_reference) {
                return Err(SerializeError::InvalidSpatialReference {
                    value: spatial_reference.to_owned(),
                });
            }
            format!("{spatial_reference}: {expression}")
        }
    } else {
        String::new()
    };
    parts.push(format!("{pattern_reference}({selection})"));
    parts.push(serialize_range(annotation.range, options)?);

    let mut args: Vec<(usize, &Arg, String)> = annotation
        .args
        .iter()
        .enumerate()
        .map(|(source_index, argument)| {
            let key = if canonical {
                canonical_arg_key(argument, pattern_definition)
            } else {
                argument.key.clone()
            };
            (source_index, argument, key)
        })
        .collect();
    args.sort_by(|left, right| {
        let left_index = definition_index(&left.2, pattern_definition);
        let right_index = definition_index(&right.2, pattern_definition);
        left_index
            .cmp(&right_index)
            .then_with(|| {
                if canonical && left_index == usize::MAX {
                    left.2.cmp(&right.2)
                } else {
                    Ordering::Equal
                }
            })
            .then(left.0.cmp(&right.0))
    });
    let mut argument_keys = HashSet::new();
    for (_, argument, key) in args {
        if mode == CanonicalMode::Stable && !argument_keys.insert(key.clone()) {
            return Err(SerializeError::DuplicateArgument {
                clip_id: annotation.id.clone().expect("checked above"),
                key,
            });
        }
        if mode == CanonicalMode::Stable && !matches!(argument.value, ArgValue::Json(_)) {
            return Err(SerializeError::CanonicalArgumentSugar {
                clip_id: annotation.id.clone().expect("checked above"),
                key,
            });
        }
        parts.push(format!(
            "{}={}",
            serialize_arg_key(&key),
            serialize_arg_value(&argument.value, canonical)?
        ));
    }
    if annotation.blend != BlendMode::Replace {
        parts.push(format!("blend={}", blend_mode_name(annotation.blend)));
    }
    Ok(parts.join(" "))
}

fn serialize_range(range: BarRange, options: SerializeOptions) -> Result<String, SerializeError> {
    if range.unit == TimeUnit::Seconds {
        return Ok(format!(
            "@{}s-{}s",
            format_number(range.start)?,
            format_number(range.end)?
        ));
    }
    let start = format_bar_position(range.start, options)?;
    let end = format_bar_position(range.end, options)?;
    if range.end == range.start + 1.0 && range.start.fract() == 0.0 {
        Ok(format!("@{start}"))
    } else {
        Ok(format!("@{start}-{end}"))
    }
}

fn format_bar_position(value: f64, options: SerializeOptions) -> Result<String, SerializeError> {
    if !value.is_finite() {
        return Err(SerializeError::NonFiniteNumber);
    }
    let bar = value.floor();
    let remainder = value - bar;
    if remainder == 0.0 {
        return Ok((bar as i64).to_string());
    }
    let total_subdivisions = options.beats_per_bar * options.subdivisions_per_beat;
    let subdivision_index = (remainder * f64::from(total_subdivisions)).round();
    let subdivision_index = subdivision_index as u32;
    let beat = subdivision_index / options.subdivisions_per_beat + 1;
    let subdivision = subdivision_index % options.subdivisions_per_beat + 1;
    // Mirror the parser's operation order, not merely the mathematically
    // equivalent total-subdivision fraction. This keeps the exact f64 bar
    // coordinate stable in non-power-of-two meters such as 3/4.
    let reconstructed = bar
        + f64::from(beat - 1) / f64::from(options.beats_per_bar)
        + f64::from(subdivision - 1) / f64::from(total_subdivisions);
    if reconstructed.to_bits() != value.to_bits() {
        return Err(SerializeError::UnrepresentableBarPosition {
            value: value.to_string(),
        });
    }
    if subdivision == 1 {
        Ok(format!("{}:{beat}", bar as i64))
    } else {
        Ok(format!("{}:{beat}:{subdivision}", bar as i64))
    }
}

fn serialize_arg_value(value: &ArgValue, canonical: bool) -> Result<String, SerializeError> {
    match value {
        ArgValue::Color(value) | ArgValue::Identifier(value) => Ok(value.clone()),
        ArgValue::Number(value) => format_number(*value),
        ArgValue::Json(value) if canonical => Ok(crate::canonical_json::to_string(value)),
        ArgValue::Json(value) => {
            serde_json::to_string(value).map_err(|_| SerializeError::InvalidJsonNumber)
        }
    }
}

fn serialize_arg_key(key: &str) -> String {
    if is_identifier(key) && key != "blend" {
        key.to_owned()
    } else {
        json_string(key)
    }
}

fn canonical_arg_key(argument: &Arg, pattern: Option<&PatternDefinition>) -> String {
    let Some(pattern) = pattern else {
        return argument.key.clone();
    };
    if pattern
        .args
        .iter()
        .any(|definition| definition.id == argument.key)
    {
        return argument.key.clone();
    }
    let aliases: Vec<&PatternArgument> = pattern
        .args
        .iter()
        .filter(|definition| definition.name == argument.key && pattern.is_safe_alias(definition))
        .collect();
    if aliases.len() == 1
        && !pattern
            .args
            .iter()
            .any(|definition| definition.id == argument.key)
    {
        aliases[0].id.clone()
    } else {
        argument.key.clone()
    }
}

fn definition_index(key: &str, pattern: Option<&PatternDefinition>) -> usize {
    let Some(pattern) = pattern else {
        return usize::MAX;
    };
    if let Some(index) = pattern
        .args
        .iter()
        .position(|definition| definition.id == key)
    {
        return index;
    }
    pattern
        .args
        .iter()
        .position(|definition| definition.name == key && pattern.is_safe_alias(definition))
        .unwrap_or(usize::MAX)
}

fn find_pattern<'a>(
    registry: &'a PatternRegistry,
    name: &str,
    id: Option<&str>,
) -> Option<&'a PatternDefinition> {
    id.and_then(|id| registry.by_id(id))
        .or_else(|| registry.by_name(name).into_iter().next())
}

pub fn serialize_group_expression(expression: &GroupExpr) -> String {
    match expression {
        GroupExpr::Group { name } => name.clone(),
        GroupExpr::Not { operand } => format!("~{}", serialize_group_expression(operand)),
        GroupExpr::And { left, right } => format!(
            "{} & {}",
            serialize_group_with_precedence(left, 4),
            serialize_group_with_precedence(right, 4)
        ),
        GroupExpr::Or { left, right } => format!(
            "{} | {}",
            serialize_group_with_precedence(left, 2),
            serialize_group_with_precedence(right, 2)
        ),
        GroupExpr::Xor { left, right } => format!(
            "{} ^ {}",
            serialize_group_with_precedence(left, 3),
            serialize_group_with_precedence(right, 3)
        ),
        GroupExpr::Fallback { left, right } => format!(
            "{} > {}",
            serialize_group_with_precedence(left, 1),
            serialize_group_with_precedence(right, 1)
        ),
        GroupExpr::Paren { inner } => format!("({})", serialize_group_expression(inner)),
    }
}

fn serialize_group_with_precedence(expression: &GroupExpr, parent_precedence: u8) -> String {
    let precedence = match expression {
        GroupExpr::Fallback { .. } => 1,
        GroupExpr::Or { .. } => 2,
        GroupExpr::Xor { .. } => 3,
        GroupExpr::And { .. } => 4,
        GroupExpr::Not { .. } => 5,
        GroupExpr::Group { .. } | GroupExpr::Paren { .. } => 6,
    };
    let value = serialize_group_expression(expression);
    if precedence < parent_precedence {
        format!("({value})")
    } else {
        value
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("strings are always valid JSON")
}

fn is_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn option_string_cmp(left: &Option<String>, right: &Option<String>) -> Ordering {
    left.as_deref().cmp(&right.as_deref())
}
