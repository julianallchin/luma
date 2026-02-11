//! Business logic for fixture group operations.
//!
//! Handles group hierarchy building, fixture type detection, and tag expression resolution.

use std::collections::HashSet;
use std::path::PathBuf;

use rand::prelude::*;
use sqlx::SqlitePool;
use tauri::AppHandle;

use crate::database::local::fixtures as fixtures_db;
use crate::database::local::groups as groups_db;
use crate::fixtures::parser;
use crate::models::fixtures::PatchedFixture;
use crate::models::groups::{FixtureGroupNode, FixtureType, GroupedFixtureNode, HeadNode};

// =============================================================================
// Public API (AppHandle versions - for Tauri commands)
// =============================================================================

/// Get grouped hierarchy for a venue: Groups -> Fixtures -> Heads
pub async fn get_grouped_hierarchy(
    app: &AppHandle,
    pool: &SqlitePool,
    venue_id: i64,
) -> Result<Vec<FixtureGroupNode>, String> {
    let resource_path = resolve_fixtures_root(app)?;
    get_grouped_hierarchy_with_path(&resource_path, pool, venue_id).await
}

// =============================================================================
// Internal API (PathBuf versions - for node graph execution)
// =============================================================================

/// Get grouped hierarchy for a venue: Groups -> Fixtures -> Heads
pub async fn get_grouped_hierarchy_with_path(
    resource_path: &PathBuf,
    pool: &SqlitePool,
    venue_id: i64,
) -> Result<Vec<FixtureGroupNode>, String> {
    let groups = groups_db::list_groups(pool, venue_id).await?;

    let mut result = Vec::with_capacity(groups.len());

    for group in groups {
        let fixtures = groups_db::get_fixtures_in_group(pool, group.id).await?;
        let mut grouped_fixtures = Vec::with_capacity(fixtures.len());
        let mut group_fixture_type = FixtureType::Unknown;

        for fixture in fixtures {
            let fixture_type = detect_fixture_type_with_path(resource_path, &fixture)?;

            // Track the dominant type for the group
            if group_fixture_type == FixtureType::Unknown {
                group_fixture_type = fixture_type.clone();
            }

            let heads = get_fixture_heads_with_path(resource_path, &fixture);

            grouped_fixtures.push(GroupedFixtureNode {
                id: fixture.id.clone(),
                label: fixture
                    .label
                    .clone()
                    .unwrap_or_else(|| format!("{} {}", fixture.manufacturer, fixture.model)),
                fixture_type,
                heads,
            });
        }

        result.push(FixtureGroupNode {
            group_id: group.id,
            group_name: group.name.clone(),
            fixture_type: group_fixture_type,
            axis_lr: group.axis_lr,
            axis_fb: group.axis_fb,
            axis_ab: group.axis_ab,
            tags: group.tags.clone(),
            fixtures: grouped_fixtures,
        });
    }

    Ok(result)
}

/// Detect fixture type from its definition (PathBuf version)
pub fn detect_fixture_type_with_path(
    resource_path: &PathBuf,
    fixture: &PatchedFixture,
) -> Result<FixtureType, String> {
    let def_path = resource_path.join(&fixture.fixture_path);

    let def = match parser::parse_definition(&def_path) {
        Ok(d) => d,
        Err(_) => return Ok(FixtureType::Unknown),
    };

    if let Some(mode) = def.modes.iter().find(|m| m.name == fixture.mode_name) {
        Ok(FixtureType::detect(&def, mode))
    } else if let Some(mode) = def.modes.first() {
        Ok(FixtureType::detect(&def, mode))
    } else {
        Ok(FixtureType::Unknown)
    }
}

/// Get heads for a fixture (PathBuf version)
fn get_fixture_heads_with_path(resource_path: &PathBuf, fixture: &PatchedFixture) -> Vec<HeadNode> {
    let def_path = resource_path.join(&fixture.fixture_path);

    let mut heads = Vec::new();

    if let Ok(def) = parser::parse_definition(&def_path) {
        if let Some(mode) = def.modes.iter().find(|m| m.name == fixture.mode_name) {
            if !mode.heads.is_empty() {
                for (i, _head) in mode.heads.iter().enumerate() {
                    heads.push(HeadNode {
                        id: format!("{}:{}", fixture.id, i),
                        label: format!("Head {}", i + 1),
                    });
                }
            }
        }
    }

    heads
}

