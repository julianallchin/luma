//! Business logic for fixture group operations.
//!
//! Handles group hierarchy building, fixture type detection, and selection query resolution.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use rand::prelude::*;
use sqlx::SqlitePool;
use tauri::{AppHandle, Manager};

use crate::database::local::fixtures as fixtures_db;
use crate::database::local::groups as groups_db;
use crate::fixtures::parser;
use crate::models::fixtures::{
    ChannelColour, ChannelType, FixtureDefinition, Mode, PatchedFixture,
};
use crate::models::groups::{
    Axis, AxisPosition, FixtureGroup, FixtureGroupNode, FixtureType, GroupWithType,
    GroupedFixtureNode, HeadNode, SelectionQuery, TypeFilter,
};

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

/// Detect fixture type from its definition
pub fn detect_fixture_type(
    app: &AppHandle,
    fixture: &PatchedFixture,
) -> Result<FixtureType, String> {
    let resource_path = resolve_fixtures_root(app)?;
    detect_fixture_type_with_path(&resource_path, fixture)
}

/// Resolve a selection query to matching fixtures
pub async fn resolve_selection_query(
    app: &AppHandle,
    pool: &SqlitePool,
    venue_id: i64,
    query: &SelectionQuery,
    rng_seed: u64,
) -> Result<Vec<PatchedFixture>, String> {
    let resource_path = resolve_fixtures_root(app)?;
    resolve_selection_query_with_path(&resource_path, pool, venue_id, query, rng_seed).await
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

/// Get groups with their computed fixture types for selection queries
async fn get_groups_with_types_with_path(
    resource_path: &PathBuf,
    pool: &SqlitePool,
    venue_id: i64,
) -> Result<Vec<GroupWithType>, String> {
    let groups = groups_db::list_groups(pool, venue_id).await?;
    let mut result = Vec::with_capacity(groups.len());

    for group in groups {
        let fixtures = groups_db::get_fixtures_in_group(pool, group.id).await?;
        let fixture_count = fixtures.len();

        // Determine dominant fixture type
        let mut type_counts: HashMap<FixtureType, usize> = HashMap::new();
        for fixture in &fixtures {
            let ft = detect_fixture_type_with_path(resource_path, fixture)?;
            *type_counts.entry(ft).or_insert(0) += 1;
        }

        let fixture_type = type_counts
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(ft, _)| ft)
            .unwrap_or(FixtureType::Unknown);

        result.push(GroupWithType {
            group,
            fixture_type,
            fixture_count,
        });
    }

    Ok(result)
}

/// Resolve a selection query to matching fixtures (PathBuf version)
pub async fn resolve_selection_query_with_path(
    resource_path: &PathBuf,
    pool: &SqlitePool,
    venue_id: i64,
    query: &SelectionQuery,
    rng_seed: u64,
) -> Result<Vec<PatchedFixture>, String> {
    let groups = get_groups_with_types_with_path(resource_path, pool, venue_id).await?;

    // Step 1: Filter by type
    let type_filtered = if let Some(type_filter) = &query.type_filter {
        resolve_type_filter(&groups, type_filter, rng_seed)
    } else {
        groups
    };

    // Step 2: Filter by spatial position
    let spatial_filtered = if let Some(spatial_filter) = &query.spatial_filter {
        resolve_spatial_filter(
            &type_filtered,
            &spatial_filter.axis,
            &spatial_filter.position,
        )
    } else {
        type_filtered
    };

    // Step 3: Collect all fixtures from matching groups
    let mut fixtures = Vec::new();
    for group_with_type in spatial_filtered {
        let group_fixtures =
            groups_db::get_fixtures_in_group(pool, group_with_type.group.id).await?;
        fixtures.extend(group_fixtures);
    }

    // Step 4: Apply amount filter
    let final_fixtures = if let Some(amount) = &query.amount {
        apply_amount_filter(fixtures, amount, rng_seed)
    } else {
        fixtures
    };

    Ok(final_fixtures)
}

// =============================================================================
// Expression-Based Selection
// =============================================================================

#[derive(Clone, Debug)]
struct FixtureCapabilities {
    has_color: bool,
    has_movement: bool,
    has_strobe: bool,
}

