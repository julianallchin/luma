//! Business logic for fixture group operations.
//!
//! Handles group hierarchy building, fixture type detection, and selection resolution.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use once_cell::sync::Lazy;
use tokio::sync::Mutex as TokioMutex;

use crate::database::local::fixtures as fixtures_db;
use crate::database::local::groups as groups_db;
use crate::database::local::venue_access::{AuthorizedVenue, VenueAccess, Write};
use crate::database::local::venue_graph::rig_generation;
use crate::fixtures::layout::{fixture_mount, head_geometry};
use crate::models::fixtures::{FixtureDefinition, Mode, PatchedFixture};
use crate::models::groups::{
    normalize_group_name, FixtureGroupNode, FixtureRole, GroupTreeNode, GroupedFixtureNode,
    HeadNode,
};
use crate::models::selection::Selection;
use crate::services::group_derivation::{
    self, DerivedTree, FixtureIdentity, ManualGroup, VenueFacts,
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
/// The `data` map holds completed results, loaded at `generation` of the rig;
/// `loading` holds per-key async mutexes so only one task loads for a given
/// venue while others wait.
struct VenueFixtureCacheInner {
    generation: u64,
    data: HashMap<String, Arc<CachedVenueFixtures>>,
    loading: HashMap<String, Arc<TokioMutex<()>>>,
}

impl VenueFixtureCacheInner {
    /// The results still current: none, once the rig has moved since they
    /// were loaded.
    fn data(&mut self) -> &mut HashMap<String, Arc<CachedVenueFixtures>> {
        let generation = rig_generation();
        if self.generation != generation {
            self.data.clear();
            self.generation = generation;
        }
        &mut self.data
    }
}

static VENUE_FIXTURE_CACHE: Lazy<std::sync::Mutex<VenueFixtureCacheInner>> = Lazy::new(|| {
    std::sync::Mutex::new(VenueFixtureCacheInner {
        generation: rig_generation(),
        data: HashMap::new(),
        loading: HashMap::new(),
    })
});

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
        let mut inner = VENUE_FIXTURE_CACHE.lock().unwrap();
        if let Some(cached) = inner.data().get(&key) {
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
    let generation = {
        let mut inner = VENUE_FIXTURE_CACHE.lock().unwrap();
        if let Some(cached) = inner.data().get(&key) {
            return Ok(cached.clone());
        }
        inner.generation
    };

    // We're the loader. Always clean up the loading lock, even on error.
    let result = async {
        let fixtures = fixtures_db::get_patched_fixtures(access).await?;
        let memberships = groups_db::venue_memberships(access).await?;

        // fixture_id → (group id → which heads are members)
        let mut by_fixture: HashMap<String, HashMap<String, HeadMembership>> = HashMap::new();
        for row in memberships {
            let entry = by_fixture
                .entry(row.fixture_id)
                .or_default()
                .entry(row.group_id)
                .or_insert_with(|| HeadMembership::Heads(HashSet::new()));
            if row.head_index < 0 {
                *entry = HeadMembership::All;
            } else if let HeadMembership::Heads(heads) = entry {
                heads.insert(row.head_index as usize);
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

    // Clean up loading lock regardless of success/failure. A result loaded
    // while the rig moved is returned but not kept.
    {
        let mut inner = VENUE_FIXTURE_CACHE.lock().unwrap();
        inner.loading.remove(&key);
        if let Ok(ref cached) = result {
            inner.data();
            if inner.generation == generation {
                inner.data.insert(key, cached.clone());
            }
        }
    }

    result
}

/// One fixture's definition. `None` when the file will not parse: one
/// unreadable `.qxf` should cost the venue one fixture's classification, not
/// the whole tree.
fn definition(resource_path: &Path, fixture: &PatchedFixture) -> Option<Arc<FixtureDefinition>> {
    crate::fixtures::definition(resource_path, &fixture.fixture_path).ok()
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
/// Empty when the mode defines no heads, and empty when the solve cannot reach
/// the fixture: a head node is a point in the room. The alternative — a head
/// at the origin — is what piled fixtures at `(0, 0, 0)`.
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
// Selection
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
    /// Group id → member heads.
    group_heads: HashMap<String, HeadMembership>,
}

/// A fixture matched by a selection, with the heads it matched on.
#[derive(Clone, Debug)]
pub struct ResolvedFixture {
    pub fixture: PatchedFixture,
    /// Matched head indices, ascending. `None` = every head of the fixture.
    pub heads: Option<Vec<usize>>,
}

/// Resolve a selection to matching fixtures/heads, in venue fixture order.
///
/// The union of the groups it lists. A group that no longer exists has no
/// members, so it adds nothing; a selection of only such groups is empty.
pub async fn resolve_selection(
    resource_path: &Path,
    access: &mut impl AuthorizedVenue,
    selection: &Selection,
) -> Result<Vec<ResolvedFixture>, String> {
    let cached = get_cached_venue_fixtures(resource_path, access).await?;
    let groups = match selection {
        Selection::All => {
            return Ok(cached
                .fixtures
                .iter()
                .map(|fixture| ResolvedFixture {
                    fixture: fixture.clone(),
                    heads: None,
                })
                .collect());
        }
        Selection::Groups(groups) => groups,
    };

    let mut result = Vec::new();
    for info in &cached.fixture_info {
        let mut heads: HashSet<usize> = HashSet::new();
        for group in groups {
            match info.group_heads.get(group) {
                Some(HeadMembership::All) => heads.extend(0..info.head_count),
                // Ignore stale rows pointing past the current mode's head count.
                Some(HeadMembership::Heads(members)) => {
                    heads.extend(members.iter().filter(|&&h| h < info.head_count));
                }
                None => {}
            }
        }
        if heads.is_empty() {
            continue;
        }
        result.push(ResolvedFixture {
            fixture: info.fixture.clone(),
            heads: (heads.len() < info.head_count).then(|| {
                let mut heads: Vec<usize> = heads.into_iter().collect();
                heads.sort_unstable();
                heads
            }),
        });
    }
    Ok(result)
}

/// What a selection is called where people and the agent write one: `all`,
/// or group names joined by `|`.
///
/// Stored selections hold group ids ([`Selection`]). This is the translation
/// at the edge where a name is typed or shown, made against one read of the
/// venue's groups so every name in a call means the same row.
pub struct GroupNames<'a> {
    /// `(id, name)`, in venue order.
    groups: Vec<(&'a str, &'a str)>,
}

/// How [`GroupNames::spell`] writes a selection that selects nothing: no
/// group listed, or only groups that have been deleted.
pub const NO_GROUPS: &str = "none";

impl<'a> GroupNames<'a> {
    /// The names of a group tree's nodes.
    #[must_use]
    pub fn of_tree(tree: &'a [GroupTreeNode]) -> Self {
        Self {
            groups: tree
                .iter()
                .filter(|node| !node.name.is_empty())
                .map(|node| (node.id.as_str(), node.name.as_str()))
                .collect(),
        }
    }

    /// The names of a venue's group rows.
    #[must_use]
    pub fn of_rows(rows: &'a [crate::models::groups::FixtureGroup]) -> Self {
        Self {
            groups: rows
                .iter()
                .filter_map(|row| Some((row.id.as_str(), row.name.as_deref()?)))
                .collect(),
        }
    }

    /// The selection `text` names.
    ///
    /// # Errors
    /// A name the venue has no group for, or anything that is not a union:
    /// the answer lists the groups there are.
    pub fn parse(&self, text: &str) -> Result<Selection, String> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("all") {
            return Ok(Selection::All);
        }
        let mut ids = Vec::new();
        let mut unknown = Vec::new();
        for term in text.split('|') {
            let term = term.trim();
            // Only what a name is made of: `front & back` is not the group
            // `front_back`.
            let named = term
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ' ' | '-'));
            let name = normalize_group_name(term);
            if named && name == "all" {
                return Ok(Selection::All);
            }
            match self.groups.iter().find(|(_, known)| named && *known == name) {
                Some((id, _)) => ids.push(*id),
                None => unknown.push(term),
            }
        }
        if unknown.is_empty() {
            return Ok(Selection::groups(ids));
        }
        let available: Vec<&str> = self.groups.iter().take(12).map(|(_, name)| *name).collect();
        Err(format!(
            "Unknown group name(s): {}. A selection is all, one group name, or several joined \
             with | (their union). Available groups: {}.",
            unknown.join(", "),
            if available.is_empty() {
                "none".into()
            } else {
                available.join(", ")
            }
        ))
    }

    /// What `group` — a group's id, or `all` — is called. `None` for a group
    /// that has been deleted.
    #[must_use]
    pub fn name(&self, group: &str) -> Option<&'a str> {
        if group == crate::controls::ALL {
            return Some(crate::controls::ALL);
        }
        self.groups
            .iter()
            .find(|(id, _)| *id == group)
            .map(|(_, name)| *name)
    }

    /// `selection` by name. A deleted group has no name and is left out.
    #[must_use]
    pub fn spell(&self, selection: &Selection) -> String {
        let Selection::Groups(groups) = selection else {
            return "all".into();
        };
        let names: Vec<&str> = groups.iter().filter_map(|id| self.name(id)).collect();
        if names.is_empty() {
            return NO_GROUPS.into();
        }
        names.join(" | ")
    }
}

