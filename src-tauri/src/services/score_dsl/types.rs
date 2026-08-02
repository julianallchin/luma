use std::collections::HashMap;

use serde_json::Value;

use crate::models::node_graph::{BlendMode, PatternArgType};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Loc {
    pub line: usize,
    pub column: usize,
    pub offset: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: Loc,
    pub end: Loc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    /// Comment text without the `#`. Formatting around the text is normalized
    /// on serialization; the authored text itself is retained.
    pub text: String,
    pub span: Span,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trivia {
    /// Full-line comments immediately preceding the node.
    pub leading_comments: Vec<Comment>,
    /// A comment on the same line as the node.
    pub trailing_comment: Option<Comment>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PatternArgument {
    pub id: String,
    pub name: String,
    pub arg_type: PatternArgType,
    pub default_value: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PatternDefinition {
    pub id: Option<String>,
    pub name: String,
    pub args: Vec<PatternArgument>,
}

impl PatternDefinition {
    pub(crate) fn is_safe_alias(&self, argument: &PatternArgument) -> bool {
        self.args
            .iter()
            .filter(|candidate| candidate.name == argument.name)
            .count()
            == 1
            && !self
                .args
                .iter()
                .any(|candidate| candidate.id == argument.name)
    }

    pub(crate) fn argument_matches_key(&self, argument: &PatternArgument, key: &str) -> bool {
        argument.id == key || (argument.name == key && self.is_safe_alias(argument))
    }
}

/// Registry entries are stored as a list rather than keyed only by name: two
/// installed patterns may legitimately share a presentation name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PatternRegistry {
    definitions: Vec<PatternDefinition>,
    by_id: HashMap<String, usize>,
    unavailable_by_id: HashMap<String, UnavailablePattern>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnavailablePattern {
    pub(crate) name: String,
    pub(crate) reason: String,
}

impl PatternRegistry {
    pub fn new(definitions: Vec<PatternDefinition>) -> Self {
        let mut by_id = HashMap::new();
        for (index, definition) in definitions.iter().enumerate() {
            if let Some(id) = &definition.id {
                by_id.entry(id.clone()).or_insert(index);
            }
        }
        Self {
            definitions,
            by_id,
            unavailable_by_id: HashMap::new(),
        }
    }

    pub(crate) fn with_unavailable(
        definitions: Vec<PatternDefinition>,
        unavailable_by_id: HashMap<String, UnavailablePattern>,
    ) -> Self {
        let mut registry = Self::new(definitions);
        registry.unavailable_by_id = unavailable_by_id;
        registry
    }

    pub fn definitions(&self) -> &[PatternDefinition] {
        &self.definitions
    }

    pub fn by_id(&self, id: &str) -> Option<&PatternDefinition> {
        self.by_id
            .get(id)
            .and_then(|index| self.definitions.get(*index))
    }

    pub fn by_name(&self, name: &str) -> Vec<&PatternDefinition> {
        self.definitions
            .iter()
            .filter(|definition| definition.name == name)
            .collect()
    }

    pub(crate) fn unavailable_by_id(&self, id: &str) -> Option<&UnavailablePattern> {
        self.unavailable_by_id.get(id)
    }

    pub(crate) fn unavailable_by_name(&self, name: &str) -> Vec<(&str, &UnavailablePattern)> {
        self.unavailable_by_id
            .iter()
            .filter(|(_, pattern)| pattern.name == name)
            .map(|(id, pattern)| (id.as_str(), pattern))
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupExpr {
    Group {
        name: String,
    },
    Not {
        operand: Box<GroupExpr>,
    },
    And {
        left: Box<GroupExpr>,
        right: Box<GroupExpr>,
    },
    Or {
        left: Box<GroupExpr>,
        right: Box<GroupExpr>,
    },
    Xor {
        left: Box<GroupExpr>,
        right: Box<GroupExpr>,
    },
    Fallback {
        left: Box<GroupExpr>,
        right: Box<GroupExpr>,
    },
    Paren {
        inner: Box<GroupExpr>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ArgValue {
    Color(String),
    Number(f64),
    Identifier(String),
    Json(Value),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub key: String,
    pub value: ArgValue,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimeUnit {
    #[default]
    Bars,
    Seconds,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarRange {
    pub start: f64,
    pub end: f64,
    pub unit: TimeUnit,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub id: Option<String>,
    pub pattern: String,
    pub pattern_id: Option<String>,
    /// `None` is an absent Selection override; `Some(all)` is explicit `all`.
    pub selection: Option<GroupExpr>,
    pub selection_spatial_reference: Option<String>,
    pub range: BarRange,
    pub args: Vec<Arg>,
    pub blend: BlendMode,
    pub span: Span,
    pub trivia: Trivia,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub z_index: i64,
    /// Whether the parsed source used a `layer N:` declaration. Canonical
    /// serialization ignores this and always writes the declaration.
    pub explicit_z: bool,
    pub annotations: Vec<Annotation>,
    pub trivia: Trivia,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    pub layers: Vec<Layer>,
    /// Full-line comments after the final layer/annotation.
    pub trailing_comments: Vec<Comment>,
}
