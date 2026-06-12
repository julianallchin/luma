//! Business logic for fixture group operations.
//!
//! Handles group hierarchy building, fixture type detection, and tag expression resolution.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use once_cell::sync::Lazy;
use rand::prelude::*;
use sqlx::SqlitePool;
use tauri::AppHandle;
use tokio::sync::Mutex as TokioMutex;

use crate::database::local::fixtures as fixtures_db;
use crate::database::local::groups as groups_db;
use crate::fixtures::layout::{compute_head_offsets, head_world_position};
use crate::fixtures::parser;
use crate::models::fixtures::PatchedFixture;
use crate::models::groups::{
    normalize_group_name, FixtureGroupNode, FixtureType, GroupedFixtureNode, HeadNode,
};

/// Cached fixture + group data for a venue, shared across concurrent graph executions.
#[derive(Clone)]
struct CachedVenueFixtures {
    fixtures: Vec<PatchedFixture>,
    fixture_info: Vec<FixtureInfo>,
}

/// Per-venue cache with loading locks to prevent thundering herd.
/// The `data` map holds completed results; `loading` holds per-key async mutexes
/// so only one task loads for a given venue while others wait.
struct VenueFixtureCacheInner {
    data: HashMap<String, Arc<CachedVenueFixtures>>,
    loading: HashMap<String, Arc<TokioMutex<()>>>,
}

static VENUE_FIXTURE_CACHE: Lazy<std::sync::Mutex<VenueFixtureCacheInner>> = Lazy::new(|| {
    std::sync::Mutex::new(VenueFixtureCacheInner {
        data: HashMap::new(),
        loading: HashMap::new(),
    })
});

/// Invalidate the venue fixture cache (call when fixtures/groups change).
pub fn invalidate_venue_fixture_cache() {
    if let Ok(mut inner) = VENUE_FIXTURE_CACHE.lock() {
        inner.data.clear();
    }
}

/// Invalidate cache for a specific venue.
pub fn invalidate_venue_fixture_cache_for(venue_id: &str) {
    if let Ok(mut inner) = VENUE_FIXTURE_CACHE.lock() {
        inner.data.remove(venue_id);
    }
}

async fn get_cached_venue_fixtures(
    resource_path: &PathBuf,
    pool: &SqlitePool,
    venue_id: &str,
) -> Result<Arc<CachedVenueFixtures>, String> {
    // Fast path: check data cache (sync mutex, instant)
    {
        let inner = VENUE_FIXTURE_CACHE.lock().unwrap();
        if let Some(cached) = inner.data.get(venue_id) {
            return Ok(cached.clone());
        }
    }

    // Get or create a per-venue loading lock
    let lock = {
        let mut inner = VENUE_FIXTURE_CACHE.lock().unwrap();
        inner
            .loading
            .entry(venue_id.to_string())
            .or_insert_with(|| Arc::new(TokioMutex::new(())))
            .clone()
    };

    // Only one task loads per venue; others wait here then read from cache
    let _guard = lock.lock().await;

    // Check again — another task may have loaded while we waited
    {
        let inner = VENUE_FIXTURE_CACHE.lock().unwrap();
        if let Some(cached) = inner.data.get(venue_id) {
            return Ok(cached.clone());
        }
    }

    // We're the loader. Always clean up the loading lock, even on error.
    let result = async {
        let fixtures = fixtures_db::get_patched_fixtures(pool, venue_id).await?;
        let memberships = groups_db::get_venue_memberships(pool, venue_id).await?;

        // fixture_id → (normalized group name → which heads are members)
        let mut by_fixture: HashMap<String, HashMap<String, HeadMembership>> = HashMap::new();
        for (fixture_id, group_name, head_index) in memberships {
            let Some(name) = group_name.as_deref().map(normalize_group_name) else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            let entry = by_fixture
                .entry(fixture_id)
                .or_default()
                .entry(name)
                .or_insert_with(|| HeadMembership::Heads(HashSet::new()));
            if head_index < 0 {
                *entry = HeadMembership::All;
            } else if let HeadMembership::Heads(heads) = entry {
                heads.insert(head_index as usize);
            }
        }

        let mut fixture_info = Vec::with_capacity(fixtures.len());
        for fixture in &fixtures {
            fixture_info.push(FixtureInfo {
                head_count: head_count_with_path(resource_path, fixture),
                group_heads: by_fixture.remove(&fixture.id).unwrap_or_default(),
                fixture: fixture.clone(),
            });
        }

        Ok::<_, String>(Arc::new(CachedVenueFixtures {
            fixtures,
            fixture_info,
        }))
    }
    .await;

    // Clean up loading lock regardless of success/failure
    {
        let mut inner = VENUE_FIXTURE_CACHE.lock().unwrap();
        inner.loading.remove(venue_id);
        if let Ok(ref cached) = result {
            inner.data.insert(venue_id.to_string(), cached.clone());
        }
    }

    result
}

