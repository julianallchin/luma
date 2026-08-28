//! Business logic for fixture group operations.
//!
//! Handles group hierarchy building, fixture type detection, and tag expression resolution.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use once_cell::sync::Lazy;
use rand::prelude::*;
use tokio::sync::Mutex as TokioMutex;

use crate::database::local::fixtures as fixtures_db;
use crate::database::local::groups as groups_db;
use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::fixtures::layout::{compute_head_offsets, head_world_position};
use crate::fixtures::parser;
use crate::models::fixtures::PatchedFixture;
use crate::models::groups::{
    normalize_group_name, FixtureGroupNode, FixtureType, GroupedFixtureNode, HeadNode,
};
use crate::models::selection::{Selection, Subset};

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

/// This cache's identity for one venue: the library it lives in, and its id.
///
/// A venue id is unique *within* a library, not across them — the app database
/// and a scratch copy of it can both hold `venue-main`. On the id alone those
/// are one entry, and whichever venue loads first serves its rig to every other
/// venue that shares the id.
///
/// NUL separates the two halves because it is the one byte a path cannot
/// contain, so no path can spell a key that belongs to another venue.
fn venue_cache_key(resource_path: &Path, venue_id: &str) -> String {
    format!("{}\u{0}{venue_id}", resource_path.display())
}

async fn get_cached_venue_fixtures(
    resource_path: &Path,
    access: &mut impl AuthorizedVenue,
) -> Result<Arc<CachedVenueFixtures>, String> {
    let key = venue_cache_key(resource_path, access.venue_id());
    // Fast path: check data cache (sync mutex, instant)
    {
        let inner = VENUE_FIXTURE_CACHE.lock().unwrap();
        if let Some(cached) = inner.data.get(&key) {
            return Ok(cached.clone());
        }
    }

    // Get or create a per-venue loading lock
    let lock = {
        let mut inner = VENUE_FIXTURE_CACHE.lock().unwrap();
        inner
            .loading
            .entry(key.clone())
            .or_insert_with(|| Arc::new(TokioMutex::new(())))
            .clone()
    };

    // Only one task loads per venue; others wait here then read from cache
    let _guard = lock.lock().await;

    // Check again — another task may have loaded while we waited
    {
        let inner = VENUE_FIXTURE_CACHE.lock().unwrap();
        if let Some(cached) = inner.data.get(&key) {
            return Ok(cached.clone());
        }
    }

    // We're the loader. Always clean up the loading lock, even on error.
    let result = async {
        let fixtures = fixtures_db::get_patched_fixtures(access).await?;
        let memberships = groups_db::get_venue_memberships(access).await?;

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
        inner.loading.remove(&key);
        if let Ok(ref cached) = result {
            inner.data.insert(key, cached.clone());
        }
    }

    result
}

// =============================================================================
// Public API
// =============================================================================

