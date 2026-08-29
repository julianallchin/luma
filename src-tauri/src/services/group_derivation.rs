//! Groups, derived.
//!
//! A group is not a bag someone filled in; it is a **set the rig already
//! describes**, shown as a tree. The rule below is the only one — role from the
//! fixture definition, rows from the structure fixtures hang on, position
//! splits within a role — and it is deterministic: the same venue derives the
//! same tree, with the same ids, every time.
//!
//! Manual work sits *on top* as overrides ([`crate::database::local::
//! group_overrides`]). A node with an override is never re-derived; deleting the
//! override restores derivation. That is the whole edit model.
//!
//! # The tree
//!
//! ```text
//! <role>                     every fixture of that role
//!   <row>                    the fixtures sharing one structure name
//!     top | bottom           the row's position split, when it separates
//!     left | right
//!   top | bottom             a cross-cut: the same split across two or more rows
//!   left | right
//! ```
//!
//! # The rule, in full
//!
//! **Role** — [`FixtureRole::of`], from the QLC+ `Type` plus the mode's channel
//! set. One table, documented there, no name heuristics.
//!
//! **Rows** — a placed fixture's row is named after the structure it is bolted
//! to; rows deriving the *same name* are one row, because "the horizontal ones"
//! is one thing a human points at even when it is two trusses. An unplaced
//! fixture's row is `unplaced`. Naming, in order:
//!
//! 1. A run hanging off a stage is a **wing**: `left wing` / `right wing` by
//!    which side of stage centre its fixtures sit on.
//! 2. Otherwise the run is named for the axis its fixtures spread along:
//!    `horizontal` (stage x) or `vertical` (up).
//! 3. Anything else — a deck, a piece, the venue floor — is its label, or its
//!    kind when it has none.
//!
//! **Splits** — role-wide classification, per-row emission. A fixture is `left`
//! or `right` by the sign of its resolved stage x against stage centre, and
//! `top` or `bottom` by its resolved z against the *role's* median z. A row
//! emits **one** split: the one across its own run — a horizontal run splits
//! top/bottom, a vertical one left/right — because splitting a run along itself
//! cuts one continuous line rather than separating things anyone points at. If
//! that split puts everything on one side the other axis is tried, and if that
//! is degenerate too the row has no children.
//!
//! **Cross-cuts** — under a role, `top`/`bottom`/`left`/`right` unioning the
//! same split across rows. Emitted only when **two or more** rows contribute: a
//! cross-cut drawing from one row is that row's child under a second name, and
//! there is one canonical way to name a set.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use luma_scene::venue::{NodeKind, ResolvedVenue, VenueGraph};

use crate::database::local::group_overrides::GroupOverride;
use crate::models::fixtures::{ChannelColour, ChannelType, FixtureDefinition, Mode};
use crate::models::groups::{normalize_group_name, GroupOrigin, GroupTreeNode};

// ---------------------------------------------------------------------------
// Role
// ---------------------------------------------------------------------------

/// What a fixture is *for*, which is what a group of them is named after.
///
/// Eight values, closed. This is the vocabulary a score speaks; a ninth would
/// mean every venue's tree grew a branch nothing references.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/bindings/groups.ts")]
#[serde(rename_all = "snake_case")]
pub enum FixtureRole {
    /// Colour over an area: pars, wash movers, anything with mixing and no gobo.
    Wash,
    /// A shaped beam: a mover with a gobo wheel.
    Spot,
    /// A hard column: a mover with a prism and no gobo, or an LED beam bar.
    Beam,
    /// Shutter-first.
    Strobe,
    /// Intensity only, no colour: the slab you point at the crowd.
    Blinder,
    /// Addressable cells — a pixel bar, a matrix, a multi-head mode.
    Pixel,
    /// Atmospherics and lasers: haze, smoke, fans, flowers, effects.
    Fx,
    /// Everything the table cannot place.
    Other,
}

impl FixtureRole {
    /// Every role, in the order the tree lists them.
    pub const ALL: [FixtureRole; 8] = [
        FixtureRole::Wash,
        FixtureRole::Spot,
        FixtureRole::Beam,
        FixtureRole::Strobe,
        FixtureRole::Blinder,
        FixtureRole::Pixel,
        FixtureRole::Fx,
        FixtureRole::Other,
    ];

