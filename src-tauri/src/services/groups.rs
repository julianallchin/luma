//! Business logic for fixture group operations.
//!
//! Handles group hierarchy building, fixture type detection, and selection query resolution.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use rand::prelude::*;
use sqlx::SqlitePool;
use tauri::AppHandle;

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

/// Get groups with their computed fixture types for selection queries
async fn get_groups_with_types_with_path(
    resource_path: &PathBuf,
    pool: &SqlitePool,
    venue_id: i64,
) -> Result<(Vec<GroupWithType>, HashMap<i64, (f64, f64, f64)>), String> {
    let groups = groups_db::list_groups(pool, venue_id).await?;
    let mut result = Vec::with_capacity(groups.len());
    let mut group_centroids: HashMap<i64, (f64, f64, f64)> = HashMap::new();

    for group in groups {
        let fixtures = groups_db::get_fixtures_in_group(pool, group.id).await?;
        let fixture_count = fixtures.len();

        if !fixtures.is_empty() {
            let (sum_x, sum_y, sum_z) =
                fixtures
                    .iter()
                    .fold((0.0, 0.0, 0.0), |(sx, sy, sz), fixture| {
                        (sx + fixture.pos_x, sy + fixture.pos_y, sz + fixture.pos_z)
                    });
            let count = fixtures.len() as f64;
            group_centroids.insert(group.id, (sum_x / count, sum_y / count, sum_z / count));
        }

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

    Ok((result, group_centroids))
}

/// Resolve a selection query to matching fixtures (PathBuf version)
pub async fn resolve_selection_query_with_path(
    resource_path: &PathBuf,
    pool: &SqlitePool,
    venue_id: i64,
    query: &SelectionQuery,
    rng_seed: u64,
) -> Result<Vec<PatchedFixture>, String> {
    let (groups, group_centroids) =
        get_groups_with_types_with_path(resource_path, pool, venue_id).await?;
    let group_list: Vec<FixtureGroup> = groups.iter().map(|g| g.group.clone()).collect();
    let axis_map = build_group_axis_map(&group_list, &group_centroids);

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
            &axis_map,
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
    capabilities: FixtureCapabilities,
    tags: HashSet<String>,
}

#[derive(Clone, Copy, Debug, Default)]
struct GroupAxis {
    lr: Option<f64>,
    fb: Option<f64>,
    ab: Option<f64>,
}

impl GroupAxis {
    fn value(&self, axis: &Axis) -> Option<f64> {
        match axis {
            Axis::Lr => self.lr,
            Axis::Fb => self.fb,
            Axis::Ab => self.ab,
            _ => None,
        }
    }
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

fn normalize_token(token: &str) -> &str {
    match token {
        "moving_spot" => "moving_head",
        _ => token,
    }
}

fn normalize_axis_value(value: f64, min: f64, max: f64) -> f64 {
    let span = max - min;
    if span.abs() <= f64::EPSILON {
        0.0
    } else {
        ((value - min) / span) * 2.0 - 1.0
    }
}

fn build_group_axis_map(
    groups: &[FixtureGroup],
    centroids: &HashMap<i64, (f64, f64, f64)>,
) -> HashMap<i64, GroupAxis> {
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut min_z = f64::INFINITY;
    let mut max_z = f64::NEG_INFINITY;

    for (_, (x, y, z)) in centroids {
        min_x = min_x.min(*x);
        max_x = max_x.max(*x);
        min_y = min_y.min(*y);
        max_y = max_y.max(*y);
        min_z = min_z.min(*z);
        max_z = max_z.max(*z);
    }

    let has_centroids = !centroids.is_empty();
    if !has_centroids {
        min_x = 0.0;
        max_x = 0.0;
        min_y = 0.0;
        max_y = 0.0;
        min_z = 0.0;
        max_z = 0.0;
    }

    let mut axis_map = HashMap::with_capacity(groups.len());
    for group in groups {
        let fallback = centroids.get(&group.id).map(|(x, y, z)| GroupAxis {
            lr: Some(normalize_axis_value(*x, min_x, max_x)),
            fb: Some(normalize_axis_value(*y, min_y, max_y)),
            ab: Some(normalize_axis_value(*z, min_z, max_z)),
        });

        let axis = GroupAxis {
            lr: group.axis_lr.or_else(|| fallback.and_then(|f| f.lr)),
            fb: group.axis_fb.or_else(|| fallback.and_then(|f| f.fb)),
            ab: group.axis_ab.or_else(|| fallback.and_then(|f| f.ab)),
        };
        axis_map.insert(group.id, axis);
    }

    axis_map
}

fn fixture_matches_token(info: &FixtureInfo, token: &str, _ctx: &EvalContext<'_>) -> bool {
    // Primary matching: check if fixture has this tag
    if info.tags.contains(token) {
        return true;
    }

    // Fallback to capability-based matching for has_* tokens
    match token {
        "all" => true,
        "has_color" => info.capabilities.has_color,
        "has_movement" => info.capabilities.has_movement,
        "has_strobe" => info.capabilities.has_strobe,
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
            // Tags are user-defined, so empty results are valid (no error for unknown tokens)
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

        // Get tags for this fixture from its groups
        let tags: HashSet<String> = groups_for_fixture
            .iter()
            .flat_map(|g| g.tags.iter().cloned())
            .collect();

        let capabilities = def
            .as_ref()
            .and_then(|d| {
                choose_mode(d, &fixture.mode_name).map(|m| detect_fixture_capabilities(d, m))
            })
            .unwrap_or(FixtureCapabilities {
                has_color: false,
                has_movement: false,
                has_strobe: false,
            });

        fixture_info.push(FixtureInfo {
            fixture: fixture.clone(),
            capabilities,
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
    axis_map: &HashMap<i64, GroupAxis>,
    axis: &Axis,
    position: &AxisPosition,
) -> Vec<GroupWithType> {
    // First, determine which axis to use
    let resolved_axis = match axis {
        Axis::Lr | Axis::Fb | Axis::Ab => axis.clone(),
        Axis::MajorAxis => find_major_axis(groups, axis_map),
        Axis::MinorAxis => find_minor_axis(groups, axis_map),
        Axis::AnyOpposing => find_opposing_axis(groups, axis_map).unwrap_or(Axis::Lr),
    };

    // Get axis values for each group
    let get_axis_value = |g: &GroupWithType| -> Option<f64> {
        axis_map
            .get(&g.group.id)
            .and_then(|axis| axis.value(&resolved_axis))
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
fn find_major_axis(groups: &[GroupWithType], axis_map: &HashMap<i64, GroupAxis>) -> Axis {
    let lr_spread = calculate_spread(groups, |g| {
        axis_map.get(&g.group.id).and_then(|a| a.value(&Axis::Lr))
    });
    let fb_spread = calculate_spread(groups, |g| {
        axis_map.get(&g.group.id).and_then(|a| a.value(&Axis::Fb))
    });
    let ab_spread = calculate_spread(groups, |g| {
        axis_map.get(&g.group.id).and_then(|a| a.value(&Axis::Ab))
    });

    if lr_spread >= fb_spread && lr_spread >= ab_spread {
        Axis::Lr
    } else if fb_spread >= ab_spread {
        Axis::Fb
    } else {
        Axis::Ab
    }
}

/// Find the axis with the smallest spread of groups
fn find_minor_axis(groups: &[GroupWithType], axis_map: &HashMap<i64, GroupAxis>) -> Axis {
    let lr_spread = calculate_spread(groups, |g| {
        axis_map.get(&g.group.id).and_then(|a| a.value(&Axis::Lr))
    });
    let fb_spread = calculate_spread(groups, |g| {
        axis_map.get(&g.group.id).and_then(|a| a.value(&Axis::Fb))
    });
    let ab_spread = calculate_spread(groups, |g| {
        axis_map.get(&g.group.id).and_then(|a| a.value(&Axis::Ab))
    });

    if lr_spread <= fb_spread && lr_spread <= ab_spread {
        Axis::Lr
    } else if fb_spread <= ab_spread {
        Axis::Fb
    } else {
        Axis::Ab
    }
}

/// Find any axis that has groups on both positive and negative sides
fn find_opposing_axis(
    groups: &[GroupWithType],
    axis_map: &HashMap<i64, GroupAxis>,
) -> Option<Axis> {
    for axis in [Axis::Ab, Axis::Lr, Axis::Fb] {
        let get_value = |g: &GroupWithType| axis_map.get(&g.group.id).and_then(|a| a.value(&axis));

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
    crate::services::fixtures::resolve_fixtures_root(app)
}