// =============================================================================
// Head membership
// =============================================================================

// =============================================================================
// The derived group tree
// =============================================================================

/// Everything the group tree is made of, read once.
///
/// **The** read: every surface that asks a venue what its groups are asks
/// this, and gets one answer — the saved `fixture_groups` rows. Derivation
/// runs only when someone asks for a starting set
/// ([`snapshot_generated_groups`]), which saves what it derives.
///
/// One solve per command. The tree, a node in it and the tree the command
/// hands back afterwards are all questions about the same solve.
pub struct GroupSources {
    root: PathBuf,
    solved: Solved,
    /// Empty on a read; [`snapshot_generated_groups`] fills it.
    derived: DerivedTree,
    manual: Vec<ManualGroup>,
    /// Every authored membership row, which is where per-head membership lives.
    /// Derivation has none: a derived set holds whole fixtures.
    members: Vec<groups_db::Membership>,
    /// The authored rows themselves, for the columns only they have.
    authored: Vec<crate::models::groups::FixtureGroup>,
}

impl GroupSources {
    /// Solve the venue and read the group tables beside it.
    ///
    /// # Errors
    /// Fails if the rows cannot be read or the catalog cannot be resolved.
    pub async fn read(
        resource_path: &Path,
        access: &mut impl AuthorizedVenue,
    ) -> Result<Self, String> {
        let solved = solve(resource_path, access).await?;
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
            derived: DerivedTree::default(),
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

    /// The group tree: whatever was derived, and the authored groups beside
    /// it. Parents before children, and every name distinct.
    #[must_use]
    pub fn tree(&self) -> Vec<GroupTreeNode> {
        let mut nodes = group_derivation::merge_tree(&self.derived, &self.manual);
        group_derivation::make_names_distinct(&mut nodes);
        nodes
    }

    /// The rig as a busk page reads it: each fixture's role and whether it
    /// aims, and each named group of [`Self::tree`] by its fixtures.
    #[must_use]
    pub(crate) fn rig(&self) -> crate::services::busk_page::Rig {
        use crate::services::busk_page::{Rig, RigFixture, RigGroup};
        Rig {
            fixtures: self
                .solved
                .fixtures
                .iter()
                .map(|fixture| {
                    let (role, moves) = role_and_aim(&self.root, fixture);
                    RigFixture {
                        id: fixture.id.clone(),
                        role,
                        moves,
                    }
                })
                .collect(),
            groups: self
                .tree()
                .into_iter()
                .filter(|node| !node.name.is_empty())
                .map(|node| RigGroup {
                    id: node.id,
                    name: node.name,
                    fixtures: node.fixtures,
                })
                .collect(),
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
/// `spots_right_wing` and the wing of that name are two sets a typed
/// selection cannot tell apart, so it would pick whichever came first. `except` is the node
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
            role: role_and_aim(resource_path, fixture).0,
        })
        .collect();

    let facts = group_derivation::facts_from(&venue_id, &venue, &graph, sockets, &identities);
    Ok(Solved {
        venue,
        facts,
        fixtures,
    })
}

/// Check the one selector namespace inside the caller's write transaction.
pub async fn require_unique_name(
    resource_path: &Path,
    access: &mut VenueAccess<'_, Write>,
    name: Option<&str>,
    exclude: Option<&str>,
) -> Result<(), String> {
    let Some(normalized) = name.map(normalize_group_name).filter(|n| !n.is_empty()) else {
        return Ok(());
    };
    let tree = GroupSources::read(resource_path, access).await?.tree();
    if let Some(clash) = node_answering_to(&tree, &normalized, exclude.unwrap_or_default()) {
        return Err(format!(
            "`{normalized}` is already what `{}` is called in this venue",
            clash.label
        ));
    }
    Ok(())
}

/// The native group editor and Python boundary share the same name and member edit.
/// The caller commits the write transaction only after every member succeeds.
pub async fn save_group_changes(
    resource_path: &Path,
    access: &mut VenueAccess<'_, Write>,
    group_id: Option<&str>,
    label: &str,
    added: &[String],
    removed: &[String],
) -> Result<crate::models::groups::FixtureGroup, String> {
    crate::models::groups::validate_group_name(&normalize_group_name(label))?;
    require_unique_name(resource_path, access, Some(label), group_id).await?;
    let group = if let Some(id) = group_id {
        let before = groups_db::get_group(access, id).await?;
        groups_db::update_group(
            access,
            id,
            Some(label),
            before.axis_lr,
            before.axis_fb,
            before.axis_ab,
        )
        .await?
    } else {
        groups_db::create_group(access, Some(label), None, None, None).await?
    };
    for fixture in removed {
        groups_db::remove_member_from_group(access, fixture, &group.id, None).await?;
    }
    for fixture in added {
        groups_db::add_member_to_group(access, fixture, &group.id, groups_db::WHOLE_FIXTURE)
            .await?;
    }
    Ok(group)
}

/// Create a named collection, or explicitly replace its members.
/// `members` are `(fixture, head)` rows, `head` a cell index or
/// [`groups_db::WHOLE_FIXTURE`]. Resolve existing names under the same write
/// lock as the edit; a retry keeps the group's identity and the rows it
/// already had, and an invalid member cannot leave a partial change.
pub async fn set_named_group(
    resource_path: &Path,
    access: &mut VenueAccess<'_, Write>,
    name: &str,
    members: &[(String, i64)],
    replace: bool,
) -> Result<FixtureGroupNode, String> {
    let normalized = normalize_group_name(name);
    crate::models::groups::validate_group_name(&normalized)?;
    let sources = GroupSources::read(resource_path, access).await?;
    for (fixture_id, head) in members {
        if *head == groups_db::WHOLE_FIXTURE {
            continue;
        }
        let Some(fixture) = sources.solved.fixtures.iter().find(|f| &f.id == fixture_id) else {
            continue;
        };
        let cells = head_count_with_path(resource_path, fixture);
        if !(0..cells as i64).contains(head) {
            return Err(format!(
                "`{fixture_id}` has {cells} cell{}, numbered from 0; there is no cell {head}",
                if cells == 1 { "" } else { "s" }
            ));
        }
    }
    let tree = sources.tree();
    let existing = node_answering_to(&tree, &normalized, "");
    let id = if replace {
        if let Some(existing) = existing {
            // Only an authored collection can be overwritten.
            groups_db::get_group(access, &existing.id).await?;
            Some(existing.id.as_str())
        } else {
            None
        }
    } else {
        None
    };
    // A whole fixture takes in its cells: those rows are not separate members.
    let wanted: HashSet<(String, i64)> = members
        .iter()
        .filter(|(fixture, head)| {
            *head == groups_db::WHOLE_FIXTURE
                || !members.contains(&(fixture.clone(), groups_db::WHOLE_FIXTURE))
        })
        .cloned()
        .collect();
    let removed: Vec<(String, i64)> = sources
        .members
        .iter()
        .filter(|member| Some(member.group_id.as_str()) == id)
        .map(|member| (member.fixture_id.clone(), member.head_index))
        .filter(|row| !wanted.contains(row))
        .collect();
    let group = save_group_changes(resource_path, access, id, name, &[], &[]).await?;
    for (fixture, head) in &removed {
        groups_db::remove_member_from_group(access, fixture, &group.id, Some(*head)).await?;
    }
    let mut seen = HashSet::new();
    for (fixture, head) in members {
        if wanted.contains(&(fixture.clone(), *head)) && seen.insert((fixture, *head)) {
            groups_db::add_member_to_group(access, fixture, &group.id, *head).await?;
        }
    }
    GroupSources::read(resource_path, access)
        .await?
        .hierarchy()
        .into_iter()
        .find(|node| node.id == group.id)
        .ok_or_else(|| "saved group could not be read".into())
}

/// Save a derived starting set. Only adds names nobody has taken; it never
/// replaces the memberships of a user's existing collections.
pub async fn snapshot_generated_groups(
    resource_path: &Path,
    access: &mut VenueAccess<'_, Write>,
) -> Result<(), String> {
    let venue = access.venue_id().to_owned();
    let mut sources = GroupSources::read(resource_path, access).await?;
    sources.derived = group_derivation::derive_groups(&sources.solved.facts);
    sources.manual.clear();
    let nodes = sources.tree();
    let uid = access.principal().to_owned();
    for (order, node) in nodes.iter().enumerate() {
        if node.name.is_empty() || sources.authored.iter().any(|g| g.id == node.id) {
            continue;
        }
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM fixture_groups WHERE venue_id = ? AND name = ?)",
        )
        .bind(&venue)
        .bind(&node.name)
        .fetch_one(&mut *access.connection())
        .await
        .map_err(|e| e.to_string())?;
        if exists {
            continue;
        }
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO fixture_groups (id, uid, venue_id, name, display_order) VALUES (?, ?, ?, ?, ?)")
            .bind(&id).bind(&uid).bind(&venue).bind(&node.name).bind(order as i64).execute(&mut *access.connection()).await.map_err(|e| e.to_string())?;
        for fixture in &node.fixtures {
            groups_db::add_member_to_group(access, fixture, &id, groups_db::WHOLE_FIXTURE).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{venue_cache_key, GroupNames, NO_GROUPS};
    use crate::models::groups::{GroupOrigin, GroupTreeNode};
    use crate::models::selection::Selection;
    use std::path::Path;

    fn tree(names: &[&str]) -> Vec<GroupTreeNode> {
        names
            .iter()
            .map(|name| GroupTreeNode {
                id: format!("id-{name}"),
                name: (*name).into(),
                label: (*name).into(),
                parent_id: None,
                origin: GroupOrigin::Manual,
                role: None,
                fixtures: vec![],
            })
            .collect()
    }

    /// A typed selection names groups; what is stored is their ids, and it
    /// reads back by name.
    #[test]
    fn a_typed_union_becomes_group_ids_and_reads_back() {
        let tree = tree(&["front_wash", "spots"]);
        let names = GroupNames::of_tree(&tree);
        for (typed, selection, spelled) in [
            ("all", Selection::All, "all"),
            ("  ALL ", Selection::All, "all"),
            ("spots", Selection::groups(["id-spots"]), "spots"),
            (
                "front_wash|spots | front_wash",
                Selection::groups(["id-front_wash", "id-spots"]),
                "front_wash | spots",
            ),
            ("Front Wash", Selection::groups(["id-front_wash"]), "front_wash"),
            ("spots | all", Selection::All, "all"),
        ] {
            assert_eq!(names.parse(typed), Ok(selection.clone()), "{typed}");
            assert_eq!(names.spell(&selection), spelled, "{typed}");
        }
    }

    /// A misspelled name, and an operator that is not a union, are refused
    /// with the names there are.
    #[test]
    fn an_unknown_name_is_refused_with_the_groups_there_are() {
        let tree = tree(&["front_wash", "spots"]);
        for typed in ["missing | spots", "front_wash & spots", "~spots", "spots |", ""] {
            let error = GroupNames::of_tree(&tree).parse(typed).unwrap_err();
            assert!(error.contains("Available groups: front_wash, spots"), "{typed}: {error}");
        }
    }

    /// A deleted group has no name: it drops out of the spelling, and a
    /// selection of nothing else says so.
    #[test]
    fn a_deleted_group_is_left_out_of_the_spelling() {
        let tree = tree(&["spots"]);
        let names = GroupNames::of_tree(&tree);
        assert_eq!(names.spell(&Selection::groups(["gone", "id-spots"])), "spots");
        assert_eq!(names.spell(&Selection::groups(["gone"])), NO_GROUPS);
        assert_eq!(names.spell(&Selection::Groups(vec![])), NO_GROUPS);
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
