use serde_json::{Map, Value};

use crate::models::node_graph::{BlendMode, PatternArgType};

use super::error::{DslError, DslErrorCode, DslWarning};
use super::tokenizer::{tokenize, Token, TokenKind};
use super::types::{
    Annotation, Arg, ArgValue, BarRange, Comment, Document, GroupExpr, Layer, Loc,
    PatternDefinition, PatternRegistry, Span, TimeUnit, Trivia,
};
use super::version::{decode_current, decode_optional_current};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseOptions {
    pub beats_per_bar: u32,
    pub subdivisions_per_beat: u32,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            beats_per_bar: 4,
            subdivisions_per_beat: 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParseResult {
    Success {
        document: Document,
        warnings: Vec<DslWarning>,
    },
    Failure {
        errors: Vec<DslError>,
        partial: Document,
    },
}

pub fn parse(source: &str, registry: &PatternRegistry, options: ParseOptions) -> ParseResult {
    let source = match decode_optional_current(source) {
        Ok(source) => source,
        Err(error) => {
            return ParseResult::Failure {
                errors: vec![error],
                partial: Document::default(),
            };
        }
    };
    Parser::new(
        tokenize(&source),
        registry,
        options,
        PatternResolution::Installed,
    )
    .parse()
}

/// Parse the stable authored form without consulting installed patterns. Canonical
/// source carries every pattern identity and raw argument value itself; using a
/// live registry here would make an old commit change meaning when a pattern's
/// interface is edited later.
pub(crate) fn parse_canonical(source: &str) -> ParseResult {
    let source = match decode_current(source) {
        Ok(source) => source,
        Err(error) => {
            return ParseResult::Failure {
                errors: vec![error],
                partial: Document::default(),
            };
        }
    };
    let registry = PatternRegistry::default();
    Parser::new(
        tokenize(&source),
        &registry,
        ParseOptions::default(),
        PatternResolution::Canonical,
    )
    .parse()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PatternResolution {
    Installed,
    Canonical,
}

struct ParsedNumber {
    value: f64,
    raw: String,
    span: Span,
}

struct Parser<'a> {
    tokens: Vec<Token>,
    registry: &'a PatternRegistry,
    options: ParseOptions,
    pattern_resolution: PatternResolution,
    position: usize,
    errors: Vec<DslError>,
    warnings: Vec<DslWarning>,
}

impl<'a> Parser<'a> {
    fn new(
        tokens: Vec<Token>,
        registry: &'a PatternRegistry,
        options: ParseOptions,
        pattern_resolution: PatternResolution,
    ) -> Self {
        Self {
            tokens,
            registry,
            options,
            pattern_resolution,
            position: 0,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn parse(mut self) -> ParseResult {
        let mut layers = Vec::new();
        let mut current: Option<Layer> = None;
        let mut pending_comments = Vec::new();

        while !self.at_end() {
            if self.check(TokenKind::Newline) {
                let mut count = 0;
                while self.check(TokenKind::Newline) {
                    self.advance();
                    count += 1;
                }
                if count >= 2
                    && current
                        .as_ref()
                        .is_some_and(|layer| !layer.explicit_z && !layer.annotations.is_empty())
                {
                    Self::push_layer(&mut layers, &mut current);
                }
                continue;
            }

            if self.check(TokenKind::Comment) {
                pending_comments.push(self.take_comment());
                if self.check(TokenKind::Newline) {
                    self.advance();
                }
                continue;
            }

            if self.is_layer_directive() {
                Self::push_layer(&mut layers, &mut current);
                if let Some(mut layer) = self.parse_layer_directive() {
                    layer.trivia.leading_comments = std::mem::take(&mut pending_comments);
                    current = Some(layer);
                }
                continue;
            }

            if self.check(TokenKind::Identifier) || self.check(TokenKind::String) {
                if current.is_none() {
                    current = Some(Layer {
                        z_index: layers.len() as i64,
                        explicit_z: false,
                        annotations: Vec::new(),
                        trivia: Trivia::default(),
                    });
                }
                if let Some(mut annotation) = self.parse_annotation() {
                    annotation.trivia.leading_comments = std::mem::take(&mut pending_comments);
                    if let Some(layer) = &mut current {
                        layer.annotations.push(annotation);
                    }
                }
                continue;
            }

            self.error(
                DslErrorCode::UnexpectedToken,
                format!("expected pattern name, got {:?}", self.peek().value),
                self.peek().span,
                None,
            );
            self.skip_to_next_line();
        }

        Self::push_layer(&mut layers, &mut current);
        let document = Document {
            layers,
            trailing_comments: pending_comments,
        };
        self.warn_about_overlaps(&document);

        if self.errors.is_empty() {
            ParseResult::Success {
                document,
                warnings: self.warnings,
            }
        } else {
            ParseResult::Failure {
                errors: self.errors,
                partial: document,
            }
        }
    }

    fn push_layer(layers: &mut Vec<Layer>, current: &mut Option<Layer>) {
        if let Some(layer) = current.take() {
            if layer.explicit_z || !layer.annotations.is_empty() {
                layers.push(layer);
            }
        }
    }

    fn parse_annotation(&mut self) -> Option<Annotation> {
        let first_start = self.peek().span.start;
        let id = if self.check(TokenKind::String) && self.kind_at(1) == Some(TokenKind::Colon) {
            let value = self.advance().value.clone();
            self.advance();
            Some(value)
        } else {
            None
        };

        if !self.check(TokenKind::Identifier) && !self.check(TokenKind::String) {
            self.error(
                DslErrorCode::UnexpectedToken,
                format!("expected pattern name, got {:?}", self.peek().value),
                self.peek().span,
                None,
            );
            self.skip_to_next_line();
            return None;
        }
        let name_token = self.advance().clone();
        let pattern_name = name_token.value.clone();

        let pattern_id = if self.check(TokenKind::LeftBracket) {
            self.advance();
            let id = self.expect(TokenKind::String)?.value.clone();
            self.expect(TokenKind::RightBracket)?;
            Some(id)
        } else {
            None
        };

        let pattern = self.resolve_pattern(&name_token, &pattern_name, pattern_id.as_deref())?;

        if !self.check(TokenKind::LeftParen) {
            self.error(
                DslErrorCode::MissingSelection,
                format!("expected \"(\" after pattern name {pattern_name:?}"),
                self.peek().span,
                None,
            );
            self.skip_to_next_line();
            return None;
        }
        self.advance();
        let mut selection = None;
        if !self.check(TokenKind::RightParen) {
            selection = Some(self.parse_group_expression());
        }
        if self.expect(TokenKind::RightParen).is_none() {
            self.skip_to_next_line();
            return None;
        }

        let range = if self.check(TokenKind::At) {
            self.parse_bar_range()
        } else {
            None
        };
        let Some(range) = range else {
            self.error(
                DslErrorCode::UnexpectedToken,
                format!("expected bar range (@) for annotation {pattern_name:?}"),
                self.peek().span,
                None,
            );
            self.skip_to_next_line();
            return None;
        };

        let mut args = Vec::new();
        let mut blend = BlendMode::Replace;
        while !self.at_end() && !self.check(TokenKind::Newline) && !self.check(TokenKind::Comment) {
            if !self.check(TokenKind::Identifier) && !self.check(TokenKind::String) {
                break;
            }
            if self.kind_at(1) != Some(TokenKind::Equals) {
                break;
            }
            let key_token = self.advance().clone();
            self.advance();
            let key = key_token.value.clone();
            if key_token.kind == TokenKind::Identifier && key == "blend" {
                let value = self.peek().clone();
                if value.kind != TokenKind::Identifier {
                    self.error(
                        DslErrorCode::UnexpectedToken,
                        "expected blend mode identifier".to_owned(),
                        value.span,
                        None,
                    );
                } else {
                    self.advance();
                    if let Some(mode) = BlendMode::from_name(&value.value) {
                        blend = mode;
                    } else {
                        self.error(
                            DslErrorCode::InvalidBlendMode,
                            format!("invalid blend mode {:?}", value.value),
                            value.span,
                            Some(format!("Valid modes: {}", blend_mode_list())),
                        );
                    }
                }
                continue;
            }

            let parsed_value = if self.pattern_resolution == PatternResolution::Canonical {
                self.parse_json_value()
                    .map(|(value, span)| (ArgValue::Json(value), span))
            } else {
                self.parse_arg_value()
            };
            let Some((value, value_span)) = parsed_value else {
                continue;
            };
            let span = Span {
                start: key_token.span.start,
                end: value_span.end,
            };
            if self.pattern_resolution == PatternResolution::Installed {
                let definition = pattern.args.iter().find(|argument| {
                    pattern.argument_matches_key(argument, &key)
                        && argument.arg_type != PatternArgType::Selection
                });
                if let Some(definition) = definition {
                    self.validate_arg_type(&definition.arg_type, &value, &key, span);
                } else if pattern.args.iter().any(|argument| {
                    pattern.argument_matches_key(argument, &key)
                        && argument.arg_type == PatternArgType::Selection
                }) {
                    self.warnings.push(DslWarning {
                        code: "selection_as_arg".to_owned(),
                        message: format!(
                            "{key:?} is a Selection arg — use the parenthesized selection instead"
                        ),
                        span,
                    });
                } else {
                    self.warnings.push(DslWarning {
                        code: "unknown_arg".to_owned(),
                        message: format!("unknown arg {key:?} for pattern {pattern_name:?}"),
                        span,
                    });
                }
            }
            args.push(Arg { key, value, span });
        }

        let trailing_comment = if self.check(TokenKind::Comment) {
            Some(self.take_comment())
        } else {
            None
        };
        let end = self.previous_end();
        Some(Annotation {
            id,
            pattern: pattern_name,
            pattern_id,
            selection,
            range,
            args,
            blend,
            span: Span {
                start: first_start,
                end,
            },
            trivia: Trivia {
                leading_comments: Vec::new(),
                trailing_comment,
            },
        })
    }

    fn resolve_pattern(
        &mut self,
        name_token: &Token,
        pattern_name: &str,
        pattern_id: Option<&str>,
    ) -> Option<PatternDefinition> {
        if self.pattern_resolution == PatternResolution::Canonical {
            let Some(pattern_id) = pattern_id else {
                self.error(
                    DslErrorCode::UnknownPattern,
                    format!("canonical pattern {pattern_name:?} is missing its stable pattern id"),
                    name_token.span,
                    None,
                );
                self.skip_to_next_line();
                return None;
            };
            return Some(PatternDefinition {
                id: Some(pattern_id.to_owned()),
                name: pattern_name.to_owned(),
                args: Vec::new(),
            });
        }

        let matching: Vec<PatternDefinition> = if let Some(pattern_id) = pattern_id {
            self.registry
                .by_id(pattern_id)
                .into_iter()
                .cloned()
                .collect()
        } else {
            self.registry
                .by_name(pattern_name)
                .into_iter()
                .cloned()
                .collect()
        };
        let unavailable = if let Some(pattern_id) = pattern_id {
            self.registry
                .unavailable_by_id(pattern_id)
                .map(|pattern| vec![(pattern_id, pattern)])
                .unwrap_or_default()
        } else {
            self.registry.unavailable_by_name(pattern_name)
        };
        if pattern_id.is_none() && matching.len() + unavailable.len() > 1 {
            self.error(
                DslErrorCode::UnknownPattern,
                format!(
                    "pattern name {pattern_name:?} is ambiguous; qualify it with its stable id"
                ),
                name_token.span,
                None,
            );
            self.skip_to_next_line();
            return None;
        }
        if matching.is_empty() {
            if let [(_, unavailable)] = unavailable.as_slice() {
                self.error(
                    DslErrorCode::UnknownPattern,
                    format!(
                        "pattern {pattern_name:?} is installed but its graph is unavailable: {}",
                        unavailable.reason
                    ),
                    name_token.span,
                    None,
                );
                self.skip_to_next_line();
                return None;
            }
            let mut available: Vec<&str> = self
                .registry
                .definitions()
                .iter()
                .map(|definition| definition.name.as_str())
                .collect();
            available.sort_unstable();
            available.dedup();
            self.error(
                DslErrorCode::UnknownPattern,
                pattern_id.map_or_else(
                    || format!("unknown pattern {pattern_name:?}"),
                    |id| format!("unknown pattern {pattern_name:?} with id {id:?}"),
                ),
                name_token.span,
                Some(format!("Available patterns: {}", available.join(", "))),
            );
            self.skip_to_next_line();
            return None;
        }
        if pattern_id.is_some() && matching[0].name != pattern_name {
            self.warnings.push(DslWarning {
                code: "stale_pattern_name".to_owned(),
                message: format!(
                    "pattern {pattern_name:?} is now named {:?}; stable id still resolves",
                    matching[0].name
                ),
                span: name_token.span,
            });
        }
        Some(matching[0].clone())
    }

    fn parse_bar_range(&mut self) -> Option<BarRange> {
        self.advance();
        if self.is_seconds_position() {
            let start = self.parse_seconds_position()?;
            self.expect(TokenKind::Dash)?;
            let end = self.parse_seconds_position()?;
            if end.value <= start.value {
                self.error(
                    DslErrorCode::InvalidBarRange,
                    "time range end must be > start".to_owned(),
                    Span {
                        start: start.span.start,
                        end: end.span.end,
                    },
                    None,
                );
                return None;
            }
            return Some(BarRange {
                start: start.value,
                end: end.value,
                unit: TimeUnit::Seconds,
            });
        }

        let start = self.parse_bar_position()?;
        let mut end_value = if start.has_subdivisions {
            start.value
                + 1.0 / f64::from(self.options.beats_per_bar * self.options.subdivisions_per_beat)
        } else if start.has_beats {
            start.value + 1.0 / f64::from(self.options.beats_per_bar)
        } else {
            start.value + 1.0
        };
        let mut end_span = start.span.end;
        if self.check(TokenKind::Dash) {
            self.advance();
            let end = self.parse_bar_position()?;
            end_value = end.value;
            end_span = end.span.end;
        }
        if end_value <= start.value {
            self.error(
                DslErrorCode::InvalidBarRange,
                "bar range end must be > start".to_owned(),
                Span {
                    start: start.span.start,
                    end: end_span,
                },
                None,
            );
            return None;
        }
        Some(BarRange {
            start: start.value,
            end: end_value,
            unit: TimeUnit::Bars,
        })
    }

    fn is_seconds_position(&self) -> bool {
        let offset = usize::from(self.check(TokenKind::Dash));
        self.kind_at(offset) == Some(TokenKind::Number)
            && self.kind_at(offset + 1) == Some(TokenKind::Identifier)
            && self
                .tokens
                .get(self.position + offset + 1)
                .is_some_and(|token| token.value == "s")
    }

    fn parse_seconds_position(&mut self) -> Option<ParsedNumber> {
        let number = self.parse_signed_number()?;
        let suffix = self.expect(TokenKind::Identifier)?.clone();
        if suffix.value != "s" {
            self.error(
                DslErrorCode::UnexpectedToken,
                format!("expected seconds suffix \"s\", got {:?}", suffix.value),
                suffix.span,
                None,
            );
            return None;
        }
        Some(ParsedNumber {
            value: number.value,
            raw: number.raw,
            span: Span {
                start: number.span.start,
                end: suffix.span.end,
            },
        })
    }

    fn parse_bar_position(&mut self) -> Option<ParsedBarPosition> {
        let bar = self.parse_unsigned_integer("bar")?;
        let mut value = bar.value as f64;
        let mut span = bar.span;
        let mut has_beats = false;
        let mut has_subdivisions = false;
        if self.check(TokenKind::Colon) {
            self.advance();
            let beat = self.parse_unsigned_integer("beat")?;
            has_beats = true;
            span.end = beat.span.end;
            value += (beat.value as f64 - 1.0) / f64::from(self.options.beats_per_bar);
            if self.check(TokenKind::Colon) {
                self.advance();
                let subdivision = self.parse_unsigned_integer("subdivision")?;
                has_subdivisions = true;
                span.end = subdivision.span.end;
                value += (subdivision.value as f64 - 1.0)
                    / f64::from(self.options.beats_per_bar * self.options.subdivisions_per_beat);
            }
        }
        Some(ParsedBarPosition {
            value,
            has_beats,
            has_subdivisions,
            span,
        })
    }

    fn parse_unsigned_integer(&mut self, label: &str) -> Option<ParsedInteger> {
        let token = self.expect(TokenKind::Number)?.clone();
        match token.value.parse::<i64>() {
            Ok(value) => Some(ParsedInteger {
                value,
                span: token.span,
            }),
            Err(_) => {
                self.error(
                    DslErrorCode::UnexpectedToken,
                    format!("{label} must be an integer"),
                    token.span,
                    None,
                );
                None
            }
        }
    }

    fn parse_arg_value(&mut self) -> Option<(ArgValue, Span)> {
        let token = self.peek().clone();
        match token.kind {
            TokenKind::HexColor => {
                self.advance();
                Some((ArgValue::Color(token.value), token.span))
            }
            TokenKind::Number | TokenKind::Dash => {
                let number = self.parse_signed_number()?;
                Some((ArgValue::Number(number.value), number.span))
            }
            TokenKind::Identifier if matches!(token.value.as_str(), "true" | "false" | "null") => {
                let (value, span) = self.parse_json_value()?;
                Some((ArgValue::Json(value), span))
            }
            TokenKind::Identifier => {
                self.advance();
                Some((ArgValue::Identifier(token.value), token.span))
            }
            TokenKind::String | TokenKind::LeftBracket | TokenKind::LeftBrace => {
                let (value, span) = self.parse_json_value()?;
                Some((ArgValue::Json(value), span))
            }
            _ => {
                self.error(
                    DslErrorCode::UnexpectedToken,
                    format!("expected value, got {:?}", token.value),
                    token.span,
                    None,
                );
                None
            }
        }
    }

    fn parse_json_value(&mut self) -> Option<(Value, Span)> {
        let start = self.peek().span.start;
        match self.peek().kind {
            TokenKind::String => {
                let token = self.advance().clone();
                Some((Value::String(token.value), token.span))
            }
            TokenKind::Number | TokenKind::Dash => {
                let number = self.parse_signed_number()?;
                let value = serde_json::from_str::<Value>(&number.raw)
                    .ok()
                    .or_else(|| {
                        self.error(
                            DslErrorCode::UnexpectedToken,
                            "number must be finite JSON".to_owned(),
                            number.span,
                            None,
                        );
                        None
                    })?;
                Some((value, number.span))
            }
            TokenKind::Identifier => {
                let token = self.advance().clone();
                let value = match token.value.as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    "null" => Value::Null,
                    _ => {
                        self.error(
                            DslErrorCode::UnexpectedToken,
                            format!("expected JSON value, got {:?}", token.value),
                            token.span,
                            None,
                        );
                        return None;
                    }
                };
                Some((value, token.span))
            }
            TokenKind::LeftBracket => {
                self.advance();
                let mut values = Vec::new();
                while !self.check(TokenKind::RightBracket) && !self.at_end() {
                    values.push(self.parse_json_value()?.0);
                    if !self.check(TokenKind::Comma) {
                        break;
                    }
                    self.advance();
                }
                let end = self.expect(TokenKind::RightBracket)?.span.end;
                Some((Value::Array(values), Span { start, end }))
            }
            TokenKind::LeftBrace => {
                self.advance();
                let mut values = Map::new();
                while !self.check(TokenKind::RightBrace) && !self.at_end() {
                    let key = self.expect(TokenKind::String)?.value.clone();
                    self.expect(TokenKind::Colon)?;
                    values.insert(key, self.parse_json_value()?.0);
                    if !self.check(TokenKind::Comma) {
                        break;
                    }
                    self.advance();
                }
                let end = self.expect(TokenKind::RightBrace)?.span.end;
                Some((Value::Object(values), Span { start, end }))
            }
            _ => {
                let token = self.peek().clone();
                self.error(
                    DslErrorCode::UnexpectedToken,
                    format!("expected JSON value, got {:?}", token.value),
                    token.span,
                    None,
                );
                None
            }
        }
    }

    fn validate_arg_type(
        &mut self,
        expected: &PatternArgType,
        value: &ArgValue,
        key: &str,
        span: Span,
    ) {
        let mismatch = match expected {
            PatternArgType::Color => !matches!(value, ArgValue::Color(_) | ArgValue::Json(_)),
            PatternArgType::Scalar => !matches!(value, ArgValue::Number(_) | ArgValue::Json(_)),
            _ => false,
        };
        if mismatch {
            let expected = match expected {
                PatternArgType::Color => "color",
                PatternArgType::Scalar => "number",
                _ => unreachable!(),
            };
            self.error(
                DslErrorCode::TypeMismatch,
                format!(
                    "expected {expected} for arg {key:?}, got {}",
                    arg_value_name(value)
                ),
                span,
                None,
            );
        }
    }

    fn parse_group_expression(&mut self) -> GroupExpr {
        self.parse_fallback()
    }

    fn parse_fallback(&mut self) -> GroupExpr {
        let mut left = self.parse_or();
        while self.check(TokenKind::Fallback) {
            self.advance();
            left = GroupExpr::Fallback {
                left: Box::new(left),
                right: Box::new(self.parse_or()),
            };
        }
        left
    }

    fn parse_or(&mut self) -> GroupExpr {
        let mut left = self.parse_xor();
        while self.check(TokenKind::Or) {
            self.advance();
            left = GroupExpr::Or {
                left: Box::new(left),
                right: Box::new(self.parse_xor()),
            };
        }
        left
    }

    fn parse_xor(&mut self) -> GroupExpr {
        let mut left = self.parse_and();
        while self.check(TokenKind::Xor) {
            self.advance();
            left = GroupExpr::Xor {
                left: Box::new(left),
                right: Box::new(self.parse_and()),
            };
        }
        left
    }

    fn parse_and(&mut self) -> GroupExpr {
        let mut left = self.parse_unary();
        while self.check(TokenKind::And) {
            self.advance();
            left = GroupExpr::And {
                left: Box::new(left),
                right: Box::new(self.parse_unary()),
            };
        }
        left
    }

    fn parse_unary(&mut self) -> GroupExpr {
        if self.check(TokenKind::Not) {
            self.advance();
            return GroupExpr::Not {
                operand: Box::new(self.parse_unary()),
            };
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> GroupExpr {
        if self.check(TokenKind::LeftParen) {
            self.advance();
            let inner = self.parse_group_expression();
            if self.check(TokenKind::RightParen) {
                self.advance();
            }
            return GroupExpr::Paren {
                inner: Box::new(inner),
            };
        }
        if self.check(TokenKind::Identifier) {
            return GroupExpr::Group {
                name: self.advance().value.clone(),
            };
        }
        let token = self.peek().clone();
        self.error(
            DslErrorCode::UnexpectedToken,
            format!("expected group name, got {:?}", token.value),
            token.span,
            None,
        );
        GroupExpr::Group {
            name: "all".to_owned(),
        }
    }

    fn is_layer_directive(&self) -> bool {
        self.check(TokenKind::Identifier)
            && self.peek().value == "layer"
            && (self.kind_at(1) == Some(TokenKind::Number)
                || (self.kind_at(1) == Some(TokenKind::Dash)
                    && self.kind_at(2) == Some(TokenKind::Number)))
    }

    fn parse_layer_directive(&mut self) -> Option<Layer> {
        self.advance();
        let number = self.parse_signed_number()?;
        let z_index = if number.value.fract() == 0.0
            && number.value >= i64::MIN as f64
            && number.value <= i64::MAX as f64
        {
            number.value as i64
        } else {
            self.error(
                DslErrorCode::UnexpectedToken,
                "layer index must be an integer".to_owned(),
                number.span,
                None,
            );
            return None;
        };
        if self.check(TokenKind::Colon) {
            self.advance();
        }
        let trailing_comment = if self.check(TokenKind::Comment) {
            Some(self.take_comment())
        } else {
            None
        };
        if !self.check(TokenKind::Newline) && !self.at_end() {
            let token = self.peek().clone();
            self.error(
                DslErrorCode::UnexpectedToken,
                format!(
                    "unexpected token after layer declaration: {:?}",
                    token.value
                ),
                token.span,
                None,
            );
            self.skip_to_next_line();
        } else if self.check(TokenKind::Newline) {
            self.advance();
        }
        Some(Layer {
            z_index,
            explicit_z: true,
            annotations: Vec::new(),
            trivia: Trivia {
                leading_comments: Vec::new(),
                trailing_comment,
            },
        })
    }

    fn parse_signed_number(&mut self) -> Option<ParsedNumber> {
        let start = self.peek().span.start;
        let negative = if self.check(TokenKind::Dash) {
            self.advance();
            true
        } else {
            false
        };
        let token = self.expect(TokenKind::Number)?.clone();
        let raw = if negative {
            format!("-{}", token.value)
        } else {
            token.value.clone()
        };
        match raw.parse::<f64>() {
            Ok(value) if value.is_finite() => Some(ParsedNumber {
                value,
                raw,
                span: Span {
                    start,
                    end: token.span.end,
                },
            }),
            _ => {
                self.error(
                    DslErrorCode::UnexpectedToken,
                    "number must be finite".to_owned(),
                    Span {
                        start,
                        end: token.span.end,
                    },
                    None,
                );
                None
            }
        }
    }

    fn warn_about_overlaps(&mut self, document: &Document) {
        for (layer_index, layer) in document.layers.iter().enumerate() {
            for pair in layer.annotations.windows(2) {
                let previous = &pair[0];
                let current = &pair[1];
                if current.range.unit == previous.range.unit
                    && current.range.start < previous.range.end
                {
                    self.warnings.push(DslWarning {
                        code: "overlap".to_owned(),
                        message: format!(
                            "{:?} @{} overlaps with {:?} @{}-{} in layer {layer_index}",
                            current.pattern,
                            current.range.start,
                            previous.pattern,
                            previous.range.start,
                            previous.range.end
                        ),
                        span: current.span,
                    });
                }
            }
        }
    }

    fn take_comment(&mut self) -> Comment {
        let token = self.advance().clone();
        Comment {
            text: token.value,
            span: token.span,
        }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.position]
    }

    fn kind_at(&self, offset: usize) -> Option<TokenKind> {
        self.tokens
            .get(self.position + offset)
            .map(|token| token.kind)
    }

    fn advance(&mut self) -> &Token {
        let index = self.position;
        if !self.at_end() {
            self.position += 1;
        }
        &self.tokens[index]
    }

    fn check(&self, kind: TokenKind) -> bool {
        self.peek().kind == kind
    }

    fn at_end(&self) -> bool {
        self.check(TokenKind::Eof)
    }

    fn expect(&mut self, kind: TokenKind) -> Option<&Token> {
        if self.check(kind) {
            return Some(self.advance());
        }
        let token = self.peek().clone();
        let code = if token.kind == TokenKind::Eof {
            DslErrorCode::UnexpectedEof
        } else {
            DslErrorCode::UnexpectedToken
        };
        self.error(
            code,
            format!("expected {kind:?}, got {:?}", token.value),
            token.span,
            None,
        );
        None
    }

    fn skip_to_next_line(&mut self) {
        while !self.at_end() && !self.check(TokenKind::Newline) {
            self.advance();
        }
        if self.check(TokenKind::Newline) {
            self.advance();
        }
    }

    fn previous_end(&self) -> Loc {
        self.position
            .checked_sub(1)
            .and_then(|index| self.tokens.get(index))
            .map_or(Loc::default(), |token| token.span.end)
    }

    fn error(&mut self, code: DslErrorCode, message: String, span: Span, hint: Option<String>) {
        self.errors.push(DslError {
            code,
            message,
            span,
            hint,
        });
    }
}

struct ParsedInteger {
    value: i64,
    span: Span,
}

struct ParsedBarPosition {
    value: f64,
    has_beats: bool,
    has_subdivisions: bool,
    span: Span,
}

fn blend_mode_list() -> String {
    BlendMode::ALL.map(BlendMode::name).join(", ")
}

fn arg_value_name(value: &ArgValue) -> &'static str {
    match value {
        ArgValue::Color(_) => "color",
        ArgValue::Number(_) => "number",
        ArgValue::Identifier(_) => "identifier",
        ArgValue::Json(_) => "json",
    }
}