    /// The wire name, and the stem of every group name under this role.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FixtureRole::Wash => "wash",
            FixtureRole::Spot => "spot",
            FixtureRole::Beam => "beam",
            FixtureRole::Strobe => "strobe",
            FixtureRole::Blinder => "blinder",
            FixtureRole::Pixel => "pixel",
            FixtureRole::Fx => "fx",
            FixtureRole::Other => "other",
        }
    }

    /// What the role's node is called in the tree — a plural, because the node
    /// is a set of fixtures rather than one fixture's type. `Pixel` reads as
    /// `led bars` for the same reason: it is what the things in it are called.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            FixtureRole::Wash => "washes",
            FixtureRole::Spot => "spots",
            FixtureRole::Beam => "beams",
            FixtureRole::Strobe => "strobes",
            FixtureRole::Blinder => "blinders",
            FixtureRole::Pixel => "led bars",
            FixtureRole::Fx => "fx",
            FixtureRole::Other => "other",
        }
    }

    /// The role of a fixture, from its definition and the mode it is patched in.
    ///
    /// The table, first match wins:
    ///
    /// | test | role |
    /// |---|---|
    /// | `Type` is `Smoke`, `Hazer`, `Fan`, `Laser`, `Flower` or `Effect` | [`Fx`](FixtureRole::Fx) |
    /// | `Type` is `Strobe` | [`Strobe`](FixtureRole::Strobe) |
    /// | `Type` is `LED Bar (Beams)` | [`Beam`](FixtureRole::Beam) |
    /// | a physical layout of more than one cell, `LED Bar (Pixels)`, or a mode with more than two heads | [`Pixel`](FixtureRole::Pixel) |
    /// | has a `Pan` or `Tilt` channel, and a `Gobo` channel | [`Spot`](FixtureRole::Spot) |
    /// | has a `Pan` or `Tilt` channel, and a `Prism` channel | [`Beam`](FixtureRole::Beam) |
    /// | has a `Pan` or `Tilt` channel | [`Wash`](FixtureRole::Wash) |
    /// | has any colour — a `Colour` channel, or an intensity channel carrying a colour | [`Wash`](FixtureRole::Wash) |
    /// | has an `Intensity` channel | [`Blinder`](FixtureRole::Blinder) |
    /// | otherwise | [`Other`](FixtureRole::Other) |
    ///
    /// Only channels the *mode* patches are read: a definition's 30-channel
    /// extended mode does not make a fixture patched in 4-channel dimmer mode a
    /// spot.
    ///
    /// Two honest approximations, recorded rather than hidden. A mover with
    /// neither gobo nor prism is called a wash, which is what a wash mover is
    /// and what a beam mover in a stripped-down mode is not. And a fixture with
    /// nothing but intensity is called a blinder, because in a rig that is what
    /// a colourless slab is — a plain dimmer channel lands there too.
    #[must_use]
    pub fn of(definition: &FixtureDefinition, mode: &Mode) -> FixtureRole {
        let type_ = definition.type_.to_ascii_lowercase();
        if matches!(
            type_.as_str(),
            "smoke" | "hazer" | "fan" | "laser" | "flower" | "effect"
        ) {
            return FixtureRole::Fx;
        }
        if type_ == "strobe" {
            return FixtureRole::Strobe;
        }
        if type_ == "led bar (beams)" {
            return FixtureRole::Beam;
        }

        let cells = definition
            .physical
            .as_ref()
            .and_then(|p| p.layout.as_ref())
            .map_or(1, |l| u64::from(l.width) * u64::from(l.height));
        if type_ == "led bar (pixels)" || cells > 1 || mode.heads.len() > 2 {
            return FixtureRole::Pixel;
        }

        let mut moves = false;
        let mut gobo = false;
        let mut prism = false;
        let mut colour = false;
        let mut intensity = false;
        for patched in &mode.channels {
            let Some(channel) = definition.channels.iter().find(|c| c.name == patched.name) else {
                continue;
            };
            match channel.get_type() {
                ChannelType::Pan | ChannelType::Tilt => moves = true,
                ChannelType::Gobo => gobo = true,
                ChannelType::Prism => prism = true,
                ChannelType::Colour => colour = true,
                ChannelType::Intensity => {
                    if channel.get_colour() == ChannelColour::None {
                        intensity = true;
                    } else {
                        colour = true;
                    }
                }
                _ => {}
            }
        }

        match (moves, gobo, prism, colour, intensity) {
            (true, true, _, _, _) => FixtureRole::Spot,
            (true, false, true, _, _) => FixtureRole::Beam,
            (true, false, false, _, _) => FixtureRole::Wash,
            (false, _, _, true, _) => FixtureRole::Wash,
            (false, _, _, false, true) => FixtureRole::Blinder,
            _ => FixtureRole::Other,
        }
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Where a fixture hangs, as derivation reads it.
#[derive(Debug, Clone)]
pub struct FixturePlacement {
    /// The venue node it is bolted to — the row it shares with its neighbours.
    pub parent: String,
    /// Resolved position, data space (`+X` stage right, `+Y` upstage, `+Z` up),
    /// metres.
    pub position: [f64; 3],
}

/// One patched fixture.
#[derive(Debug, Clone)]
pub struct FixtureFact {
    /// The `fixtures` row id, which is also its venue-graph node id.
    pub id: String,
    /// The definition's model, for `<model> <n>` labels.
    pub model: String,
    pub role: FixtureRole,
    /// `None` for a fixture in the patch tray. Unplaced fixtures still get a
    /// role and a row (`unplaced`); they cannot get a position split.
    pub placement: Option<FixturePlacement>,
}

/// One structure node fixtures hang on.
#[derive(Debug, Clone)]
pub struct StructureFact {
    /// The venue-graph node id, as a placement names it.
    pub node: String,
    /// Whether this node's parent chain reaches a stage. A run bolted to the
    /// deck is a wing; one hanging off the house structure is not.
    pub on_stage: bool,
    /// The node's own label, which names the row when it is not a run.
    pub label: Option<String>,
    /// The node's kind, the fallback name.
    pub kind: String,
}

/// Everything the rule reads. Assembled from the venue graph and the patch list
/// by [`super::groups::venue_facts`]; a pure value so the rule can be tested
/// without a database, a catalog or a solve.
#[derive(Debug, Clone, Default)]
pub struct VenueFacts {
    /// Namespaces the derived ids, so two venues with the same rig do not share
    /// group identity.
    pub venue_id: String,
    /// In creation order — which is the order `<model> <n>` counts in and the
    /// order rows appear in.
    pub fixtures: Vec<FixtureFact>,
    pub structures: Vec<StructureFact>,
    /// Stage x that `left` and `right` are measured against.
    pub stage_centre_x: f64,
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// One node of the derived tree.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedGroup {
    /// Deterministic in the venue and the path — see [`derived_id`].
    pub id: String,
    /// The display path from the role down, e.g. `["spots", "left wing", "top"]`.
    pub path: Vec<String>,
    /// The selection name: the path in snake_case, e.g. `spots_left_wing_top`.
    /// Unique within the venue because the path is.
    pub name: String,
    /// The parent node's id, `None` for a role.
    pub parent: Option<String>,
    /// The role this node sits under. Every derived node has one — the path
    /// starts at a role.
    pub role: FixtureRole,
    /// Fixture ids, in creation order.
    pub members: Vec<String>,
}

impl DerivedGroup {
    /// What the tree shows: the last path segment.
    #[must_use]
    pub fn label(&self) -> &str {
        self.path.last().map_or("", String::as_str)
    }
}

/// The whole derivation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DerivedTree {
    /// Parents before children, roles in [`FixtureRole::ALL`] order.
    pub groups: Vec<DerivedGroup>,
    /// Fixture id → `<model> <n>`, `n` counting per model in creation order.
    pub fixture_labels: BTreeMap<String, String>,
}

/// A derived group's id: a hash of the venue and the derivation path, rendered
/// as a UUID so it sits in the same `TEXT` column an authored group does.
///
/// Stable by construction, which is the point: a pattern naming a group holds
/// this id, and re-deriving after someone hangs one more par must not hand the
/// same set a new identity. Version nibble 8 — the RFC's "custom" version —
/// says out loud that this is not a random uuid.
#[must_use]
pub fn derived_id(venue_id: &str, path: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(venue_id.as_bytes());
    hasher.update([0x1f]);
    hasher.update(path.join("/").as_bytes());
    let digest = hasher.finalize();
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest[..16]);
    b[6] = (b[6] & 0x0f) | 0x80; // version 8
    b[8] = (b[8] & 0x3f) | 0x80; // RFC variant
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15],
    )
}

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

/// Which half of a split a fixture is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Half {
    Top,
    Bottom,
    Left,
    Right,
}

impl Half {
    fn as_str(self) -> &'static str {
        match self {
            Half::Top => "top",
            Half::Bottom => "bottom",
            Half::Left => "left",
            Half::Right => "right",
        }
    }
}

/// The two axes a set of fixtures can be cut along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    /// `top` / `bottom`, against the role's median z.
    Level,
    /// `left` / `right`, against stage centre.
    Side,
}

impl Axis {
    fn halves(self) -> [Half; 2] {
        match self {
            Axis::Level => [Half::Top, Half::Bottom],
            Axis::Side => [Half::Left, Half::Right],
        }
    }

    fn other(self) -> Axis {
        match self {
            Axis::Level => Axis::Side,
            Axis::Side => Axis::Level,
        }
    }
}

