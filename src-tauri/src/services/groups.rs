//! Business logic for fixture group operations.
//!
//! Handles group hierarchy building, fixture type detection, and tag expression resolution.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use once_cell::sync::Lazy;
use rand::prelude::*;
use tokio::sync::Mutex as TokioMutex;

use crate::database::local;
use crate::database::local::fixtures as fixtures_db;
use crate::database::local::group_overrides as overrides_db;
use crate::database::local::group_overrides::GroupOverride;
use crate::database::local::groups as groups_db;
use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::fixtures::layout::{fixture_mount, head_geometry};
use crate::fixtures::parser;
use crate::models::fixtures::{FixtureDefinition, Mode, PatchedFixture};
use crate::models::groups::{
    normalize_group_name, FixtureGroupNode, GroupTreeNode, GroupedFixtureNode, HeadNode,
};
use crate::models::selection::{Selection, Subset};
use crate::services::group_derivation::{
    self, DerivedTree, FixtureIdentity, FixtureRole, ManualGroup, VenueFacts,
};
use fixture_kinematics::rig_position;
use luma_scene::venue::ResolvedVenue;

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
        let memberships = groups_db::venue_memberships(access).await?;

        // fixture_id → (normalized group name → which heads are members)
        let mut by_fixture: HashMap<String, HashMap<String, HeadMembership>> = HashMap::new();
        for row in memberships {
            let Some(name) = row.group_name.as_deref().map(normalize_group_name) else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            let entry = by_fixture
                .entry(row.fixture_id)
                .or_default()
                .entry(name)
                .or_insert_with(|| HeadMembership::Heads(HashSet::new()));
            if row.head_index < 0 {
                *entry = HeadMembership::All;
            } else if let HeadMembership::Heads(heads) = entry {
                heads.insert(row.head_index as usize);
            }
        }

        // Derived groups are selectable by name like anything else: a score
        // naming `spots_top` has to resolve, and there is one membership
        // answer rather than a derived one and an authored one. A venue whose
        // graph has not been built has no structure to derive from, which is an
        // empty tree rather than an error.
        if local::venue_graph::root_id(access).await?.is_some() {
            for node in GroupSources::read(resource_path, access).await?.tree() {
                if node.name.is_empty() {
                    continue;
                }
                for fixture_id in &node.fixtures {
                    by_fixture
                        .entry(fixture_id.clone())
                        .or_default()
                        .entry(node.name.clone())
                        .or_insert(HeadMembership::All);
                }
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

/// One fixture's definition, parsed at most once per file per process.
///
/// Deriving the group tree asks every fixture for its role, and a venue's
/// fixtures share a handful of definitions; re-reading and re-parsing a `.qxf`
/// per fixture per derivation was most of the cost of renaming a group. The
/// modification time is stored *beside* the entry rather than in its key, so a
/// definition replaced on disk is re-read and the superseded parse is dropped:
/// keying on `(path, mtime)` bounded the memo by how many times the library had
/// ever been edited, which is not a bound.
///
/// `None` when the file will not parse: one unreadable `.qxf` should cost the
/// venue one fixture's classification, not the whole tree.
fn definition(resource_path: &Path, fixture: &PatchedFixture) -> Option<Arc<FixtureDefinition>> {
    /// One entry per file: when it was last read, and what it parsed to.
    type Parsed = HashMap<PathBuf, (Option<SystemTime>, Arc<FixtureDefinition>)>;
    static CACHE: Lazy<std::sync::Mutex<Parsed>> = Lazy::new(Default::default);

    let path = resource_path.join(&fixture.fixture_path);
    let changed = std::fs::metadata(&path).ok()?.modified().ok();
    if let Some(hit) = CACHE
        .lock()
        .ok()?
        .get(&path)
        .filter(|(seen, _)| *seen == changed)
        .map(|(_, parsed)| parsed.clone())
    {
        return Some(hit);
    }
    let parsed = Arc::new(parser::parse_definition(&path).ok()?);
    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(path, (changed, parsed.clone()));
    }
    Some(parsed)
}

/// The mode a fixture is patched in, falling back to the definition's first —
/// a patch naming a mode the definition no longer has still describes a light.
fn patched_mode<'a>(def: &'a FixtureDefinition, fixture: &PatchedFixture) -> Option<&'a Mode> {
    def.modes
        .iter()
        .find(|mode| mode.name == fixture.mode_name)
        .or_else(|| def.modes.first())
}