#[derive(Clone, Debug)]
struct FixtureInfo {
    fixture: PatchedFixture,
    fixture_type: FixtureType,
    capabilities: FixtureCapabilities,
    groups: Vec<FixtureGroup>,
}

#[derive(Clone, Debug)]
struct GroupInfo {
    group: FixtureGroup,
    is_circular: bool,
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
    group_info: &'a HashMap<i64, GroupInfo>,
    major_axis: Axis,
    minor_axis: Axis,
    rng: StdRng,
}

fn normalize_token(token: &str) -> &str {
    match token {
        "moving_spot" => "moving_head",
        _ => token,
    }
}

fn group_is_center(group: &FixtureGroup) -> bool {
    let threshold = 0.3;
    let axes = [group.axis_lr, group.axis_fb, group.axis_ab];
    axes.iter()
        .all(|value| value.map(|v| v.abs() < threshold).unwrap_or(false))
}

fn group_aligns_with_axis(group: &FixtureGroup, axis: &Axis) -> bool {
    let lr = group.axis_lr.unwrap_or(0.0).abs();
    let fb = group.axis_fb.unwrap_or(0.0).abs();
    let ab = group.axis_ab.unwrap_or(0.0).abs();

    match axis {
        Axis::Lr => lr >= fb && lr >= ab && lr > 0.0,
        Axis::Fb => fb >= lr && fb >= ab && fb > 0.0,
        Axis::Ab => ab >= lr && ab >= fb && ab > 0.0,
        _ => false,
    }
}

fn fixture_matches_token(info: &FixtureInfo, token: &str, ctx: &EvalContext<'_>) -> bool {
    match token {
        "all" => true,
        "moving_head" => info.fixture_type == FixtureType::MovingHead,
        "pixel_bar" => info.fixture_type == FixtureType::PixelBar,
        "par_wash" => info.fixture_type == FixtureType::ParWash,
        "scanner" => info.fixture_type == FixtureType::Scanner,
        "strobe" => info.fixture_type == FixtureType::Strobe,
        "static" => info.fixture_type == FixtureType::Static,
        "unknown" => info.fixture_type == FixtureType::Unknown,
        "has_color" => info.capabilities.has_color,
        "has_movement" => info.capabilities.has_movement,
        "has_strobe" => info.capabilities.has_strobe,
        "left" => info
            .groups
            .iter()
            .any(|g| g.axis_lr.map(|v| v < 0.0).unwrap_or(false)),
        "right" => info
            .groups
            .iter()
            .any(|g| g.axis_lr.map(|v| v > 0.0).unwrap_or(false)),
        "front" => info
            .groups
            .iter()
            .any(|g| g.axis_fb.map(|v| v < 0.0).unwrap_or(false)),
        "back" => info
            .groups
            .iter()
            .any(|g| g.axis_fb.map(|v| v > 0.0).unwrap_or(false)),
        "high" => info
            .groups
            .iter()
            .any(|g| g.axis_ab.map(|v| v > 0.0).unwrap_or(false)),
        "low" => info
            .groups
            .iter()
            .any(|g| g.axis_ab.map(|v| v < 0.0).unwrap_or(false)),
        "center" => info.groups.iter().any(group_is_center),
        "along_major_axis" => info
            .groups
            .iter()
            .any(|g| group_aligns_with_axis(g, &ctx.major_axis)),
        "along_minor_axis" => info
            .groups
            .iter()
            .any(|g| group_aligns_with_axis(g, &ctx.minor_axis)),
        "is_circular" => info.groups.iter().any(|g| {
            ctx.group_info
                .get(&g.id)
                .map(|info| info.is_circular)
                .unwrap_or(false)
        }),
        _ => false,
    }
}