/// Derive a venue's group tree.
///
/// Pure and total: no database, no catalog, no failure mode. Every list it
/// produces is in a defined order, so two derivations of the same facts are
/// equal — which is what makes [`derived_id`] a stable identity rather than a
/// hopeful one.
#[must_use]
pub fn derive_groups(facts: &VenueFacts) -> DerivedTree {
    let mut tree = DerivedTree {
        fixture_labels: fixture_labels(&facts.fixtures),
        ..DerivedTree::default()
    };

    for role in FixtureRole::ALL {
        let members: Vec<&FixtureFact> = facts
            .fixtures
            .iter()
            .filter(|fixture| fixture.role == role)
            .collect();
        if members.is_empty() {
            continue;
        }

        let role_path = vec![role.display_name().to_string()];
        let role_id = push(&mut tree, facts, None, role, role_path.clone(), &members);

        // The classification is role-wide: one median z and one stage centre for
        // every fixture of the role, so `top` means the same height whichever
        // row it is read under.
        let median_z = median_z(&members);
        let half = |fixture: &FixtureFact, axis: Axis| -> Option<Half> {
            let position = fixture.placement.as_ref()?.position;
            Some(match axis {
                Axis::Level => {
                    if position[2] >= median_z? {
                        Half::Top
                    } else {
                        Half::Bottom
                    }
                }
                Axis::Side => {
                    if position[0] < facts.stage_centre_x {
                        Half::Left
                    } else {
                        Half::Right
                    }
                }
            })
        };

        // Rows, keyed by name so two trusses a human calls "the horizontal ones"
        // are one row, in the order their first fixture was patched.
        let mut rows: Vec<Row<'_>> = Vec::new();
        for fixture in &members {
            let (name, structure) = match &fixture.placement {
                Some(placement) => (
                    row_name(facts, &members, &placement.parent),
                    Some(placement.parent.clone()),
                ),
                None => (UNPLACED.to_string(), None),
            };
            let index = match rows.iter().position(|row| row.name == name) {
                Some(index) => index,
                None => {
                    rows.push(Row {
                        name,
                        members: Vec::new(),
                        structures: Vec::new(),
                    });
                    rows.len() - 1
                }
            };
            let row = &mut rows[index];
            row.members.push(fixture);
            if let Some(structure) = structure {
                if !row.structures.contains(&structure) {
                    row.structures.push(structure);
                }
            }
        }

        // `half name -> rows that emitted it`, for the cross-cuts below.
        let mut cross: BTreeMap<&'static str, (Vec<&FixtureFact>, usize)> = BTreeMap::new();

        for row in &rows {
            let mut row_path = role_path.clone();
            row_path.push(row.name.clone());
            let row_id = push(
                &mut tree,
                facts,
                Some(&role_id),
                role,
                row_path.clone(),
                &row.members,
            );

            // A row splits across its own run: the axis its fixtures do *not*
            // spread along. When that puts everything on one side — a wing is
            // all on one side of the stage by definition — the other axis is
            // tried, and when that is degenerate too the row has no children.
            let across = row_axis(&row.structures, &members).other();
            for axis in [across, across.other()] {
                let halves = axis.halves().map(|which| {
                    (
                        which,
                        row.members
                            .iter()
                            .copied()
                            .filter(|fixture| half(fixture, axis) == Some(which))
                            .collect::<Vec<_>>(),
                    )
                });
                if halves.iter().any(|(_, side)| side.is_empty()) {
                    continue;
                }
                for (which, side) in &halves {
                    let mut path = row_path.clone();
                    path.push(which.as_str().to_string());
                    push(&mut tree, facts, Some(&row_id), role, path, side);
                    let entry = cross.entry(which.as_str()).or_default();
                    entry.0.extend(side.iter().copied());
                    entry.1 += 1;
                }
                break;
            }
        }

        // A cross-cut drawing from a single row *is* that row's child, under a
        // second name. Two names for one set is the thing this whole design is
        // trying not to do, so it is not emitted.
        for which in [Half::Top, Half::Bottom, Half::Left, Half::Right] {
            let Some((side, rows_contributing)) = cross.get(which.as_str()) else {
                continue;
            };
            if *rows_contributing < 2 {
                continue;
            }
            let mut path = role_path.clone();
            path.push(which.as_str().to_string());
            push(&mut tree, facts, Some(&role_id), role, path, side);
        }
    }

    tree
}

/// The row name for a placement with no structure behind it.
const UNPLACED: &str = "unplaced";

struct Row<'a> {
    name: String,
    members: Vec<&'a FixtureFact>,
    /// The structure nodes that produced this name, in first-seen order.
    structures: Vec<String>,
}

/// Add one node, returning its id.
fn push(
    tree: &mut DerivedTree,
    facts: &VenueFacts,
    parent: Option<&str>,
    role: FixtureRole,
    path: Vec<String>,
    members: &[&FixtureFact],
) -> String {
    let id = derived_id(&facts.venue_id, &path);
    tree.groups.push(DerivedGroup {
        id: id.clone(),
        name: normalize_group_name(&path.join(" ")),
        path,
        parent: parent.map(str::to_string),
        role,
        members: members.iter().map(|fixture| fixture.id.clone()).collect(),
    });
    id
}

/// `<model> <n>`, `n` counting per model over the venue's creation order.
fn fixture_labels(fixtures: &[FixtureFact]) -> BTreeMap<String, String> {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    fixtures
        .iter()
        .map(|fixture| {
            let n = seen.entry(fixture.model.as_str()).or_insert(0);
            *n += 1;
            (fixture.id.clone(), format!("{} {}", fixture.model, n))
        })
        .collect()
}

/// The role's median height, `None` when nothing in it is placed.
///
/// The lower median on an even count, so a role of two splits one and one
/// rather than two and none.
fn median_z(members: &[&FixtureFact]) -> Option<f64> {
    let mut heights: Vec<f64> = members
        .iter()
        .filter_map(|fixture| Some(fixture.placement.as_ref()?.position[2]))
        .collect();
    if heights.is_empty() {
        return None;
    }
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(heights[heights.len() / 2])
}

/// Which way one structure's fixtures run.
///
/// Read off the fixtures rather than the structure's geometry on purpose: a
/// run's direction is the line the things on it lie along, which is true of a
/// generated truss, a measured GLB and a wall alike, and needs no catalog.
fn structure_axis(structure: &str, members: &[&FixtureFact]) -> Option<Axis> {
    let positions: Vec<[f64; 3]> = members
        .iter()
        .filter_map(|fixture| {
            let placement = fixture.placement.as_ref()?;
            (placement.parent == structure).then_some(placement.position)
        })
        .collect();
    if positions.len() < 2 {
        return None;
    }
    let spread = |axis: usize| {
        let (min, max) = positions.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
            (lo.min(p[axis]), hi.max(p[axis]))
        });
        max - min
    };
    // x spread against z spread: the run is horizontal or it is vertical.
    Some(if spread(0) >= spread(2) {
        Axis::Side
    } else {
        Axis::Level
    })
}

/// A merged row's axis: what most of its structures agree on. Ties, and a row
/// whose structures have no axis at all, read as horizontal — the default a
/// single fixture on a truss gets, and the one that splits it by height.
fn row_axis(structures: &[String], members: &[&FixtureFact]) -> Axis {
    let (mut side, mut level) = (0, 0);
    for structure in structures {
        match structure_axis(structure, members) {
            Some(Axis::Side) => side += 1,
            Some(Axis::Level) => level += 1,
            None => {}
        }
    }
    if level > side {
        Axis::Level
    } else {
        Axis::Side
    }
}