/// A fixture's role and whether it aims — the two questions the group tree and
/// the movement pyramid ask, answered from one parse.
///
/// A definition that will not parse is [`FixtureRole::Other`] and does not aim,
/// rather than an error.
fn role_and_aim(resource_path: &Path, fixture: &PatchedFixture) -> (FixtureRole, bool) {
    let Some(def) = definition(resource_path, fixture) else {
        return (FixtureRole::Other, false);
    };
    let Some(mode) = patched_mode(&def, fixture) else {
        return (FixtureRole::Other, false);
    };
    (
        FixtureRole::of(&def, mode),
        group_derivation::aims(&def, mode),
    )
}

/// Get all heads for a fixture with their world positions (PathBuf version).
///
/// Empty when the mode defines no heads, and empty when the fixture is patched
/// but **not placed**: a head node is a point in the room, and a fixture in the
/// tray is not in the room. The alternative — a head at the origin — is what
/// piled every unplaced fixture at `(0, 0, 0)`.
fn get_fixture_heads_with_path(
    resource_path: &Path,
    venue: &ResolvedVenue,
    fixture: &PatchedFixture,
) -> Vec<HeadNode> {
    let Some(def) = definition(resource_path, fixture) else {
        return Vec::new();
    };
    let Some(mode) = def.modes.iter().find(|m| m.name == fixture.mode_name) else {
        return Vec::new();
    };
    if mode.heads.is_empty() {
        return Vec::new();
    }
    let Some(pose) = venue.pose(&fixture.id) else {
        return Vec::new();
    };

    let geom = head_geometry(&def, &fixture.mode_name);
    let mount = fixture_mount(pose);

    (0..geom.cell_count())
        .map(|i| HeadNode {
            id: format!("{}:{}", fixture.id, i),
            label: format!("Head {}", i + 1),
            head_index: i as i64,
            position: rig_position(&geom, &mount, i).to_array(),
        })
        .collect()
}