// =============================================================================
// Expression-Based Selection
// =============================================================================

#[derive(Clone, Debug)]
struct FixtureInfo {
    fixture: PatchedFixture,
    tags: HashSet<String>,
}

#[derive(Clone, Debug)]
enum Expr {
    Token(String),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Xor(Box<Expr>, Box<Expr>),
    Fallback(Box<Expr>, Box<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
enum LexToken {
    Ident(String),
    Or,
    And,
    Xor,
    Not,
    Fallback,
    LParen,
    RParen,
    End,
}

struct Lexer<'a> {
    chars: Vec<char>,
    pos: usize,
    input: &'a str,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
            input,
        }
    }

    fn next_token(&mut self) -> Result<LexToken, String> {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
        if self.pos >= self.chars.len() {
            return Ok(LexToken::End);
        }

        let c = self.chars[self.pos];
        match c {
            '|' => {
                self.pos += 1;
                Ok(LexToken::Or)
            }
            '&' => {
                self.pos += 1;
                Ok(LexToken::And)
            }
            '^' => {
                self.pos += 1;
                Ok(LexToken::Xor)
            }
            '~' => {
                self.pos += 1;
                Ok(LexToken::Not)
            }
            '>' => {
                self.pos += 1;
                Ok(LexToken::Fallback)
            }
            '(' => {
                self.pos += 1;
                Ok(LexToken::LParen)
            }
            ')' => {
                self.pos += 1;
                Ok(LexToken::RParen)
            }
            _ => {
                if c.is_ascii_alphanumeric() || c == '_' {
                    let start = self.pos;
                    while self.pos < self.chars.len()
                        && (self.chars[self.pos].is_ascii_alphanumeric()
                            || self.chars[self.pos] == '_')
                    {
                        self.pos += 1;
                    }
                    let ident = &self.input[start..self.pos];
                    Ok(LexToken::Ident(ident.to_lowercase()))
                } else {
                    Err(format!("Unexpected character '{}' in selection query", c))
                }
            }
        }
    }
}

struct Parser {
    tokens: Vec<LexToken>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<LexToken>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> &LexToken {
        self.tokens.get(self.pos).unwrap_or(&LexToken::End)
    }

    fn consume(&mut self) -> LexToken {
        let tok = self.peek().clone();
        self.pos += 1;
        tok
    }

    fn expect(&mut self, expected: LexToken) -> Result<(), String> {
        let tok = self.consume();
        if tok == expected {
            Ok(())
        } else {
            Err(format!("Expected {:?}, found {:?}", expected, tok))
        }
    }

    fn parse_expression(&mut self) -> Result<Expr, String> {
        self.parse_fallback()
    }

    fn parse_fallback(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_union()?;
        while matches!(self.peek(), LexToken::Fallback) {
            self.consume();
            let right = self.parse_union()?;
            expr = Expr::Fallback(Box::new(expr), Box::new(right));
        }
        Ok(expr)
    }

    fn parse_union(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_xor()?;
        while matches!(self.peek(), LexToken::Or) {
            self.consume();
            let right = self.parse_xor()?;
            expr = Expr::Or(Box::new(expr), Box::new(right));
        }
        Ok(expr)
    }

    fn parse_xor(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_and()?;
        while matches!(self.peek(), LexToken::Xor) {
            self.consume();
            let right = self.parse_and()?;
            expr = Expr::Xor(Box::new(expr), Box::new(right));
        }
        Ok(expr)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_unary()?;
        while matches!(self.peek(), LexToken::And) {
            self.consume();
            let right = self.parse_unary()?;
            expr = Expr::And(Box::new(expr), Box::new(right));
        }
        Ok(expr)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        if matches!(self.peek(), LexToken::Not) {
            self.consume();
            let inner = self.parse_unary()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.consume() {
            LexToken::Ident(name) => Ok(Expr::Token(name)),
            LexToken::LParen => {
                let expr = self.parse_expression()?;
                self.expect(LexToken::RParen)?;
                Ok(expr)
            }
            LexToken::End => Err("Unexpected end of selection query".into()),
            tok => Err(format!("Unexpected token {:?}", tok)),
        }
    }
}

fn parse_selection_expression(input: &str) -> Result<Expr, String> {
    let mut lexer = Lexer::new(input);
    let mut tokens = Vec::new();
    loop {
        let tok = lexer.next_token()?;
        if tok == LexToken::End {
            tokens.push(tok);
            break;
        }
        tokens.push(tok);
    }
    let mut parser = Parser::new(tokens);
    let expr = parser.parse_expression()?;
    if !matches!(parser.peek(), LexToken::End) {
        return Err("Unexpected token after selection query".into());
    }
    Ok(expr)
}

struct EvalContext<'a> {
    fixtures: &'a [FixtureInfo],
    all_ids: HashSet<String>,
    rng: StdRng,
}