fn eval_expr(expr: &Expr, ctx: &mut EvalContext<'_>) -> Result<HashSet<String>, String> {
    match expr {
        Expr::Token(raw_token) => {
            let token = normalize_token(raw_token);
            if token == "all" {
                return Ok(ctx.all_ids.clone());
            }
            let mut set = HashSet::new();
            for info in ctx.fixtures {
                if fixture_matches_token(info, token, ctx) {
                    set.insert(info.fixture.id.clone());
                }
            }
            if set.is_empty() && token != "all" {
                let known = [
                    "all",
                    "moving_head",
                    "moving_spot",
                    "pixel_bar",
                    "par_wash",
                    "scanner",
                    "strobe",
                    "static",
                    "unknown",
                    "has_color",
                    "has_movement",
                    "has_strobe",
                    "left",
                    "right",
                    "front",
                    "back",
                    "high",
                    "low",
                    "center",
                    "along_major_axis",
                    "along_minor_axis",
                    "is_circular",
                ];
                if !known.contains(&token) {
                    return Err(format!("Unknown token '{}'", raw_token));
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

fn detect_fixture_capabilities(definition: &FixtureDefinition, mode: &Mode) -> FixtureCapabilities {
    let mut has_color = definition.has_rgb_channels(mode) || definition.has_color_wheel(mode);
    let mut has_movement = false;
    let mut has_strobe = false;

    for mode_channel in &mode.channels {
        let channel = match definition
            .channels
            .iter()
            .find(|c| c.name == mode_channel.name)
        {
            Some(channel) => channel,
            None => continue,
        };
        match channel.get_type() {
            ChannelType::Pan | ChannelType::Tilt => has_movement = true,
            ChannelType::Shutter => has_strobe = true,
            ChannelType::Colour => has_color = true,
            ChannelType::Intensity => {
                let colour = channel.get_colour();
                if colour != ChannelColour::None {
                    has_color = true;
                }
            }
            _ => {}
        }
        if !has_strobe && channel.capabilities.iter().any(|cap| cap.is_strobe()) {
            has_strobe = true;
        }
    }

    FixtureCapabilities {
        has_color,
        has_movement,
        has_strobe,
    }
}

fn choose_mode<'a>(definition: &'a FixtureDefinition, mode_name: &str) -> Option<&'a Mode> {
    definition
        .modes
        .iter()
        .find(|m| m.name == mode_name)
        .or_else(|| definition.modes.first())
}

fn compute_group_is_circular(fixtures: &[PatchedFixture]) -> bool {
    if fixtures.len() < 3 {
        return false;
    }
    let (sum_x, sum_y) = fixtures.iter().fold((0.0, 0.0), |acc, fixture| {
        (acc.0 + fixture.pos_x, acc.1 + fixture.pos_y)
    });
    let count = fixtures.len() as f64;
    let center_x = sum_x / count;
    let center_y = sum_y / count;

    let mut radii = Vec::with_capacity(fixtures.len());
    for fixture in fixtures {
        let dx = fixture.pos_x - center_x;
        let dy = fixture.pos_y - center_y;
        radii.push((dx * dx + dy * dy).sqrt());
    }

    let mean = radii.iter().sum::<f64>() / count;
    if mean < 0.05 {
        return false;
    }
    let variance = radii.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / count;
    let std_dev = variance.sqrt();
    (std_dev / mean) < 0.2
}

fn find_major_axis_for_groups(groups: &[FixtureGroup]) -> Axis {
    let lr_spread = calculate_spread(groups, |g| g.axis_lr);
    let fb_spread = calculate_spread(groups, |g| g.axis_fb);
    let ab_spread = calculate_spread(groups, |g| g.axis_ab);

    if lr_spread >= fb_spread && lr_spread >= ab_spread {
        Axis::Lr
    } else if fb_spread >= ab_spread {
        Axis::Fb
    } else {
        Axis::Ab
    }
}

fn find_minor_axis_for_groups(groups: &[FixtureGroup]) -> Axis {
    let lr_spread = calculate_spread(groups, |g| g.axis_lr);
    let fb_spread = calculate_spread(groups, |g| g.axis_fb);
    let ab_spread = calculate_spread(groups, |g| g.axis_ab);

    if lr_spread <= fb_spread && lr_spread <= ab_spread {
        Axis::Lr
    } else if fb_spread <= ab_spread {
        Axis::Fb
    } else {
        Axis::Ab
    }
}

pub async fn resolve_selection_expression_with_path(
    resource_path: &PathBuf,
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

    let groups = groups_db::list_groups(pool, venue_id).await?;
    let major_axis = find_major_axis_for_groups(&groups);
    let minor_axis = find_minor_axis_for_groups(&groups);

    let mut group_info = HashMap::new();
    for group in &groups {
        let group_fixtures = groups_db::get_fixtures_in_group(pool, group.id).await?;
        let is_circular = compute_group_is_circular(&group_fixtures);
        group_info.insert(
            group.id,
            GroupInfo {
                group: group.clone(),
                is_circular,
            },
        );
    }

    let mut definition_cache: HashMap<String, FixtureDefinition> = HashMap::new();
    let mut fixture_info = Vec::with_capacity(fixtures.len());
    for fixture in &fixtures {
        let groups_for_fixture = groups_db::get_groups_for_fixture(pool, &fixture.id).await?;
        let def = if let Some(def) = definition_cache.get(&fixture.fixture_path) {
            Some(def.clone())
        } else {
            let def_path = resource_path.join(&fixture.fixture_path);
            match parser::parse_definition(&def_path) {
                Ok(parsed) => {
                    definition_cache.insert(fixture.fixture_path.clone(), parsed.clone());
                    Some(parsed)
                }
                Err(_) => None,
            }
        };

        let Some(definition) = def else {
            fixture_info.push(FixtureInfo {
                fixture: fixture.clone(),
                fixture_type: FixtureType::Unknown,
                capabilities: FixtureCapabilities {
                    has_color: false,
                    has_movement: false,
                    has_strobe: false,
                },
                groups: groups_for_fixture,
            });
            continue;
        };

        let Some(mode) = choose_mode(&definition, &fixture.mode_name) else {
            fixture_info.push(FixtureInfo {
                fixture: fixture.clone(),
                fixture_type: FixtureType::Unknown,
                capabilities: FixtureCapabilities {
                    has_color: false,
                    has_movement: false,
                    has_strobe: false,
                },
                groups: groups_for_fixture,
            });
            continue;
        };

        let fixture_type = FixtureType::detect(&definition, mode);
        let capabilities = detect_fixture_capabilities(&definition, mode);

        fixture_info.push(FixtureInfo {
            fixture: fixture.clone(),
            fixture_type,
            capabilities,
            groups: groups_for_fixture,
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
        group_info: &group_info,
        major_axis,
        minor_axis,
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
// Filter Implementation
// =============================================================================

/// Resolve type filter (XOR with fallback)
fn resolve_type_filter(
    groups: &[GroupWithType],
    filter: &TypeFilter,
    rng_seed: u64,
) -> Vec<GroupWithType> {
    let mut rng = StdRng::seed_from_u64(rng_seed);

    // Check which XOR types are available
    let available_xor: Vec<&FixtureType> = filter
        .xor
        .iter()
        .filter(|ft| groups.iter().any(|g| &g.fixture_type == *ft))
        .collect();

    if !available_xor.is_empty() {
        // Randomly pick one XOR type
        let chosen = available_xor[rng.gen_range(0..available_xor.len())];
        return groups
            .iter()
            .filter(|g| &g.fixture_type == chosen)
            .cloned()
            .collect();
    }

    // Try fallbacks in order
    for fallback in &filter.fallback {
        let matching: Vec<GroupWithType> = groups
            .iter()
            .filter(|g| &g.fixture_type == fallback)
            .cloned()
            .collect();

        if !matching.is_empty() {
            return matching;
        }
    }

    // No matches found
    vec![]
}

/// Resolve spatial filter
fn resolve_spatial_filter(
    groups: &[GroupWithType],
    axis: &Axis,
    position: &AxisPosition,
) -> Vec<GroupWithType> {
    // First, determine which axis to use
    let resolved_axis = match axis {
        Axis::Lr | Axis::Fb | Axis::Ab => axis.clone(),
        Axis::MajorAxis => find_major_axis(groups),
        Axis::MinorAxis => find_minor_axis(groups),
        Axis::AnyOpposing => find_opposing_axis(groups).unwrap_or(Axis::Lr),
    };

    // Get axis values for each group
    let get_axis_value = |g: &GroupWithType| -> Option<f64> {
        match resolved_axis {
            Axis::Lr => g.group.axis_lr,
            Axis::Fb => g.group.axis_fb,
            Axis::Ab => g.group.axis_ab,
            _ => None,
        }
    };

    // Filter based on position
    match position {
        AxisPosition::Positive => groups
            .iter()
            .filter(|g| get_axis_value(g).map(|v| v > 0.0).unwrap_or(false))
            .cloned()
            .collect(),
        AxisPosition::Negative => groups
            .iter()
            .filter(|g| get_axis_value(g).map(|v| v < 0.0).unwrap_or(false))
            .cloned()
            .collect(),
        AxisPosition::Center => groups
            .iter()
            .filter(|g| get_axis_value(g).map(|v| v.abs() < 0.3).unwrap_or(true))
            .cloned()
            .collect(),
        AxisPosition::Both => groups.to_vec(),
    }
}

/// Find the axis with the largest spread of groups
fn find_major_axis(groups: &[GroupWithType]) -> Axis {
    let lr_spread = calculate_spread(groups, |g| g.group.axis_lr);
    let fb_spread = calculate_spread(groups, |g| g.group.axis_fb);
    let ab_spread = calculate_spread(groups, |g| g.group.axis_ab);

    if lr_spread >= fb_spread && lr_spread >= ab_spread {
        Axis::Lr
    } else if fb_spread >= ab_spread {
        Axis::Fb
    } else {
        Axis::Ab
    }
}

/// Find the axis with the smallest spread of groups
fn find_minor_axis(groups: &[GroupWithType]) -> Axis {
    let lr_spread = calculate_spread(groups, |g| g.group.axis_lr);
    let fb_spread = calculate_spread(groups, |g| g.group.axis_fb);
    let ab_spread = calculate_spread(groups, |g| g.group.axis_ab);

    if lr_spread <= fb_spread && lr_spread <= ab_spread {
        Axis::Lr
    } else if fb_spread <= ab_spread {
        Axis::Fb
    } else {
        Axis::Ab
    }
}

/// Find any axis that has groups on both positive and negative sides
fn find_opposing_axis(groups: &[GroupWithType]) -> Option<Axis> {
    for axis in [Axis::Ab, Axis::Lr, Axis::Fb] {
        let get_value = |g: &GroupWithType| match axis {
            Axis::Lr => g.group.axis_lr,
            Axis::Fb => g.group.axis_fb,
            Axis::Ab => g.group.axis_ab,
            _ => None,
        };

        let has_positive = groups
            .iter()
            .any(|g| get_value(g).map(|v| v > 0.0).unwrap_or(false));
        let has_negative = groups
            .iter()
            .any(|g| get_value(g).map(|v| v < 0.0).unwrap_or(false));

        if has_positive && has_negative {
            return Some(axis);
        }
    }

    None
}

/// Calculate the spread (max - min) of values on an axis
fn calculate_spread<T, F>(groups: &[T], get_value: F) -> f64
where
    F: Fn(&T) -> Option<f64>,
{
    let values: Vec<f64> = groups.iter().filter_map(|g| get_value(g)).collect();

    if values.is_empty() {
        return 0.0;
    }

    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    max - min
}

/// Apply amount filter to fixtures
fn apply_amount_filter(
    fixtures: Vec<PatchedFixture>,
    amount: &crate::models::groups::AmountFilter,
    rng_seed: u64,
) -> Vec<PatchedFixture> {
    use crate::models::groups::AmountFilter;

    match amount {
        AmountFilter::All => fixtures,
        AmountFilter::Percent(pct) => {
            let count = ((fixtures.len() as f64) * (pct / 100.0)).ceil() as usize;
            let mut rng = StdRng::seed_from_u64(rng_seed);
            let mut shuffled = fixtures;
            shuffled.shuffle(&mut rng);
            shuffled.into_iter().take(count).collect()
        }
        AmountFilter::Count(n) => {
            let mut rng = StdRng::seed_from_u64(rng_seed);
            let mut shuffled = fixtures;
            shuffled.shuffle(&mut rng);
            shuffled.into_iter().take(*n).collect()
        }
        AmountFilter::EveryOther => fixtures.into_iter().step_by(2).collect(),
    }
}

// =============================================================================
// Helpers
// =============================================================================

pub fn resolve_fixtures_root(app: &AppHandle) -> Result<PathBuf, String> {
    let resource_path = app
        .path()
        .resource_dir()
        .map(|p| p.join("resources/fixtures/2511260420"))
        .unwrap_or_else(|_| PathBuf::from("resources/fixtures/2511260420"));

    if resource_path.exists() {
        return Ok(resource_path);
    }

    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let dev_path = cwd.join("../resources/fixtures/2511260420");
    if dev_path.exists() {
        return Ok(dev_path);
    }

    Ok(cwd.join("resources/fixtures/2511260420"))
}