/// Number of heads a fixture's mode defines, floored at 1 — the eval engine
/// always emits at least one primitive ("{id}:0") per fixture.
fn head_count_with_path(resource_path: &Path, fixture: &PatchedFixture) -> usize {
    definition(resource_path, fixture)
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

/// The group names a selection expression is a plain union of, or `None` when
/// it says something a list of checkboxes cannot.
///
/// The picker dialog's whole contract with the grammar. A union of bare tokens
/// is exactly what a set of checked rows means, so those expressions round-trip
/// through the picker unchanged; anything using `&`, `~`, `^` or `?` — or a
/// parenthesised sub-expression that is not itself a bare token — has no
/// checkbox spelling and is handed back as `None` for the caller to show
/// read-only. Parsing here rather than in the UI keeps one grammar: the picker
/// cannot drift from what the resolver actually evaluates.
///
/// The whole venue — an empty expression or `all` — is the empty list, so
/// "nothing checked" and "everything lit" are the same state in both
/// directions.
///
/// Names come back normalized and deduplicated, in first-mention order.
#[must_use]
pub fn or_terms(expression: &str) -> Option<Vec<String>> {
    let trimmed = expression.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("all") {
        return Some(Vec::new());
    }
    let mut terms = Vec::new();
    collect_or_terms(&parse_selection_expression(trimmed).ok()?, &mut terms).then_some(terms)
}

/// Flatten a union of bare tokens into `out`; `false` as soon as any other
/// operator is reached.
fn collect_or_terms(expr: &Expr, out: &mut Vec<String>) -> bool {
    match expr {
        Expr::Token(name) => {
            // `all` inside a union is redundant with every other arm of it,
            // and as a checkbox it would be a row that is not a group.
            if !name.eq_ignore_ascii_case("all") && !out.iter().any(|seen| seen == name) {
                out.push(name.clone());
            }
            true
        }
        Expr::Or(a, b) => collect_or_terms(a, out) && collect_or_terms(b, out),
        _ => false,
    }
}

/// The expression a set of checked groups spells — the inverse of [`or_terms`].
///
/// Empty is `all`, which is what an LD unchecking every row means and what
/// [`or_terms`] reads back as empty.
#[must_use]
pub fn or_expression(terms: &[String]) -> String {
    if terms.is_empty() {
        return "all".into();
    }
    terms.join(" | ")
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

// =============================================================================
// The derived group tree
// =============================================================================

/// Everything the group tree is made of, read once.
///
/// **The** read: every surface that asks a venue what its groups are asks
/// this, and gets one answer — the derivation, the overrides on top of it, and
/// the authored `fixture_groups` rows beside it. There is no second place to
/// ask, and nothing writes derived sets into `fixture_groups` to make them
/// visible; a set exists because the rig describes it, not because a row does.
///
/// One derivation per command. The tree, a node in it, the derivation path an
/// override row records and the tree the command hands back afterwards are all
/// questions about the same solve — asking each of them separately re-solved
/// the venue and re-parsed every `.qxf` in it, four times over for a rename.
pub struct GroupSources {
    root: PathBuf,
    solved: Solved,
    derived: DerivedTree,
    overrides: Vec<GroupOverride>,
    manual: Vec<ManualGroup>,
    /// Every authored membership row, which is where per-head membership lives.
    /// Derivation has none: a derived set holds whole fixtures.
    members: Vec<groups_db::Membership>,
    /// The authored rows themselves, for the columns only they have.
    authored: Vec<crate::models::groups::FixtureGroup>,
}

impl GroupSources {
    /// Solve the venue, derive, and read the two group tables beside it.
    ///
    /// # Errors
    /// Fails if the rows cannot be read, if the catalog cannot be resolved, or
    /// if the venue has no graph root (`crate::venue_graph::ensure_migrated`
    /// has never run for it).
    pub async fn read(
        resource_path: &Path,
        access: &mut impl AuthorizedVenue,
    ) -> Result<Self, String> {
        let solved = solve(resource_path, access).await?;
        let derived = group_derivation::derive_groups(&solved.facts);
        let overrides = overrides_db::list(access).await?;
        let members = groups_db::venue_memberships(access).await?;
        let authored = groups_db::list_groups(access).await?;

        // Membership order, deduplicated across the per-head rows — the ids a
        // node carries, in the order the editor put them in.
        let manual = authored
            .iter()
            .map(|group| {
                let mut fixtures: Vec<String> = Vec::new();
                for row in members.iter().filter(|row| row.group_id == group.id) {
                    if !fixtures.contains(&row.fixture_id) {
                        fixtures.push(row.fixture_id.clone());
                    }
                }
                ManualGroup {
                    id: group.id.clone(),
                    name: group.name.clone(),
                    fixtures,
                }
            })
            .collect();

        Ok(GroupSources {
            root: resource_path.to_path_buf(),
            solved,
            derived,
            overrides,
            manual,
            members,
            authored,
        })
    }

    /// The merged tree with every node's fixtures resolved — role, aim, heads,
    /// and the columns an authored row has.
    ///
    /// The same nodes [`Self::tree`] returns, in the same order; the difference
    /// is only how much of each fixture comes with them.
    #[must_use]
    pub fn hierarchy(&self) -> Vec<FixtureGroupNode> {
        let by_id: HashMap<&str, &PatchedFixture> = self
            .solved
            .fixtures
            .iter()
            .map(|fixture| (fixture.id.as_str(), fixture))
            .collect();

        self.tree()
            .into_iter()
            .map(|node| {
                let authored = self
                    .authored
                    .iter()
                    .find(|group| group.id == node.id)
                    .filter(|_| node.role.is_none());
                let mut moves = false;
                let mut fixtures = Vec::with_capacity(node.fixtures.len());
                for id in &node.fixtures {
                    let Some(fixture) = by_id.get(id.as_str()).copied() else {
                        continue;
                    };
                    let (role, aims) = role_and_aim(&self.root, fixture);
                    moves |= aims;
                    let all_heads =
                        get_fixture_heads_with_path(&self.root, &self.solved.venue, fixture);
                    let head_count = all_heads.len() as i64;
                    let heads = match self.member_heads(&node.id, id) {
                        None => all_heads,
                        Some(indices) => all_heads
                            .into_iter()
                            .filter(|head| indices.contains(&head.head_index))
                            .collect(),
                    };
                    fixtures.push(GroupedFixtureNode {
                        id: fixture.id.clone(),
                        label: fixture.label.clone().unwrap_or_else(|| {
                            format!("{} {}", fixture.manufacturer, fixture.model)
                        }),
                        role,
                        moves: aims,
                        heads,
                        head_count,
                    });
                }
                FixtureGroupNode {
                    id: node.id,
                    name: node.name,
                    label: node.label,
                    parent_id: node.parent_id,
                    origin: node.origin,
                    role: node.role,
                    moves,
                    axis_lr: authored.and_then(|group| group.axis_lr),
                    axis_fb: authored.and_then(|group| group.axis_fb),
                    axis_ab: authored.and_then(|group| group.axis_ab),
                    movement_config: authored.and_then(|group| group.movement_config.clone()),
                    fixtures,
                }
            })
            .collect()
    }

    /// Every group's members as the compositor's *member keys*:
    /// `"<fixture>"` for a whole fixture, `"<fixture>:<head>"` for one head.
    ///
    /// Keyed by node id, derived nodes included — a controller fader bound to
    /// `spots_left_wing` has to find that set, and only the authored table
    /// having an id for it is what made every such binding silently do nothing.
    #[must_use]
    pub fn member_keys(&self) -> HashMap<String, Vec<String>> {
        self.hierarchy()
            .into_iter()
            .map(|node| {
                let keys = node
                    .fixtures
                    .iter()
                    .flat_map(|fixture| {
                        // A fixture the group holds whole is one key; the mode
                        // that defines no heads is held whole too.
                        if fixture.heads.is_empty()
                            || fixture.heads.len() as i64 == fixture.head_count
                        {
                            vec![fixture.id.clone()]
                        } else {
                            fixture.heads.iter().map(|head| head.id.clone()).collect()
                        }
                    })
                    .collect();
                (node.id, keys)
            })
            .collect()
    }

    /// Which heads of `fixture` a node holds — `None` when it holds the whole
    /// fixture, which is every derived node and the ordinary authored row.
    fn member_heads(&self, group_id: &str, fixture_id: &str) -> Option<Vec<i64>> {
        let mut heads = Vec::new();
        for row in &self.members {
            if row.group_id != group_id || row.fixture_id != fixture_id {
                continue;
            }
            if row.head_index == groups_db::WHOLE_FIXTURE {
                return None;
            }
            heads.push(row.head_index);
        }
        (!heads.is_empty()).then_some(heads)
    }

    /// The merged group tree: derivation, the overrides on top, and the
    /// authored groups beside them. Parents before children, and every name
    /// distinct.
    #[must_use]
    pub fn tree(&self) -> Vec<GroupTreeNode> {
        let mut nodes = self.named();
        group_derivation::make_names_distinct(&mut nodes);
        nodes
    }

    /// The same tree with the names the rules mint, two of which can be one
    /// word. Private: [`Self::tree`] and [`Self::clash_for`] are the two
    /// questions it answers, and nothing else should have to know that
    /// distinctness is a second step.
    fn named(&self) -> Vec<GroupTreeNode> {
        group_derivation::merge_tree(&self.derived, &self.overrides, &self.manual)
    }

    /// The node already answering to the name `group_id` asks for, if any.
    ///
    /// Asked of the minted names rather than the distinct ones, because after
    /// [`group_derivation::make_names_distinct`] there is no collision left to
    /// find. That is the line between the two: a name someone *typed* is
    /// refused here, and only a name nobody typed gets a suffix.
    #[must_use]
    pub fn clash_for(&self, group_id: &str) -> Option<GroupTreeNode> {
        let named = self.named();
        let asked = &named.iter().find(|node| node.id == group_id)?.name;
        node_answering_to(&named, asked, group_id).cloned()
    }

    /// Whether `group_id` names a node of this venue's tree at all.
    #[must_use]
    pub fn contains(&self, group_id: &str) -> bool {
        self.derived.groups.iter().any(|g| g.id == group_id)
            || self.manual.iter().any(|g| g.id == group_id)
    }

    /// The derivation path of a derived node, `/`-joined — the override row's
    /// record of *which set* was touched. Empty for an authored group, which
    /// has no derivation to record.
    #[must_use]
    pub fn derived_path(&self, group_id: &str) -> String {
        self.derived
            .groups
            .iter()
            .find(|group| group.id == group_id)
            .map_or_else(String::new, |group| group.path.join("/"))
    }

    /// The override row already standing for `group_id`, if any.
    #[must_use]
    pub fn override_of(&self, group_id: &str) -> Option<&GroupOverride> {
        self.overrides.iter().find(|row| row.group_id == group_id)
    }

    /// Every override in the venue — what [`group_derivation::merged_terminal`]
    /// reads to follow a merge chain.
    #[must_use]
    pub fn overrides(&self) -> &[GroupOverride] {
        &self.overrides
    }

    /// Apply a row locally, so the caller that just wrote it can hand back the
    /// resulting tree without re-deriving the venue it derived a moment ago.
    pub fn apply(&mut self, row: Option<GroupOverride>, group_id: &str) {
        self.overrides
            .retain(|existing| existing.group_id != group_id);
        if let Some(row) = row {
            self.overrides.push(row);
        }
    }
}

/// The label path of every node of a merged tree, `/`-joined, keyed by id.
///
/// The tree arrives parents-first, so one pass builds each path out of the one
/// already built for its parent. What a node *is called* is [`FixtureGroupNode::
/// name`]; this is where it sits, spelled for a reader.
#[must_use]
pub fn label_paths(nodes: &[FixtureGroupNode]) -> HashMap<String, String> {
    let mut paths: HashMap<String, String> = HashMap::with_capacity(nodes.len());
    for node in nodes {
        let path = match node.parent_id.as_deref().and_then(|id| paths.get(id)) {
            Some(parent) => format!("{parent}/{}", node.label),
            None => node.label.clone(),
        };
        paths.insert(node.id.clone(), path);
    }
    paths
}

/// The node already answering to `name`, if any — the one check that keeps the
/// selection namespace a namespace.
///
/// Derived nodes and authored groups share it: a `fixture_groups` row called
/// `spots_right_wing` and the wing of that name are two sets an expression
/// cannot tell apart, so it would quietly union them. `except` is the node
/// being renamed, which is allowed to keep its own name.
///
/// Names that normalize to empty are not names and never collide.
///
/// This is the check for a name someone *typed* — a created group, a renamed
/// node — and it refuses. A name nobody typed is a different question: a piece
/// labelled `Truss-1` beside one labelled `Truss 1` mints one derived name
/// twice, and refusing the label would make a stage verb answer for a group
/// tree it cannot see. Derivation separates those instead
/// ([`group_derivation::make_names_distinct`]), so a label write stays a label
/// write.
#[must_use]
pub fn node_answering_to<'a>(
    tree: &'a [GroupTreeNode],
    name: &str,
    except: &str,
) -> Option<&'a GroupTreeNode> {
    tree.iter()
        .find(|node| node.id != except && !node.name.is_empty() && node.name == name)
}