/// Get grouped hierarchy for a venue: Groups -> Fixtures -> Heads
pub async fn get_grouped_hierarchy_with_path(
    resource_path: &Path,
    access: &mut impl AuthorizedVenue,
) -> Result<Vec<FixtureGroupNode>, String> {
    let groups = groups_db::list_groups(access).await?;

    let mut result = Vec::with_capacity(groups.len());

    for group in groups {
        let members = groups_db::get_members_in_group(access, &group.id).await?;

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
    resource_path: &Path,
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
fn get_fixture_heads_with_path(resource_path: &Path, fixture: &PatchedFixture) -> Vec<HeadNode> {
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
fn head_count_with_path(resource_path: &Path, fixture: &PatchedFixture) -> usize {
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

/// Deterministically keep `subset.keep(n)` of `n` units, as ascending indices.
///
/// Shuffle-then-truncate rather than repeated sampling: the drawn set depends
/// only on the seed and `n`, and sorting afterwards restores venue order so the
/// caller never sees the shuffle.
fn pick(n: usize, subset: Subset, rng: &mut StdRng) -> Vec<usize> {
    let keep = subset.keep(n);
    if keep == n {
        return (0..n).collect();
    }
    let mut indices: Vec<usize> = (0..n).collect();
    indices.shuffle(rng);
    indices.truncate(keep);
    indices.sort_unstable();
    indices
}

/// Narrow a matched head set to the selection's subset.
///
/// Whole fixtures are the unit whenever the match is one: a half of six whole
/// fixtures is three fixtures lit end to end, not three heads scattered across
/// six bars. Only a head-partial match (some fixture matched on some of its
/// heads) falls back to picking heads, because there is no whole fixture to pick.
fn narrow(selected: HeadSet, head_counts: &[usize], subset: Subset, rng: &mut StdRng) -> HeadSet {
    if subset.is_all() || selected.is_empty() {
        return selected;
    }

    // Matched fixtures in venue order, each with its matched heads.
    let mut matched: Vec<(usize, Vec<usize>)> = Vec::new();
    for (fi, &head_count) in head_counts.iter().enumerate() {
        let heads: Vec<usize> = (0..head_count)
            .filter(|&h| selected.contains(&(fi, h)))
            .collect();
        if !heads.is_empty() {
            matched.push((fi, heads));
        }
    }

    let head_partial = matched
        .iter()
        .any(|(fi, heads)| heads.len() != head_counts[*fi]);

    if head_partial {
        let heads: Vec<(usize, usize)> = matched
            .iter()
            .flat_map(|(fi, heads)| heads.iter().map(move |&h| (*fi, h)))
            .collect();
        pick(heads.len(), subset, rng)
            .into_iter()
            .map(|at| heads[at])
            .collect()
    } else {
        pick(matched.len(), subset, rng)
            .into_iter()
            .flat_map(|at| {
                let (fi, heads) = &matched[at];
                heads.iter().map(move |&h| (*fi, h))
            })
            .collect()
    }
}

/// Resolve a selection to matching fixtures/heads, in venue fixture order.
///
/// # Determinism
///
/// `rng_seed` decides every random choice the resolution makes — which side an
/// `^` takes, and which fixtures a [`Subset`] keeps — so one seed always lights
/// the same rig. Callers own that contract: the eval pre-pass seeds from the
/// selection node's id *and the clip it belongs to*
/// ([`crate::eval::context::seed_for`]), so one clip renders the same lights on
/// every run while the same pattern placed twice draws two different halves —
/// hold a motion for a phrase, then mix it up. The IPC preview passes a fixed
/// seed so the picker does not flicker between calls. The subset draw runs
/// *after* the expression, on the same rng, so adding a subset to a selection
/// cannot change which side an `^` took.
pub async fn resolve_selection_expression_with_path(
    resource_path: &Path,
    access: &mut impl AuthorizedVenue,
    selection: &Selection,
    rng_seed: u64,
) -> Result<Vec<ResolvedFixture>, String> {
    let trimmed = selection.expression.trim();
    let cached = get_cached_venue_fixtures(resource_path, access).await?;

    if cached.fixtures.is_empty() {
        return Ok(vec![]);
    }

    let mut rng = StdRng::seed_from_u64(rng_seed);

    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("all") {
        // The whole venue is whole fixtures, so the subset picks at that level.
        return Ok(pick(cached.fixtures.len(), selection.subset, &mut rng)
            .into_iter()
            .map(|at| ResolvedFixture {
                fixture: cached.fixtures[at].clone(),
                heads: None,
            })
            .collect());
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
        rng,
    };
    let selected = eval_expr(&expr, &mut ctx)?;
    let selected = if selection.subset.is_all() {
        selected
    } else {
        let head_counts: Vec<usize> = cached
            .fixture_info
            .iter()
            .map(|info| info.head_count)
            .collect();
        narrow(selected, &head_counts, selection.subset, &mut ctx.rng)
    };

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
    resource_path: &Path,
    access: &mut VenueAccess<'_, Write>,
    fixture_id: &str,
    group_id: &str,
    head_index: i64,
) -> Result<(), String> {
    let fixture = fixtures_db::get_fixture(access, fixture_id).await?;
    let head_count = head_count_with_path(resource_path, &fixture) as i64;
    let keep: Vec<i64> = (0..head_count).filter(|&h| h != head_index).collect();

    if groups_db::split_whole_fixture_membership(access, fixture_id, group_id, &keep).await? {
        return Ok(());
    }
    groups_db::remove_member_from_group(access, fixture_id, group_id, Some(head_index)).await
}

#[cfg(test)]
mod tests {
    use super::{narrow, venue_cache_key, HeadSet};
    use crate::models::selection::Subset;
    use rand::{rngs::StdRng, SeedableRng};
    use std::path::Path;

    /// `n` fixtures of `heads` heads each, all matched.
    fn whole(n: usize, heads: usize) -> (HeadSet, Vec<usize>) {
        let set = (0..n)
            .flat_map(|f| (0..heads).map(move |h| (f, h)))
            .collect();
        (set, vec![heads; n])
    }

    fn run(set: HeadSet, counts: &[usize], subset: Subset, seed: u64) -> Vec<(usize, usize)> {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut out: Vec<(usize, usize)> =
            narrow(set, counts, subset, &mut rng).into_iter().collect();
        out.sort_unstable();
        out
    }

    /// The default, and the shape of every selection stored before `subset`
    /// existed: nothing is dropped.
    #[test]
    fn all_keeps_everything() {
        let (set, counts) = whole(5, 2);
        assert_eq!(run(set, &counts, Subset::All, 7).len(), 10);
    }

    /// One seed, one rig. This is the contract clips rely on to render the same
    /// lights on every run.
    #[test]
    fn the_same_seed_picks_the_same_lights() {
        let (set, counts) = whole(8, 1);
        let a = run(set.clone(), &counts, Subset::Fraction(0.5), 42);
        let b = run(set.clone(), &counts, Subset::Fraction(0.5), 42);
        let c = run(set, &counts, Subset::Fraction(0.5), 43);
        assert_eq!(a, b);
        assert_eq!(a.len(), 4);
        assert_ne!(a, c, "a different seed re-rolls");
    }

    /// A half of whole fixtures is whole fixtures — never half of every bar.
    #[test]
    fn whole_fixtures_are_picked_whole() {
        let (set, counts) = whole(6, 4);
        let kept = run(set, &counts, Subset::Fraction(0.5), 9);
        assert_eq!(kept.len(), 12, "3 of 6 fixtures, 4 heads each");
        let mut fixtures: Vec<usize> = kept.iter().map(|&(f, _)| f).collect();
        fixtures.dedup();
        assert_eq!(fixtures.len(), 3);
        for f in fixtures {
            assert_eq!(kept.iter().filter(|&&(fi, _)| fi == f).count(), 4);
        }
    }

    /// With no whole fixture to pick, heads are the unit.
    #[test]
    fn a_head_partial_match_falls_back_to_heads() {
        // Fixture 0 matched on 3 of its 4 heads; fixture 1 matched whole.
        let set: HeadSet = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1)]
            .into_iter()
            .collect();
        let kept = run(set, &[4, 2], Subset::Count(2), 3);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn count_clamps_and_fraction_never_empties_the_set() {
        let (set, counts) = whole(3, 1);
        assert_eq!(run(set.clone(), &counts, Subset::Count(99), 1).len(), 3);
        assert_eq!(run(set.clone(), &counts, Subset::Count(2), 1).len(), 2);
        assert_eq!(run(set, &counts, Subset::Fraction(0.01), 1).len(), 1);
    }

    /// The point of a per-clip seed: place one pattern twice over the same
    /// group and the two clips light different halves, while either clip on its
    /// own lights the same half every run.
    #[test]
    fn two_clips_of_one_pattern_draw_different_halves() {
        use crate::eval::context::seed_for;

        let draw = |clip: &str| {
            let (set, counts) = whole(8, 1);
            run(
                set,
                &counts,
                Subset::Fraction(0.5),
                seed_for(Some(clip), "select-1"),
            )
        };
        assert_eq!(draw("clip-a"), draw("clip-a"), "one clip is stable");
        assert_ne!(draw("clip-a"), draw("clip-b"));

        // And the node still matters: two selection nodes in one clip draw
        // independently, so an `^` inside a clip is not forced to agree.
        let a = seed_for(Some("clip-a"), "select-1");
        let b = seed_for(Some("clip-a"), "select-2");
        assert_ne!(a, b);

        // No clip (a pattern's own preview) leaves the seed as it always was.
        assert_eq!(seed_for(None, "select-1"), {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            "select-1".hash(&mut h);
            h.finish()
        });
    }

    #[test]
    fn an_empty_match_stays_empty() {
        assert!(run(HeadSet::new(), &[2, 2], Subset::Fraction(0.5), 1).is_empty());
    }

    /// The bug this key exists to prevent: two libraries holding a venue of the
    /// same id are two venues, and a cache that cannot tell them apart serves
    /// the first one's rig to the second.
    #[test]
    fn one_venue_id_in_two_libraries_is_two_keys() {
        let venue = "venue-main";
        assert_ne!(
            venue_cache_key(Path::new("/tmp/luma-a"), venue),
            venue_cache_key(Path::new("/tmp/luma-b"), venue),
        );
    }

    /// And the other half: the same venue in the same library must stay one
    /// entry, or the cache stops being a cache.
    #[test]
    fn the_same_venue_in_the_same_library_is_one_key() {
        assert_eq!(
            venue_cache_key(Path::new("/tmp/luma-a"), "venue-main"),
            venue_cache_key(Path::new("/tmp/luma-a"), "venue-main"),
        );
    }

    /// The separator is NUL because a path cannot contain one. Without that,
    /// a library directory named to look like `library + separator + venue`
    /// could collide with a different library's venue.
    #[test]
    fn a_path_cannot_spell_another_venues_key() {
        assert_ne!(
            venue_cache_key(Path::new("/tmp/luma"), "a/venue-main"),
            venue_cache_key(Path::new("/tmp/luma/a"), "venue-main"),
        );
    }
}