/// What the row containing `structure` is called.
fn row_name(facts: &VenueFacts, members: &[&FixtureFact], structure: &str) -> String {
    let fact = facts.structures.iter().find(|s| s.node == structure);
    let axis = structure_axis(structure, members);

    // A run bolted to a stage is a wing, and a wing is named by its side —
    // which is the fixtures' side, not the truss's, because that is the answer
    // the tree is asked for.
    if fact.is_some_and(|fact| fact.on_stage) {
        let mean_x = mean_x(structure, members);
        if let Some(mean_x) = mean_x {
            return if mean_x < facts.stage_centre_x {
                "left wing".to_string()
            } else {
                "right wing".to_string()
            };
        }
    }

    match axis {
        Some(Axis::Side) => "horizontal".to_string(),
        Some(Axis::Level) => "vertical".to_string(),
        None => fact
            .and_then(|fact| fact.label.clone())
            .or_else(|| fact.map(|fact| fact.kind.clone()))
            .unwrap_or_else(|| UNPLACED.to_string()),
    }
}

fn mean_x(structure: &str, members: &[&FixtureFact]) -> Option<f64> {
    let xs: Vec<f64> = members
        .iter()
        .filter_map(|fixture| {
            let placement = fixture.placement.as_ref()?;
            (placement.parent == structure).then_some(placement.position[0])
        })
        .collect();
    (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
}

// ---------------------------------------------------------------------------
// Reading the facts out of a solved venue
// ---------------------------------------------------------------------------

/// What the patch list knows about a fixture that the graph does not.
///
/// The split is the seam: the database owns definitions and therefore roles,
/// the graph owns placement, and [`facts_from`] is where they meet. Neither
/// half has to know the other's storage.
#[derive(Debug, Clone)]
pub struct FixtureIdentity {
    /// The `fixtures` row id, which is also the venue-graph node id.
    pub id: String,
    pub model: String,
    pub role: FixtureRole,
}

/// Assemble [`VenueFacts`] from a solved venue and the patch list.
///
/// `fixtures` is in creation order and that order is carried through: it is
/// what `<model> <n>` counts in and what orders the rows.
#[must_use]
pub fn facts_from(
    venue_id: &str,
    solved: &ResolvedVenue,
    graph: &VenueGraph,
    fixtures: &[FixtureIdentity],
) -> VenueFacts {
    let mut facts = VenueFacts {
        venue_id: venue_id.to_string(),
        stage_centre_x: stage_centre_x(solved),
        ..VenueFacts::default()
    };

    for identity in fixtures {
        let placement = solved.pose(&identity.id).and_then(|pose| {
            let parent = pose.parent.clone()?;
            Some(FixturePlacement {
                parent,
                position: pose.data_pose().0,
            })
        });
        if let Some(placement) = &placement {
            if !facts
                .structures
                .iter()
                .any(|structure| structure.node == placement.parent)
            {
                let pose = solved.pose(&placement.parent);
                facts.structures.push(StructureFact {
                    node: placement.parent.clone(),
                    on_stage: on_stage(graph, &placement.parent),
                    label: pose.and_then(|pose| pose.label.clone()),
                    kind: pose.map_or("piece", |pose| pose.kind.as_str()).to_string(),
                });
            }
        }
        facts.fixtures.push(FixtureFact {
            id: identity.id.clone(),
            model: identity.model.clone(),
            role: identity.role,
            placement,
        });
    }

    facts
}

/// The x that `left` and `right` are measured against: the middle of the
/// stages, or the venue frame's own origin when the room has none.
fn stage_centre_x(solved: &ResolvedVenue) -> f64 {
    let xs: Vec<f64> = solved
        .poses()
        .filter(|pose| pose.kind == NodeKind::Stage)
        .map(|pose| pose.data_pose().0[0])
        .collect();
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

/// Whether `node`'s parent chain reaches a stage — what makes a run a wing
/// rather than house structure.
fn on_stage(graph: &VenueGraph, node: &str) -> bool {
    let mut at = node.to_string();
    // The chain is acyclic by the graph's insertion invariant; the bound is
    // belt and braces against a row set that arrived some other way.
    for _ in 0..64 {
        match graph.node(&at) {
            Some(found) if found.kind == NodeKind::Stage => return true,
            Some(_) => {}
            None => return false,
        }
        match graph.edge(&at) {
            Some(edge) => at = edge.parent.clone(),
            None => return false,
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Derivation + overrides + authored groups: the merged tree
// ---------------------------------------------------------------------------

/// An authored `fixture_groups` row and the fixtures in it.
#[derive(Debug, Clone)]
pub struct ManualGroup {
    pub id: String,
    pub name: String,
    pub fixtures: Vec<String>,
}

/// The tree the patch page shows: derivation, with the overrides applied, plus
/// the groups someone made by hand.
///
/// Pure, so the edit model is testable without a database. The model is one
/// sentence: **an override names a derived node, it does not replace it.** A
/// renamed node keeps its new name and goes on collecting whatever the rule
/// puts in it — which is why renaming `spots/left wing/top` and then hanging
/// four more movers on that wing files the new movers under the name you gave
/// it, rather than under a set that stopped listening.
///
/// Merge is the one edit that moves fixtures, and it moves them by *reference*:
/// the target counts the source's members alongside its own, both still
/// derived, and deleting the row undoes it.
///
/// Parents come before children.
#[must_use]
pub fn merge_tree(
    tree: &DerivedTree,
    overrides: &[GroupOverride],
    manual: &[ManualGroup],
) -> Vec<GroupTreeNode> {
    let lookup = |id: &str| overrides.iter().find(|row| row.group_id == id);
    // Whatever was merged into each node, by target id. Read once: a merge is
    // rare and a scan per node would be quadratic in the tree.
    let mut folded: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for row in overrides {
        if let Some(target) = row.merged_into.as_deref() {
            folded.entry(target).or_default().push(&row.group_id);
        }
    }
    let members_of = |id: &str, own: &[String]| -> Vec<String> {
        let mut members = own.to_vec();
        for source in folded.get(id).into_iter().flatten() {
            let from = tree
                .groups
                .iter()
                .find(|group| &group.id == source)
                .map(|group| group.members.as_slice())
                .unwrap_or_default();
            for fixture in from {
                if !members.contains(fixture) {
                    members.push(fixture.clone());
                }
            }
        }
        members
    };

    let mut nodes = Vec::with_capacity(tree.groups.len() + manual.len());
    for group in &tree.groups {
        let row = lookup(&group.id);
        // Merged away: its fixtures are counted under the target, and showing
        // the husk would count them twice.
        if row.is_some_and(|row| row.merged_into.is_some()) {
            continue;
        }
        let label = row
            .and_then(|row| row.label.clone())
            .unwrap_or_else(|| group.label().to_string());
        // A rename renames the leaf, not the branch above it, so the selection
        // name keeps its namespace and stays unique.
        let mut path = group.path.clone();
        path.pop();
        path.push(label.clone());
        nodes.push(GroupTreeNode {
            id: group.id.clone(),
            name: normalize_group_name(&path.join(" ")),
            label,
            parent_id: match row.and_then(|row| row.parent_id.as_deref()) {
                None => group.parent.clone(),
                Some("") => None,
                Some(parent) => Some(parent.to_string()),
            },
            origin: if row.is_some() {
                GroupOrigin::Edited
            } else {
                GroupOrigin::Derived
            },
            role: Some(group.role),
            fixtures: members_of(&group.id, &group.members),
        });
    }

    // An authored group can be the *target* of a merge, and can be renamed or
    // moved like anything else. Same rule: the row names it, the members are
    // still whoever is in it.
    for group in manual {
        let row = lookup(&group.id);
        if row.is_some_and(|row| row.merged_into.is_some()) {
            continue;
        }
        let label = row
            .and_then(|row| row.label.clone())
            .unwrap_or_else(|| group.name.clone());
        nodes.push(GroupTreeNode {
            id: group.id.clone(),
            name: normalize_group_name(&label),
            label,
            parent_id: row
                .and_then(|row| row.parent_id.as_deref())
                .filter(|parent| !parent.is_empty())
                .map(str::to_string),
            origin: if row.is_some() {
                GroupOrigin::Edited
            } else {
                GroupOrigin::Manual
            },
            role: None,
            fixtures: members_of(&group.id, &group.fixtures),
        });
    }

    nodes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::fixtures::{
        Channel, FixtureDefinition, Group, Head, Mode, ModeChannel, Physical,
    };

    // -----------------------------------------------------------------------
    // Role
    // -----------------------------------------------------------------------

    fn channel(name: &str, group: &str) -> Channel {
        Channel {
            name: name.into(),
            preset: None,
            group: Some(Group {
                byte: 0,
                value: group.into(),
            }),
            capabilities: Vec::new(),
        }
    }

    fn coloured(name: &str, preset: &str) -> Channel {
        Channel {
            name: name.into(),
            preset: Some(preset.into()),
            group: None,
            capabilities: Vec::new(),
        }
    }

    fn definition(type_: &str, channels: Vec<Channel>) -> FixtureDefinition {
        FixtureDefinition {
            manufacturer: "Test".into(),
            model: "Test".into(),
            type_: type_.into(),
            modes: Vec::new(),
            physical: None,
            channels,
        }
    }

    fn mode(channels: &[&str]) -> Mode {
        Mode {
            name: "Test".into(),
            channels: channels
                .iter()
                .enumerate()
                .map(|(i, name)| ModeChannel {
                    number: i as u32,
                    name: (*name).into(),
                })
                .collect(),
            heads: Vec::new(),
        }
    }

    #[test]
    fn role_reads_the_type_before_the_channels() {
        // A hazer with a dimmer is atmospherics, not a blinder.
        let hazer = definition("Hazer", vec![channel("Dimmer", "Intensity")]);
        assert_eq!(FixtureRole::of(&hazer, &mode(&["Dimmer"])), FixtureRole::Fx);

        let strobe = definition("Strobe", vec![channel("Dimmer", "Intensity")]);
        assert_eq!(
            FixtureRole::of(&strobe, &mode(&["Dimmer"])),
            FixtureRole::Strobe
        );
    }

    #[test]
    fn movers_split_on_gobo_then_prism() {
        let channels = |extra: Vec<Channel>| {
            let mut base = vec![channel("Pan", "Pan"), channel("Tilt", "Tilt")];
            base.extend(extra);
            base
        };
        let spot = definition("Moving Head", channels(vec![channel("Gobo", "Gobo")]));
        assert_eq!(
            FixtureRole::of(&spot, &mode(&["Pan", "Tilt", "Gobo"])),
            FixtureRole::Spot
        );

        let beam = definition("Moving Head", channels(vec![channel("Prism", "Prism")]));
        assert_eq!(
            FixtureRole::of(&beam, &mode(&["Pan", "Tilt", "Prism"])),
            FixtureRole::Beam
        );

        // A gobo *and* a prism is a spot: the shape is what it is for.
        let both = definition(
            "Moving Head",
            channels(vec![channel("Gobo", "Gobo"), channel("Prism", "Prism")]),
        );
        assert_eq!(
            FixtureRole::of(&both, &mode(&["Pan", "Tilt", "Gobo", "Prism"])),
            FixtureRole::Spot
        );

        let wash = definition(
            "Moving Head",
            channels(vec![coloured("Red", "IntensityRed")]),
        );
        assert_eq!(
            FixtureRole::of(&wash, &mode(&["Pan", "Tilt", "Red"])),
            FixtureRole::Wash
        );
    }

    #[test]
    fn only_the_patched_mode_is_read() {
        // The definition has a gobo wheel; the mode patched here does not
        // reach it, so this is not a spot.
        let def = definition(
            "Moving Head",
            vec![
                channel("Pan", "Pan"),
                channel("Tilt", "Tilt"),
                channel("Gobo", "Gobo"),
                coloured("Red", "IntensityRed"),
            ],
        );
        assert_eq!(
            FixtureRole::of(&def, &mode(&["Pan", "Tilt", "Red"])),
            FixtureRole::Wash
        );
    }

    #[test]
    fn static_fixtures_split_on_colour() {
        let par = definition("Color Changer", vec![coloured("Red", "IntensityRed")]);
        assert_eq!(FixtureRole::of(&par, &mode(&["Red"])), FixtureRole::Wash);

        let blinder = definition("Dimmer", vec![channel("Dimmer", "Intensity")]);
        assert_eq!(
            FixtureRole::of(&blinder, &mode(&["Dimmer"])),
            FixtureRole::Blinder
        );

        let nothing = definition("Other", vec![channel("Control", "Maintenance")]);
        assert_eq!(
            FixtureRole::of(&nothing, &mode(&["Control"])),
            FixtureRole::Other
        );
    }

    #[test]
    fn cells_and_heads_make_a_pixel() {
        let mut bar = definition("Color Changer", vec![coloured("Red", "IntensityRed")]);
        bar.physical = Some(Physical {
            dimensions: None,
            layout: Some(crate::models::fixtures::Layout {
                width: 8,
                height: 1,
            }),
            bulb: None,
            lens: None,
            focus: None,
            technical: None,
        });
        assert_eq!(FixtureRole::of(&bar, &mode(&["Red"])), FixtureRole::Pixel);

        let mut heads = definition("Color Changer", vec![coloured("Red", "IntensityRed")]);
        heads.physical = None;
        let mut multi = mode(&["Red"]);
        multi.heads = (0..4).map(|_| Head { channels: vec![0] }).collect();
        assert_eq!(FixtureRole::of(&heads, &multi), FixtureRole::Pixel);
    }

    // -----------------------------------------------------------------------
    // Structure
    // -----------------------------------------------------------------------

    fn fixture(id: &str, role: FixtureRole, parent: &str, at: [f64; 3]) -> FixtureFact {
        FixtureFact {
            id: id.into(),
            model: "Bar".into(),
            role,
            placement: Some(FixturePlacement {
                parent: parent.into(),
                position: at,
            }),
        }
    }

    fn structure(node: &str, on_stage: bool) -> StructureFact {
        StructureFact {
            node: node.into(),
            on_stage,
            label: None,
            kind: "run".into(),
        }
    }

    /// Two horizontal runs at different heights: one merged `horizontal` row,
    /// split top/bottom — across the run, not along it.
    fn horizontal_facts() -> VenueFacts {
        VenueFacts {
            venue_id: "v".into(),
            stage_centre_x: 0.0,
            structures: vec![structure("high", false), structure("low", false)],
            fixtures: vec![
                fixture("a", FixtureRole::Wash, "high", [-2.0, 0.0, 5.0]),
                fixture("b", FixtureRole::Wash, "high", [2.0, 0.0, 5.0]),
                fixture("c", FixtureRole::Wash, "low", [-2.0, 0.0, 1.0]),
                fixture("d", FixtureRole::Wash, "low", [2.0, 0.0, 1.0]),
            ],
        }
    }

    fn paths(tree: &DerivedTree) -> Vec<String> {
        tree.groups
            .iter()
            .map(|group| group.path.join("/"))
            .collect()
    }

    #[test]
    fn a_horizontal_row_splits_by_height() {
        let tree = derive_groups(&horizontal_facts());
        assert_eq!(
            paths(&tree),
            vec![
                "washes",
                "washes/horizontal",
                "washes/horizontal/top",
                "washes/horizontal/bottom",
            ]
        );
    }

    #[test]
    fn a_vertical_row_splits_by_side() {
        let facts = VenueFacts {
            venue_id: "v".into(),
            stage_centre_x: 0.0,
            structures: vec![structure("left", false), structure("right", false)],
            fixtures: vec![
                fixture("a", FixtureRole::Wash, "left", [-4.0, 0.0, 1.0]),
                fixture("b", FixtureRole::Wash, "left", [-4.0, 0.0, 5.0]),
                fixture("c", FixtureRole::Wash, "right", [4.0, 0.0, 1.0]),
                fixture("d", FixtureRole::Wash, "right", [4.0, 0.0, 5.0]),
            ],
        };
        // Both runs are vertical, so the row is one `vertical` and the split is
        // the one across it. Height would also have separated these, which is
        // exactly the ambiguity the "across the run" rule settles.
        assert_eq!(
            paths(&derive_groups(&facts)),
            vec![
                "washes",
                "washes/vertical",
                "washes/vertical/left",
                "washes/vertical/right",
            ]
        );
    }

    #[test]
    fn a_degenerate_split_falls_back_to_the_other_axis() {
        // Two vertical towers, both stage left. The split across them is
        // side, and every fixture is on the same side of centre — so height
        // is used instead, and no row is left childless for want of a rule.
        let facts = VenueFacts {
            venue_id: "v".into(),
            stage_centre_x: 0.0,
            structures: vec![structure("tower", true)],
            fixtures: vec![
                fixture("a", FixtureRole::Spot, "tower", [-4.0, 0.0, 1.0]),
                fixture("b", FixtureRole::Spot, "tower", [-4.0, 0.0, 5.0]),
            ],
        };
        assert_eq!(
            paths(&derive_groups(&facts)),
            vec![
                "spots",
                "spots/left wing",
                "spots/left wing/top",
                "spots/left wing/bottom",
            ]
        );
    }

    #[test]
    fn a_cross_cut_needs_two_rows() {
        // One row's `top` is that row's child; naming it twice is what the
        // cross-cut rule refuses.
        let one = derive_groups(&horizontal_facts());
        assert!(!paths(&one).contains(&"washes/top".to_string()));

        // Two wings, each split by height — now `top` means something no single
        // row says.
        let mut facts = VenueFacts {
            venue_id: "v".into(),
            stage_centre_x: 0.0,
            structures: vec![structure("left", true), structure("right", true)],
            fixtures: Vec::new(),
        };
        for (id, parent, x) in [("a", "left", -4.0), ("b", "right", 4.0)] {
            for (n, z) in [(0, 1.0), (1, 5.0)] {
                facts.fixtures.push(fixture(
                    &format!("{id}{n}"),
                    FixtureRole::Spot,
                    parent,
                    [x, 0.0, z],
                ));
            }
        }
        let paths = paths(&derive_groups(&facts));
        assert!(paths.contains(&"spots/top".to_string()));
        assert!(paths.contains(&"spots/bottom".to_string()));
        // No row emitted a side split, so there is no side cross-cut.
        assert!(!paths.contains(&"spots/left".to_string()));
    }

    #[test]
    fn unplaced_fixtures_get_a_row_and_no_split() {
        let facts = VenueFacts {
            venue_id: "v".into(),
            stage_centre_x: 0.0,
            structures: Vec::new(),
            fixtures: vec![FixtureFact {
                id: "tray".into(),
                model: "Bar".into(),
                role: FixtureRole::Wash,
                placement: None,
            }],
        };
        assert_eq!(
            paths(&derive_groups(&facts)),
            vec!["washes", "washes/unplaced"]
        );
    }

    #[test]
    fn labels_count_per_model_in_creation_order() {
        let mut facts = horizontal_facts();
        facts.fixtures[1].model = "Mover".into();
        let tree = derive_groups(&facts);
        assert_eq!(tree.fixture_labels["a"], "Bar 1");
        assert_eq!(tree.fixture_labels["b"], "Mover 1");
        assert_eq!(tree.fixture_labels["c"], "Bar 2");
        assert_eq!(tree.fixture_labels["d"], "Bar 3");
    }

    // -----------------------------------------------------------------------
    // Identity
    // -----------------------------------------------------------------------

    #[test]
    fn derivation_is_deterministic() {
        let facts = horizontal_facts();
        assert_eq!(derive_groups(&facts), derive_groups(&facts));
    }

    #[test]
    fn ids_survive_the_rig_growing() {
        let before = derive_groups(&horizontal_facts());
        let mut facts = horizontal_facts();
        // One more par on the high run: the sets change, the paths do not.
        facts
            .fixtures
            .push(fixture("e", FixtureRole::Wash, "high", [0.0, 0.0, 5.0]));
        let after = derive_groups(&facts);

        for group in &before.groups {
            let same = after
                .groups
                .iter()
                .find(|other| other.path == group.path)
                .expect("the path still derives");
            assert_eq!(same.id, group.id, "{:?} changed identity", group.path);
        }
        assert!(after
            .groups
            .iter()
            .find(|g| g.path == ["washes", "horizontal", "top"])
            .is_some_and(|g| g.members.len() == 3));
    }

    #[test]
    fn ids_are_namespaced_by_venue() {
        let path = vec!["washes".to_string()];
        assert_ne!(derived_id("a", &path), derived_id("b", &path));
    }

    // -----------------------------------------------------------------------
    // Overrides
    // -----------------------------------------------------------------------

    fn override_row(group_id: &str, path: &str) -> GroupOverride {
        GroupOverride {
            group_id: group_id.into(),
            path: path.into(),
            label: None,
            parent_id: None,
            merged_into: None,
        }
    }

    fn top_of(tree: &DerivedTree) -> &DerivedGroup {
        tree.groups
            .iter()
            .find(|group| group.path == ["washes", "horizontal", "top"])
            .expect("the split derives")
    }

    #[test]
    fn a_rename_survives_the_rig_growing_and_still_collects() {
        let tree = derive_groups(&horizontal_facts());
        let top = top_of(&tree);
        let mut row = override_row(&top.id, &top.path.join("/"));
        row.label = Some("house left".into());

        // One more par on the high run, after the rename.
        let mut grown = horizontal_facts();
        grown
            .fixtures
            .push(fixture("e", FixtureRole::Wash, "high", [0.0, 0.0, 5.0]));
        let merged = merge_tree(&derive_groups(&grown), std::slice::from_ref(&row), &[]);

        let node = merged
            .iter()
            .find(|node| node.id == top.id)
            .expect("still in the tree");
        assert_eq!(node.label, "house left");
        assert_eq!(node.origin, GroupOrigin::Edited);
        // The rename keeps its namespace, so the name stays unique.
        assert_eq!(node.name, "washes_horizontal_house_left");
        // And the new par filed itself under the name a human gave it.
        assert!(node.fixtures.contains(&"e".to_string()));

        // Its untouched sibling is still plain derivation.
        let bottom = merged
            .iter()
            .find(|node| node.label == "bottom")
            .expect("the sibling derives");
        assert_eq!(bottom.origin, GroupOrigin::Derived);
    }

    #[test]
    fn deleting_the_override_restores_derivation() {
        let tree = derive_groups(&horizontal_facts());
        let top = top_of(&tree);
        let mut row = override_row(&top.id, &top.path.join("/"));
        row.label = Some("renamed".into());
        let renamed = merge_tree(&tree, std::slice::from_ref(&row), &[]);
        assert_eq!(
            renamed.iter().find(|n| n.id == top.id).map(|n| &n.label),
            Some(&"renamed".to_string())
        );

        let plain = merge_tree(&tree, &[], &[]);
        let node = plain.iter().find(|n| n.id == top.id).expect("derives");
        assert_eq!(node.label, "top");
        assert_eq!(node.origin, GroupOrigin::Derived);
    }

    #[test]
    fn a_merged_node_leaves_the_tree_and_its_fixtures_arrive_in_the_target() {
        let tree = derive_groups(&horizontal_facts());
        let top = top_of(&tree);
        let bottom = tree
            .groups
            .iter()
            .find(|group| group.path == ["washes", "horizontal", "bottom"])
            .expect("derives");

        let mut row = override_row(&top.id, &top.path.join("/"));
        row.merged_into = Some(bottom.id.clone());
        let merged = merge_tree(&tree, &[row], &[]);

        assert!(!merged.iter().any(|node| node.id == top.id));
        let target = merged
            .iter()
            .find(|node| node.id == bottom.id)
            .expect("the target stays");
        assert_eq!(target.fixtures, ["c", "d", "a", "b"]);
    }

    #[test]
    fn an_override_whose_path_stopped_deriving_is_inert() {
        // The truss came down: there is no node for the patch to name, so it
        // says nothing rather than inventing a group nobody built.
        let tree = derive_groups(&horizontal_facts());
        let mut row = override_row("gone", "washes/gone/top");
        row.label = Some("the old wing".into());
        let merged = merge_tree(&tree, &[row], &[]);
        assert!(!merged.iter().any(|node| node.id == "gone"));
        assert_eq!(merged.len(), tree.groups.len());
    }

    #[test]
    fn moving_the_structure_moves_the_split_with_no_group_edit() {
        // Lower the high run below the low one and the two halves swap, with
        // nothing touched but the rig.
        let before = derive_groups(&horizontal_facts());
        let top_before = top_of(&before).members.clone();

        let mut moved = horizontal_facts();
        for fact in &mut moved.fixtures {
            if let Some(placement) = fact.placement.as_mut() {
                if placement.parent == "high" {
                    placement.position[2] = 0.0;
                }
            }
        }
        let after = derive_groups(&moved);
        assert_eq!(top_before, ["a", "b"]);
        assert_eq!(top_of(&after).members, ["c", "d"]);
    }
}

/// The two canonical group goldens.
///
/// Both are **seeded venues built through the graph API** — real nodes, real
/// sockets, a real solve — rather than hand-written positions, so a change to
/// the resolver that moves a fixture across a split shows up here as a group
/// moving rather than as nothing at all.
///
/// Each rewrites its golden and then fails if it changed, so a stale capture
/// cannot be committed silently.
#[cfg(test)]
mod goldens {
    use std::path::{Path, PathBuf};

    use luma_render::catalog::{VenueSockets, FIXTURE_CLAMP_SOCKET};
    use luma_scene::venue::{resolve, Edge, Node, NodeKind, Params, VenueGraph, FLOOR_SOCKET};
    use serde_json::{json, Value};

    use super::{derive_groups, facts_from, FixtureIdentity, FixtureRole};

    /// The one deck in the catalog every seeded venue is built out of: it has a
    /// `bottom` that sits on the floor and a `top` a fixture can clamp to,
    /// which is the whole of what these goldens need from geometry.
    const DECK: &str = "stage_lab/stage_praticavel_1x1.glb";

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    fn catalog() -> &'static VenueSockets {
        static SOCKETS: std::sync::OnceLock<VenueSockets> = std::sync::OnceLock::new();
        SOCKETS.get_or_init(|| {
            VenueSockets::load(repo_root().join("resources/meshes"))
                .expect("the catalog resolves against the shipped meshes")
        })
    }

    struct Venue {
        graph: VenueGraph,
        fixtures: Vec<FixtureIdentity>,
    }

    impl Venue {
        fn new() -> Self {
            Venue {
                graph: VenueGraph::new(Node {
                    id: "venue".into(),
                    kind: NodeKind::Venue,
                    catalog_ref: None,
                    label: Some("Golden room".into()),
                    params: Params::default(),
                }),
                fixtures: Vec::new(),
            }
        }

        fn params(at: &[(&str, f64)]) -> Params {
            let mut params = Params::default();
            for (key, value) in at {
                params.set(*key, *value);
            }
            params
        }

        /// A piece resting on a host surface at `(u, v, trim)`.
        fn piece(&mut self, id: &str, kind: NodeKind, host: (&str, &str), at: &[(&str, f64)]) {
            self.graph.insert(Node {
                id: id.into(),
                kind,
                catalog_ref: Some(DECK.into()),
                label: None,
                params: Self::params(at),
            });
            self.attach(id, "bottom", host);
        }

        /// A patched fixture clamped to a host surface at `(u, v, trim)`.
        fn fixture(
            &mut self,
            id: &str,
            model: &str,
            role: FixtureRole,
            host: (&str, &str),
            at: &[(&str, f64)],
        ) {
            self.graph.insert(Node {
                id: id.into(),
                kind: NodeKind::Fixture,
                catalog_ref: Some(id.into()),
                label: None,
                params: Self::params(at),
            });
            self.attach(id, FIXTURE_CLAMP_SOCKET, host);
            self.fixtures.push(FixtureIdentity {
                id: id.into(),
                model: model.into(),
                role,
            });
        }

        fn attach(&mut self, id: &str, my_socket: &str, (parent, their_socket): (&str, &str)) {
            self.graph
                .attach(
                    id,
                    Edge {
                        parent: parent.into(),
                        my_socket: my_socket.into(),
                        their_socket: their_socket.into(),
                        roll: 0.0,
                    },
                    catalog(),
                )
                .unwrap_or_else(|e| panic!("{id}: {e}"));
        }

        fn tree(&self) -> (super::VenueFacts, super::DerivedTree) {
            let solved = resolve(&self.graph, catalog());
            let facts = facts_from("golden", &solved, &self.graph, &self.fixtures);
            let tree = derive_groups(&facts);
            (facts, tree)
        }

        fn capture(&self) -> Value {
            let (facts, tree) = self.tree();
            let groups: Vec<Value> = tree
                .groups
                .iter()
                .map(|group| {
                    json!({
                        "path": group.path,
                        "name": group.name,
                        "role": group.role.as_str(),
                        // Members as their derived labels: the golden pins the
                        // naming rule and the set in one readable line.
                        "members": group.members.iter()
                            .map(|id| tree.fixture_labels[id].clone())
                            .collect::<Vec<_>>(),
                    })
                })
                .collect();
            json!({ "stageCentreX": facts.stage_centre_x, "groups": groups })
        }

        fn paths(&self) -> Vec<String> {
            self.tree()
                .1
                .groups
                .iter()
                .map(|group| group.path.join("/"))
                .collect()
        }
    }

    /// (a) LED bars around a wall: two horizontal runs at different heights and
    /// two vertical ones down the sides. The horizontal row splits by height,
    /// the vertical row by side, and neither cross-cut is emitted because each
    /// draws from exactly one row.
    ///
    /// `mirror` is `+1.0` or `-1.0` — the same rig reflected about stage
    /// centre. A mirrored venue must derive the same tree, because the rule
    /// reads resolved positions and never authoring order.
    fn led_bars_wall(mirror: f64) -> Venue {
        let mut venue = Venue::new();
        // Four wall segments, each standing in for one run of the loop. They
        // are `piece`, not `stage`: nothing here is a deck anyone stands on, and
        // a stage would move the centre `left`/`right` are measured against.
        for (id, u) in [
            ("wall_high", 0.0),
            ("wall_low", 0.0),
            ("wall_left", -5.0),
            ("wall_right", 5.0),
        ] {
            venue.piece(
                id,
                NodeKind::Piece,
                ("venue", FLOOR_SOCKET),
                &[("u", u * mirror), ("v", 4.0)],
            );
        }

        let mut n = 0;
        let mut bar = |venue: &mut Venue, host: &str, u: f64, trim: f64| {
            n += 1;
            venue.fixture(
                &format!("bar_{n}"),
                "LED Bar",
                FixtureRole::Pixel,
                (host, "top"),
                &[("u", u * mirror), ("v", 0.0), ("trim", trim)],
            );
        };
        for i in 0..6 {
            bar(&mut venue, "wall_high", -5.0 + f64::from(i) * 2.0, 5.0);
        }
        for i in 0..6 {
            bar(&mut venue, "wall_low", -5.0 + f64::from(i) * 2.0, 0.5);
        }
        for side in ["wall_left", "wall_right"] {
            for i in 0..4 {
                bar(&mut venue, side, 0.0, 1.5 + f64::from(i) * 1.2);
            }
        }
        venue
    }

    /// (b) Spot wings: two towers bolted to the stage, four spots up each. Both
    /// wings split by height — the split across them is side, and a wing is all
    /// on one side by definition — so `top` and `bottom` are cross-cuts drawn
    /// from two rows, which is exactly when a cross-cut earns its name.
    fn spot_wings(mirror: f64) -> Venue {
        let mut venue = Venue::new();
        venue.piece(
            "stage",
            NodeKind::Stage,
            ("venue", FLOOR_SOCKET),
            &[("u", 0.0), ("v", 0.0)],
        );
        // `u` runs stage *left*-positive on a deck's `top` socket and stage
        // right-positive on the venue floor — the two surfaces disagree about
        // the sign of their tangent. Nothing in derivation depends on it (the
        // split reads resolved x), but it is why these two numbers look
        // backwards; see the report's smells.
        for (id, u) in [("wing_left", 4.0), ("wing_right", -4.0)] {
            venue.piece(
                id,
                NodeKind::Piece,
                ("stage", "top"),
                &[("u", u * mirror), ("v", 0.0)],
            );
        }
        let mut n = 0;
        for wing in ["wing_left", "wing_right"] {
            for i in 0..4 {
                n += 1;
                venue.fixture(
                    &format!("spot_{n}"),
                    "Mover",
                    FixtureRole::Spot,
                    (wing, "top"),
                    &[("u", 0.0), ("v", 0.0), ("trim", 2.0 + f64::from(i) * 1.5)],
                );
            }
        }
        venue
    }

    /// The canonical trees, in words, so a diff in the JSON reads as a rule
    /// that broke rather than as numbers that moved.
    const LED_BARS_WALL: [&str; 7] = [
        "led bars",
        "led bars/horizontal",
        "led bars/horizontal/top",
        "led bars/horizontal/bottom",
        "led bars/vertical",
        "led bars/vertical/left",
        "led bars/vertical/right",
    ];

    const SPOT_WINGS: [&str; 9] = [
        "spots",
        "spots/left wing",
        "spots/left wing/top",
        "spots/left wing/bottom",
        "spots/right wing",
        "spots/right wing/top",
        "spots/right wing/bottom",
        "spots/top",
        "spots/bottom",
    ];

    #[test]
    fn the_venue_groups_golden_is_current() {
        let path = repo_root().join("harness/goldens/venue-groups.json");
        let mut contents = serde_json::to_string_pretty(&json!({
            "ledBarsWall": led_bars_wall(1.0).capture(),
            "spotWings": spot_wings(1.0).capture(),
        }))
        .expect("the capture serializes");
        contents.push('\n');

        let same = std::fs::read_to_string(&path).is_ok_and(|old| old == contents);
        if !same {
            std::fs::write(&path, &contents).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        }
        assert!(
            same,
            "the venue-groups golden was stale and has been rewritten — review and commit it"
        );
    }

    #[test]
    fn the_goldens_produce_the_canonical_trees() {
        assert_eq!(led_bars_wall(1.0).paths(), LED_BARS_WALL);
        assert_eq!(spot_wings(1.0).paths(), SPOT_WINGS);
    }

    /// Reflect both rigs about stage centre and the trees are the same, node
    /// for node: `left` and `right` are read off resolved positions, so the
    /// wing built first is still named for the side it ends up on.
    ///
    /// Compared as a set of paths rather than a sequence, because sibling rows
    /// appear in the order their first fixture was patched — and mirroring is
    /// precisely a change to which side got built first.
    #[test]
    fn a_mirrored_rig_derives_the_same_tree() {
        let sorted = |mut paths: Vec<String>| {
            paths.sort();
            paths
        };
        let want = |canonical: &[&str]| sorted(canonical.iter().map(|p| (*p).into()).collect());
        assert_eq!(sorted(led_bars_wall(-1.0).paths()), want(&LED_BARS_WALL));
        assert_eq!(sorted(spot_wings(-1.0).paths()), want(&SPOT_WINGS));
    }
}