fn fixture_matches_token(info: &FixtureInfo, token: &str) -> bool {
    token == "all" || info.tags.contains(token)
}

fn eval_expr(expr: &Expr, ctx: &mut EvalContext<'_>) -> Result<HashSet<String>, String> {
    match expr {
        Expr::Token(token) => {
            if token == "all" {
                return Ok(ctx.all_ids.clone());
            }
            let mut set = HashSet::new();
            for info in ctx.fixtures {
                if fixture_matches_token(info, token) {
                    set.insert(info.fixture.id.clone());
                }
            }
            Ok(set)
        }
        Expr::Not(inner) => {
            let inner_set = eval_expr(inner, ctx)?;
            let mut result = ctx.all_ids.clone();
            result.retain(|id| !inner_set.contains(id));
            Ok(result)
        }
        Expr::And(a, b) => {
            let left = eval_expr(a, ctx)?;
            let right = eval_expr(b, ctx)?;
            let result = left.intersection(&right).cloned().collect::<HashSet<_>>();
            Ok(result)
        }
        Expr::Or(a, b) => {
            let mut left = eval_expr(a, ctx)?;
            let right = eval_expr(b, ctx)?;
            left.extend(right);
            Ok(left)
        }
        Expr::Xor(a, b) => {
            let left = eval_expr(a, ctx)?;
            let right = eval_expr(b, ctx)?;
            if left.is_empty() && right.is_empty() {
                return Ok(HashSet::new());
            }
            if left.is_empty() {
                return Ok(right);
            }
            if right.is_empty() {
                return Ok(left);
            }
            let pick_left = ctx.rng.gen_bool(0.5);
            Ok(if pick_left { left } else { right })
        }
        Expr::Fallback(a, b) => {
            let left = eval_expr(a, ctx)?;
            if !left.is_empty() {
                return Ok(left);
            }
            eval_expr(b, ctx)
        }
    }
}

/// Resolve a tag expression to matching fixtures
pub async fn resolve_selection_expression_with_path(
    _resource_path: &PathBuf,
    pool: &SqlitePool,
    venue_id: i64,
    expression: &str,
    rng_seed: u64,
) -> Result<Vec<PatchedFixture>, String> {
    let trimmed = expression.trim();
    let fixtures = fixtures_db::get_patched_fixtures(pool, venue_id).await?;
    if fixtures.is_empty() {
        return Ok(vec![]);
    }

    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("all") {
        return Ok(fixtures);
    }

    let mut fixture_info = Vec::with_capacity(fixtures.len());
    for fixture in &fixtures {
        let groups_for_fixture = groups_db::get_groups_for_fixture(pool, &fixture.id).await?;
        let tags: HashSet<String> = groups_for_fixture
            .iter()
            .flat_map(|g| g.tags.iter().cloned())
            .collect();

        fixture_info.push(FixtureInfo {
            fixture: fixture.clone(),
            tags,
        });
    }

    let expr = parse_selection_expression(trimmed)?;
    let all_ids = fixtures
        .iter()
        .map(|fixture| fixture.id.clone())
        .collect::<HashSet<_>>();
    let mut ctx = EvalContext {
        fixtures: &fixture_info,
        all_ids,
        rng: StdRng::seed_from_u64(rng_seed),
    };
    let selected_ids = eval_expr(&expr, &mut ctx)?;
    let result = fixtures
        .into_iter()
        .filter(|fixture| selected_ids.contains(&fixture.id))
        .collect();
    Ok(result)
}

// =============================================================================
// Helpers
// =============================================================================

pub fn resolve_fixtures_root(app: &AppHandle) -> Result<PathBuf, String> {
    crate::services::fixtures::resolve_fixtures_root(app)
}