/// One venue, solved once: where everything is, what the patch says it is, and
/// the facts derivation reads off both.
///
/// One value because they come from one pass. A caller that resolved the venue
/// and then asked for the facts solved it twice, and the two solves were free
/// to disagree.
pub struct Solved {
    pub venue: ResolvedVenue,
    pub facts: VenueFacts,
    /// The patch list, in database order.
    pub fixtures: Vec<PatchedFixture>,
}

/// Solve a venue and read the facts derivation needs off it.
///
/// The graph supplies placement and structure, the patch list supplies identity
/// and role, and `facts_from` is where they meet — see [`group_derivation`] for
/// why that seam is there.
///
/// # Errors
/// Fails if the graph or the patch list cannot be read, or if the catalog
/// cannot be resolved.
pub async fn solve(
    resource_path: &Path,
    access: &mut impl AuthorizedVenue,
) -> Result<Solved, String> {
    let venue_id = access.venue_id().to_string();
    let graph = crate::venue_graph::graph(access).await?;
    let sockets = crate::venue_graph::sockets(resource_path)?;
    let venue = luma_scene::venue::resolve(&graph, sockets);

    let order = groups_db::fixture_creation_order(access).await?;
    let fixtures = fixtures_db::get_patched_fixtures(access).await?;
    let identities: Vec<FixtureIdentity> = order
        .iter()
        .filter_map(|id| fixtures.iter().find(|fixture| &fixture.id == id))
        .map(|fixture| FixtureIdentity {
            id: fixture.id.clone(),
            model: fixture.model.clone(),
            role: role_with_path(resource_path, fixture),
        })
        .collect();

    let facts = group_derivation::facts_from(&venue_id, &venue, &graph, sockets, &identities);
    Ok(Solved {
        venue,
        facts,
        fixtures,
    })
}