// =============================================================================
// Public API (AppHandle versions - for Tauri commands)
// =============================================================================

/// Get grouped hierarchy for a venue: Groups -> Fixtures -> Heads
pub async fn get_grouped_hierarchy(
    app: &AppHandle,
    pool: &SqlitePool,
    venue_id: &str,
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
    venue_id: &str,
) -> Result<Vec<FixtureGroupNode>, String> {
    let groups = groups_db::list_groups(pool, venue_id).await?;

    let mut result = Vec::with_capacity(groups.len());

    for group in groups {
        let members = groups_db::get_members_in_group(pool, &group.id).await?;

        // Fold per-head rows into one node per fixture, preserving member order.
        // `None` heads = whole-fixture membership.
        let mut order: Vec<String> = Vec::new();
        let mut folded: HashMap<String, (PatchedFixture, Option<Vec<i64>>)> = HashMap::new();
        for member in members {
            let id = member.fixture.id.clone();
            if !folded.contains_key(&id) {
                order.push(id.clone());
                folded.insert(id.clone(), (member.fixture, Some(Vec::new())));
            }
            let entry = folded.get_mut(&id).unwrap();
            if member.head_index < 0 {
                entry.1 = None;
            } else if let Some(heads) = entry.1.as_mut() {
                heads.push(member.head_index);
            }
        }

        let mut grouped_fixtures = Vec::with_capacity(order.len());
        let mut group_fixture_type = FixtureType::Unknown;

        for fixture_id in order {
            let (fixture, member_heads) = folded.remove(&fixture_id).unwrap();
            let fixture_type = detect_fixture_type_with_path(resource_path, &fixture)?;

            // Track the dominant type for the group
            if group_fixture_type == FixtureType::Unknown {
                group_fixture_type = fixture_type.clone();
            }

            let all_heads = get_fixture_heads_with_path(resource_path, &fixture);
            let head_count = all_heads.len() as i64;
            let heads = match member_heads {
                None => all_heads,
                Some(indices) => all_heads
                    .into_iter()
                    .filter(|h| indices.contains(&h.head_index))
                    .collect(),
            };

            grouped_fixtures.push(GroupedFixtureNode {
                id: fixture.id.clone(),
                label: fixture
                    .label
                    .clone()
                    .unwrap_or_else(|| format!("{} {}", fixture.manufacturer, fixture.model)),
                fixture_type,
                heads,
                head_count,
            });
        }

        result.push(FixtureGroupNode {
            group_id: group.id,
            group_name: group.name.clone(),
            fixture_type: group_fixture_type,
            axis_lr: group.axis_lr,
            axis_fb: group.axis_fb,
            axis_ab: group.axis_ab,
            movement_config: group.movement_config.clone(),
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

/// Get all heads for a fixture with their world positions (PathBuf version).
/// Empty when the mode defines no heads.
fn get_fixture_heads_with_path(resource_path: &PathBuf, fixture: &PatchedFixture) -> Vec<HeadNode> {
    let def_path = resource_path.join(&fixture.fixture_path);

    let Ok(def) = parser::parse_definition(&def_path) else {
        return Vec::new();
    };
    let Some(mode) = def.modes.iter().find(|m| m.name == fixture.mode_name) else {
        return Vec::new();
    };
    if mode.heads.is_empty() {
        return Vec::new();
    }

    let offsets = compute_head_offsets(&def, &fixture.mode_name);
    let base = [
        fixture.pos_x as f32,
        fixture.pos_y as f32,
        fixture.pos_z as f32,
    ];
    let rot = [fixture.rot_x, fixture.rot_y, fixture.rot_z];

    offsets
        .iter()
        .enumerate()
        .map(|(i, offset)| HeadNode {
            id: format!("{}:{}", fixture.id, i),
            label: format!("Head {}", i + 1),
            head_index: i as i64,
            position: head_world_position(base, rot, *offset),
        })
        .collect()
}

/// Number of heads a fixture's mode defines, floored at 1 — the eval engine
/// always emits at least one primitive ("{id}:0") per fixture.
fn head_count_with_path(resource_path: &PathBuf, fixture: &PatchedFixture) -> usize {
    let def_path = resource_path.join(&fixture.fixture_path);
    parser::parse_definition(&def_path)
        .ok()
        .and_then(|def| {
            def.modes
                .iter()
                .find(|m| m.name == fixture.mode_name)
                .map(|m| m.heads.len())
        })
        .unwrap_or(0)
        .max(1)
}

// =============================================================================
// Expression-Based Selection
// =============================================================================

/// Which heads of a fixture belong to one group.
#[derive(Clone, Debug)]
enum HeadMembership {
    /// Whole-fixture membership (head_index = -1 row).
    All,
    /// Explicit per-head rows.
    Heads(HashSet<usize>),
}

#[derive(Clone, Debug)]
struct FixtureInfo {
    fixture: PatchedFixture,
    /// Heads the fixture's mode defines, floored at 1 (eval's primitive universe).
    head_count: usize,
    /// Normalized group name → member heads.
    group_heads: HashMap<String, HeadMembership>,
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

/// Selection sets are `(fixture index, head index)` pairs — the atom of
/// targeting is a head, so boolean algebra (esp. negation and intersection)
/// stays exact when groups contain partial fixtures.
type HeadSet = HashSet<(usize, usize)>;

struct EvalContext<'a> {
    fixtures: &'a [FixtureInfo],
    all_heads: HeadSet,
    rng: StdRng,
}

fn eval_expr(expr: &Expr, ctx: &mut EvalContext<'_>) -> Result<HeadSet, String> {
    match expr {
        Expr::Token(token) => {
            if token == "all" {
                return Ok(ctx.all_heads.clone());
            }
            let mut set = HeadSet::new();
            for (fi, info) in ctx.fixtures.iter().enumerate() {
                match info.group_heads.get(token) {
                    Some(HeadMembership::All) => {
                        set.extend((0..info.head_count).map(|h| (fi, h)));
                    }
                    Some(HeadMembership::Heads(heads)) => {
                        // Ignore stale rows pointing past the current mode's head count.
                        set.extend(
                            heads
                                .iter()
                                .filter(|&&h| h < info.head_count)
                                .map(|&h| (fi, h)),
                        );
                    }
                    None => {}
                }
            }
            Ok(set)
        }
        Expr::Not(inner) => {
            let inner_set = eval_expr(inner, ctx)?;
            let mut result = ctx.all_heads.clone();
            result.retain(|pair| !inner_set.contains(pair));
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

/// A fixture matched by a selection expression, with the heads it matched on.
#[derive(Clone, Debug)]
pub struct ResolvedFixture {
    pub fixture: PatchedFixture,
    /// Matched head indices, ascending. `None` = every head of the fixture.
    pub heads: Option<Vec<usize>>,
}

/// Resolve a tag expression to matching fixtures/heads, in venue fixture order.
pub async fn resolve_selection_expression_with_path(
    resource_path: &PathBuf,
    pool: &SqlitePool,
    venue_id: &str,
    expression: &str,
    rng_seed: u64,
) -> Result<Vec<ResolvedFixture>, String> {
    let trimmed = expression.trim();
    let cached = get_cached_venue_fixtures(resource_path, pool, venue_id).await?;

    if cached.fixtures.is_empty() {
        return Ok(vec![]);
    }

    let whole_venue = |fixtures: &[PatchedFixture]| {
        fixtures
            .iter()
            .map(|fixture| ResolvedFixture {
                fixture: fixture.clone(),
                heads: None,
            })
            .collect::<Vec<_>>()
    };

    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("all") {
        return Ok(whole_venue(&cached.fixtures));
    }

    let expr = parse_selection_expression(trimmed)?;
    let all_heads: HeadSet = cached
        .fixture_info
        .iter()
        .enumerate()
        .flat_map(|(fi, info)| (0..info.head_count).map(move |h| (fi, h)))
        .collect();
    let mut ctx = EvalContext {
        fixtures: &cached.fixture_info,
        all_heads,
        rng: StdRng::seed_from_u64(rng_seed),
    };
    let selected = eval_expr(&expr, &mut ctx)?;

    let mut result = Vec::new();
    for (fi, info) in cached.fixture_info.iter().enumerate() {
        let heads: Vec<usize> = (0..info.head_count)
            .filter(|&h| selected.contains(&(fi, h)))
            .collect();
        if heads.is_empty() {
            continue;
        }
        result.push(ResolvedFixture {
            fixture: info.fixture.clone(),
            heads: if heads.len() == info.head_count {
                None
            } else {
                Some(heads)
            },
        });
    }
    Ok(result)
}

// =============================================================================
// Head membership
// =============================================================================

/// Remove one head of a fixture from a group. If the fixture is in the group
/// whole (head_index = -1), the membership is split into explicit rows for the
/// remaining heads; otherwise the head's own row is deleted.
pub async fn remove_head_from_group(
    resource_path: &PathBuf,
    pool: &SqlitePool,
    fixture_id: &str,
    group_id: &str,
    head_index: i64,
) -> Result<(), String> {
    let fixture = fixtures_db::get_fixture(pool, fixture_id).await?;
    let head_count = head_count_with_path(resource_path, &fixture) as i64;
    let keep: Vec<i64> = (0..head_count).filter(|&h| h != head_index).collect();

    if groups_db::split_whole_fixture_membership(pool, fixture_id, group_id, &keep).await? {
        return Ok(());
    }
    groups_db::remove_member_from_group(pool, fixture_id, group_id, Some(head_index)).await
}

// =============================================================================
// Helpers
// =============================================================================

pub fn resolve_fixtures_root(app: &AppHandle) -> Result<PathBuf, String> {
    crate::services::fixtures::resolve_fixtures_root(app)
}