/// A fixture's role, from its definition and the mode it is patched in.
pub fn role_with_path(resource_path: &Path, fixture: &PatchedFixture) -> FixtureRole {
    role_and_aim(resource_path, fixture).0
}

#[cfg(test)]
mod tests {
    use super::{narrow, or_expression, or_terms, venue_cache_key, HeadSet};
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

    /// A union of bare tokens is what a set of checkboxes means; anything
    /// else has no checkbox spelling.
    #[test]
    fn or_terms_reads_back_only_plain_unions() {
        assert_eq!(or_terms("front_wash"), Some(vec!["front_wash".into()]));
        assert_eq!(
            or_terms("front_wash | back_movers|spots"),
            Some(vec![
                "front_wash".into(),
                "back_movers".into(),
                "spots".into()
            ])
        );
        assert_eq!(
            or_terms("(front_wash) | (spots)"),
            or_terms("front_wash | spots")
        );
        assert_eq!(
            or_terms("front_wash | front_wash"),
            Some(vec!["front_wash".into()])
        );

        for opaque in [
            "front_wash & left",
            "~spots",
            "a ^ b",
            "a ? b",
            "a & (b | c)",
        ] {
            assert_eq!(or_terms(opaque), None, "{opaque}");
        }
        assert_eq!(or_terms("front_wash |"), None);
    }

    /// Nothing checked and the whole venue are the same state, both ways.
    #[test]
    fn the_whole_venue_is_the_empty_term_list() {
        assert_eq!(or_terms(""), Some(vec![]));
        assert_eq!(or_terms("  ALL "), Some(vec![]));
        assert_eq!(or_terms("all | spots"), Some(vec!["spots".into()]));
        assert_eq!(or_expression(&[]), "all");
        assert_eq!(or_expression(&["a".to_string(), "b".to_string()]), "a | b");
        assert_eq!(or_terms(&or_expression(&[])), Some(vec![]));
    }
}
