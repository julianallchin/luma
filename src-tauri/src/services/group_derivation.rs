//! Groups, derived.
//!
//! A group is not a bag someone filled in; it is a **set the rig already
//! describes**, shown as a tree. The rule below is the only one — role from the
//! fixture definition, class from where a run sits, one row per distribution,
//! and inside a row the halves its fixtures measurably fall into — and it is
//! deterministic: the same venue derives the same tree, with the same ids,
//! every time.
//!
//! Manual work sits *on top* as overrides ([`crate::database::local::
//! group_overrides`]). A node with an override is never re-derived; deleting the
//! override restores derivation. That is the whole edit model.
//!
//! # The tree
//!
//! ```text
//! <role>                     every fixture of that role
//!   <class>                  where its runs sit: a wing, or which way it runs
//!     <row>                  one distribution — one structure piece
//!       <split>              the halves that row's fixtures fall into
//!   top | bottom             a cross-cut: one split name, across the role
//!   left | right
//!   downstage | upstage
//! ```
//!
//! # The rule, in full
//!
//! **Role** — [`FixtureRole::of`], from the QLC+ `Type` plus the mode's channel
//! set. One table, documented there, no name heuristics.
//!
//! **Class** — where a run sits, in the words a human uses for it. A run bolted
//! to a stage is a **wing**, `left wing` or `right wing` by which side of the
//! stage's centre it is *attached* to. Anything else is named for the way it
//! runs: `horizontal` (spread along the stage or into it) or `vertical`
//! (spread up). Unplaced fixtures are their own class, `unplaced`.
//!
//! **Row** — one per distribution: one structure piece, one row, never merged
//! with the piece beside it. Two towers on the left are two rows of one
//! `left wing`, because they are two things a human points at separately. A
//! row is named by the piece's authored label when it has one; otherwise a
//! class of exactly two rows names them for the axis they **measurably**
//! separate on ([`separation`]) and anything else numbers them `row 1`…`row n`.
//! A rank is not a name: two towers side by side at one trim are not "top" and
//! "bottom", they are `left` and `right`, and if nothing separates them they
//! are `row 1` and `row 2`.
//!
//! **Split** — a row cut in two where its fixtures measurably separate: `top`
//! and `bottom` by height, `left` and `right` across, `downstage` and
//! `upstage` into the stage. One cut per row, along the axis whose gap is
//! widest, and none at all when the row is an evenly spaced run — six bars a
//! metre apart are one distribution, not two halves. A class holding **one**
//! unlabelled row emits no row node, because the class node already is that
//! set: its children are that row's splits.
//!
//! **Name** — the path in snake_case, and *distinct*: two paths can spell one
//! name (`Truss 1` and `Truss-1` are both `truss_1`), and a name is what an
//! expression selects by, so the second claimant is suffixed — see
//! [`make_distinct`].
//!
//! **Cross-cuts** — under a role, a positional name unioned across the whole
//! role: every `top` on every wing, in one set. A cross-cut gathers the
//! *leaves* that carry the name — a row that split contributes its halves
//! rather than itself — and is emitted only when **two or more** of them do.
//! One contributor would be that set under a second name, which is the thing
//! this design refuses.
//!
//! # What derivation reads, and what it does not
//!
//! Positions are read in data space, resolved. A stage's own yaw is therefore
//! *not* consulted: `left`, `across` and `into the stage` are the room's axes,
//! not the deck's, because a rig hung around a deck turned 30° is still hung
//! left and right of the room. A stage rotated a quarter turn would want the
//! other reading, and does not get it — recorded here rather than half-solved.
//!
//! A structure attached exactly *at* the stage centre is a right wing. The tie
//! has to go somewhere and a strict `<` is what puts it there; nothing in a rig
//! sits on the line by accident, and a piece that does is one nudge from either
//! answer whatever this rule says.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use glam::{DMat4, DVec3};
use luma_scene::coords::data_pose_of_d;
use luma_scene::venue::{NodeKind, NodeSockets, ResolvedVenue, VenueGraph};

use crate::database::local::group_overrides::GroupOverride;
use crate::models::fixtures::{ChannelType, FixtureDefinition, Mode};
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
    /// | has a `Pan` or `Tilt` channel, and a `Gobo` channel | [`Spot`](FixtureRole::Spot) |
    /// | has a `Pan` or `Tilt` channel, and a `Prism` channel | [`Beam`](FixtureRole::Beam) |
    /// | has a `Pan` or `Tilt` channel | [`Wash`](FixtureRole::Wash) |
    /// | a physical layout of more than one cell, `LED Bar (Pixels)`, or a mode with more than two heads | [`Pixel`](FixtureRole::Pixel) |
    /// | has any colour — a `Colour` channel, or one that [carries a colour](crate::models::fixtures::Channel::carries_colour) | [`Wash`](FixtureRole::Wash) |
    /// | has an `Intensity` channel | [`Blinder`](FixtureRole::Blinder) |
    /// | otherwise | [`Other`](FixtureRole::Other) |
    ///
    /// Only channels the *mode* patches are read: a definition's 30-channel
    /// extended mode does not make a fixture patched in 4-channel dimmer mode a
    /// spot.
    ///
    /// **Movement outranks cells.** A moving head is a moving head whether or
    /// not its mode addresses four LED rings, and a wash mover in a 56-channel
    /// mode has plenty of heads; reading the cells first called sixty of the
    /// shipped movers pixel bars.
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

        let patched = Patched::of(definition, mode);
        if patched.moves {
            return if patched.gobo {
                FixtureRole::Spot
            } else if patched.prism {
                FixtureRole::Beam
            } else {
                FixtureRole::Wash
            };
        }

        let cells = definition
            .physical
            .as_ref()
            .and_then(|p| p.layout.as_ref())
            .map_or(1, |l| u64::from(l.width) * u64::from(l.height));
        if type_ == "led bar (pixels)" || cells > 1 || mode.heads.len() > 2 {
            return FixtureRole::Pixel;
        }

        match (patched.colour, patched.intensity) {
            (true, _) => FixtureRole::Wash,
            (false, true) => FixtureRole::Blinder,
            (false, false) => FixtureRole::Other,
        }
    }
}

/// Whether a fixture *aims*: a pan or a tilt channel in the mode it is patched
/// in.
///
/// Beside [`FixtureRole`] rather than inside it because it is a different
/// question and the answers cross: a wash mover and a par are both
/// [`FixtureRole::Wash`], and only one of them has anything to point.
#[must_use]
pub fn aims(definition: &FixtureDefinition, mode: &Mode) -> bool {
    Patched::of(definition, mode).moves
}

/// What the channels a mode actually patches add up to. The one pass over them;
/// everything the definition can tell a classifier is read here.
struct Patched {
    moves: bool,
    gobo: bool,
    prism: bool,
    colour: bool,
    intensity: bool,
}

impl Patched {
    fn of(definition: &FixtureDefinition, mode: &Mode) -> Self {
        let mut patched = Patched {
            moves: false,
            gobo: false,
            prism: false,
            colour: false,
            intensity: false,
        };
        for in_mode in &mode.channels {
            let Some(channel) = definition.channels.iter().find(|c| c.name == in_mode.name) else {
                continue;
            };
            match channel.get_type() {
                ChannelType::Pan | ChannelType::Tilt => patched.moves = true,
                ChannelType::Gobo => patched.gobo = true,
                ChannelType::Prism => patched.prism = true,
                ChannelType::Colour => patched.colour = true,
                ChannelType::Intensity => {
                    if channel.carries_colour() {
                        patched.colour = true;
                    } else {
                        patched.intensity = true;
                    }
                }
                _ => {}
            }
        }
        patched
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
    /// role and a class (`unplaced`); with no position there is nothing to
    /// measure, so they get no row name and no split.
    pub placement: Option<FixturePlacement>,
}

/// One structure node fixtures hang on — one row of the tree.
#[derive(Debug, Clone)]
pub struct StructureFact {
    /// The venue-graph node id, as a placement names it.
    pub node: String,
    /// Whether this node *hangs off* a stage. A run bolted to the deck is a
    /// wing; the deck itself is not, and one hanging off the house structure is
    /// not either.
    pub on_stage: bool,
    /// The node's own label, which names its row when it has one.
    pub label: Option<String>,
    /// Where the run is bolted, data space. A wing's side is read off *this*
    /// and not off its fixtures: which side of the stage a tower stands on is a
    /// fact about the tower.
    pub position: [f64; 3],
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
    /// Stage x that `left` and `right` are measured against — the middle of the
    /// deck's surface, not the origin of its mesh.
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
    /// Unique within the tree — two paths that spell one name are separated by
    /// [`make_distinct`], because the path is not enough on its own.
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

/// The three axes a set of fixtures can separate along, and what a human calls
/// each end of one.
///
/// Closed, and in the order a tie is broken: how high first, then which side,
/// then how far back — which is the order the questions get asked of a rig.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spread {
    /// Up: `top` and `bottom`.
    Height,
    /// Across the room: `left` and `right`.
    Across,
    /// Into the room: `downstage` and `upstage`.
    Depth,
}

impl Spread {
    const ALL: [Spread; 3] = [Spread::Height, Spread::Across, Spread::Depth];

    /// The data-space coordinate this axis reads. Data space is `+X` stage
    /// right, `+Y` upstage, `+Z` up.
    fn axis(self) -> usize {
        match self {
            Spread::Across => 0,
            Spread::Depth => 1,
            Spread::Height => 2,
        }
    }

    /// The two ends, in the order the tree lists them.
    fn ends(self) -> [&'static str; 2] {
        match self {
            Spread::Height => ["top", "bottom"],
            Spread::Across => ["left", "right"],
            Spread::Depth => ["downstage", "upstage"],
        }
    }

    /// Whether the end named first is the *greater* coordinate. Only height
    /// reads downwards: `top` is the high one, while `left` and `downstage` are
    /// the low ones.
    fn first_is_greater(self) -> bool {
        matches!(self, Spread::Height)
    }
}

/// Every end name, in the order cross-cuts are listed under a role.
const ENDS: [&str; 6] = ["top", "bottom", "left", "right", "downstage", "upstage"];

/// Below this, in metres, nothing in a rig is two things.
///
/// It is the floor under the in-cluster spacing rather than a rule beside it,
/// so [`separation`] stays one predicate: a pair of fixtures with no spacing to
/// compare against still has to clear half a metre to be a top and a bottom.
const MIN_SEPARATION: f64 = 0.5;

/// How much wider than the spacing around it a gap must be to be a *gap*.
///
/// One and a half, not one: an evenly spaced run measures a little unevenly
/// once it has been through a solve, and a rule that split on the widest of six
/// equal gaps would cut every truss in the world in half.
const SPACING_RATIO: f64 = 1.5;

/// Where a set of points measurably falls into two clusters, and along which
/// axis — the one measurement the whole rule is built on.
///
/// One predicate, used at both levels: it names a class's two rows and it cuts
/// a row into halves. Sort the coordinates; the widest gap between neighbours
/// is the candidate cut, and it counts when it is more than
/// [`SPACING_RATIO`] times the median of the *other* gaps — the spacing inside
/// the clusters — and never below [`MIN_SEPARATION`]. Evenly spaced points have
/// no gap that beats their own spacing and so never separate.
///
/// Returns the axis and the coordinate the cut sits at: everything above it is
/// the far cluster. The strongest axis wins, measured in metres and therefore
/// comparable across the three; ties go to [`Spread::ALL`]'s order.
fn separation(points: &[[f64; 3]]) -> Option<(Spread, f64)> {
    Spread::ALL
        .into_iter()
        .filter_map(|spread| {
            let values: Vec<f64> = points.iter().map(|point| point[spread.axis()]).collect();
            widest_gap(&values).map(|(at, gap)| (spread, at, gap))
        })
        .reduce(|best, next| if next.2 > best.2 { next } else { best })
        .map(|(spread, at, _)| (spread, at))
}

/// The midpoint of `values`' widest neighbour gap, and its width — when it is
/// wide enough to mean two clusters. See [`separation`] for the predicate.
fn widest_gap(values: &[f64]) -> Option<(f64, f64)> {
    if values.len() < 2 {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let gaps: Vec<f64> = sorted.windows(2).map(|pair| pair[1] - pair[0]).collect();
    let (index, gap) =
        gaps.iter()
            .copied()
            .enumerate()
            .reduce(|best, next| if next.1 > best.1 { next } else { best })?;
    let mut others = gaps.clone();
    others.remove(index);
    if gap > (median(&mut others) * SPACING_RATIO).max(MIN_SEPARATION) {
        Some(((sorted[index] + sorted[index + 1]) / 2.0, gap))
    } else {
        None
    }
}

/// The median of `values`, `0.0` for an empty slice — which is what makes
/// [`MIN_SEPARATION`] the whole test for a pair of points with no spacing of
/// their own to compare against.
fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

/// Which side of the stage a wing is bolted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wing {
    Left,
    Right,
}

/// The two ways a free-standing run can lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lie {
    /// Up: a tower.
    Vertical,
    /// Along the room or into it: a truss, a wall, a floor run.
    Horizontal,
}

/// Where a run sits — the tree's second level, and the thing rows are named
/// within.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Bolted to a stage, on one side of its centre.
    Wing(Wing),
    /// Free-standing, named for the way it runs.
    Run(Lie),
    /// Not in the room at all.
    Unplaced,
}

impl Class {
    fn name(self) -> &'static str {
        match self {
            Class::Wing(Wing::Left) => "left wing",
            Class::Wing(Wing::Right) => "right wing",
            Class::Run(Lie::Horizontal) => "horizontal",
            Class::Run(Lie::Vertical) => "vertical",
            Class::Unplaced => UNPLACED,
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

        // One row per structure, in the order their first fixture was patched.
        // Rows are never merged: two trusses are two things, whatever they are
        // called.
        let mut rows: Vec<Row<'_>> = Vec::new();
        for fixture in &members {
            let structure = fixture
                .placement
                .as_ref()
                .map(|placement| placement.parent.clone());
            let index = match rows.iter().position(|row| row.structure == structure) {
                Some(index) => index,
                None => {
                    rows.push(Row {
                        structure,
                        members: Vec::new(),
                    });
                    rows.len() - 1
                }
            };
            rows[index].members.push(fixture);
        }

        // Classes, also in the order their first row was patched.
        let mut classes: Vec<(Class, Vec<Row<'_>>)> = Vec::new();
        for row in rows {
            let class = class_of(facts, &row);
            match classes.iter_mut().find(|(seen, _)| *seen == class) {
                Some((_, rows)) => rows.push(row),
                None => classes.push((class, vec![row])),
            }
        }

        // `end name -> the leaves that carried it`, for the cross-cuts below.
        let mut cross: BTreeMap<&'static str, (Vec<&FixtureFact>, usize)> = BTreeMap::new();

        for (class, rows) in classes {
            let mut class_path = role_path.clone();
            class_path.push(class.name().to_string());
            let members: Vec<&FixtureFact> = rows
                .iter()
                .flat_map(|row| row.members.iter().copied())
                .collect();
            let class_id = push(
                &mut tree,
                facts,
                Some(&role_id),
                role,
                class_path.clone(),
                &members,
            );

            // One unlabelled row *is* the class; naming it again would be two
            // names for one set. Its splits hang off the class instead.
            if rows.len() == 1 && facts.label_of(&rows[0]).is_none() {
                split_row(
                    &mut tree,
                    facts,
                    &class_id,
                    role,
                    &class_path,
                    &rows[0],
                    &mut cross,
                );
                continue;
            }

            for (name, end, row) in name_rows(facts, rows) {
                let mut path = class_path.clone();
                path.push(name);
                let row_id = push(
                    &mut tree,
                    facts,
                    Some(&class_id),
                    role,
                    path.clone(),
                    &row.members,
                );
                let split = split_row(&mut tree, facts, &row_id, role, &path, &row, &mut cross);
                // A row that split contributes its halves; contributing itself
                // as well would count its fixtures twice under one name.
                if let Some(end) = end.filter(|_| !split) {
                    let entry = cross.entry(end).or_default();
                    entry.0.extend(row.members.iter().copied());
                    entry.1 += 1;
                }
            }
        }

        // A cross-cut drawing from a single leaf *is* that leaf, under a second
        // name. Two names for one set is the thing this whole design is trying
        // not to do, so it is not emitted.
        for end in ENDS {
            let Some((side, contributors)) = cross.get(end) else {
                continue;
            };
            if *contributors < 2 {
                continue;
            }
            let mut path = role_path.clone();
            path.push(end.to_string());
            push(&mut tree, facts, Some(&role_id), role, path, side);
        }
    }

    make_distinct(tree.groups.iter_mut().map(|group| &mut group.name));
    tree
}

/// Name and order a class's rows.
///
/// A labelled piece names its own row, always. Exactly two unlabelled rows are
/// a pair, and a pair is named for the axis it measurably separates on — never
/// for a rank, so two towers at one trim come out `left` and `right` rather
/// than "top" and "bottom". Everything else is a stack, and a stack's rungs are
/// numbered in the order they were built: `row 1` claims a position among
/// siblings and nothing about where it hangs.
///
/// The returned end name is the row's contribution to a cross-cut, `None` for a
/// label or a number — a cross-cut unions rows *named* `top`, not rows that
/// happen to be the higher of two.
fn name_rows<'a>(
    facts: &VenueFacts,
    rows: Vec<Row<'a>>,
) -> Vec<(String, Option<&'static str>, Row<'a>)> {
    let pair = (rows.len() == 2)
        .then(|| separation(&[rows[0].centre(), rows[1].centre()]))
        .flatten();
    let Some((spread, _)) = pair else {
        return rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| match facts.label_of(&row) {
                Some(label) => (label.to_string(), None, row),
                None => (format!("row {}", index + 1), None, row),
            })
            .collect();
    };
    let mut rows = rows;
    let axis = spread.axis();
    rows.sort_by(|a, b| {
        let (a, b) = (a.centre()[axis], b.centre()[axis]);
        let order = a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal);
        if spread.first_is_greater() {
            order.reverse()
        } else {
            order
        }
    });
    rows.into_iter()
        .zip(spread.ends())
        .map(|(row, end)| match facts.label_of(&row) {
            Some(label) => (label.to_string(), None, row),
            None => (end.to_string(), Some(end), row),
        })
        .collect()
}

/// Cut one row along the axis its fixtures measurably separate on, if any, and
/// record the halves as cross-cut contributors. Returns whether it cut.
///
/// `parent`/`parent_path` is the row's node — or the class's, when the class
/// holds one unlabelled row and therefore *is* that row.
fn split_row<'a>(
    tree: &mut DerivedTree,
    facts: &VenueFacts,
    parent: &str,
    role: FixtureRole,
    parent_path: &[String],
    row: &Row<'a>,
    cross: &mut BTreeMap<&'static str, (Vec<&'a FixtureFact>, usize)>,
) -> bool {
    let points: Vec<[f64; 3]> = row
        .members
        .iter()
        .filter_map(|fixture| Some(fixture.placement.as_ref()?.position))
        .collect();
    let Some((spread, at)) = separation(&points) else {
        return false;
    };
    let mut halves: [Vec<&'a FixtureFact>; 2] = [Vec::new(), Vec::new()];
    for fixture in &row.members {
        let Some(placement) = fixture.placement.as_ref() else {
            continue;
        };
        let above = placement.position[spread.axis()] > at;
        halves[usize::from(above != spread.first_is_greater())].push(fixture);
    }
    for (end, members) in spread.ends().into_iter().zip(halves) {
        let mut path = parent_path.to_vec();
        path.push(end.to_string());
        push(tree, facts, Some(parent), role, path, &members);
        let entry = cross.entry(end).or_default();
        entry.0.extend(members);
        entry.1 += 1;
    }
    true
}

/// The class name for fixtures nothing in the room holds.
const UNPLACED: &str = "unplaced";

/// One distribution: the fixtures of one role on one structure.
struct Row<'a> {
    /// The structure they hang on, `None` for the patch tray.
    structure: Option<String>,
    members: Vec<&'a FixtureFact>,
}

impl Row<'_> {
    /// The mean of the row's placed fixtures, `[0, 0, 0]` when none are
    /// placed — which only happens for the unplaced row, and that row is never
    /// measured against a sibling.
    fn centre(&self) -> [f64; 3] {
        let mut sum = [0.0; 3];
        let mut n = 0.0;
        for fixture in &self.members {
            let Some(placement) = fixture.placement.as_ref() else {
                continue;
            };
            for (axis, value) in sum.iter_mut().enumerate() {
                *value += placement.position[axis];
            }
            n += 1.0;
        }
        if n == 0.0 {
            return sum;
        }
        sum.map(|value| value / n)
    }

    /// Which way the row lies: up, or along the room. A run of one fixture has
    /// no direction and reads as horizontal, the class a lone par on a truss
    /// belongs to.
    ///
    /// Height has to beat *both* of the other two, not just the one across the
    /// stage: a run laid into the room, deep rather than wide, is a horizontal
    /// run and calling it a tower because it is wider than it is tall in `x` is
    /// how a floor run came out `vertical`.
    ///
    /// Read off the fixtures rather than the structure's geometry on purpose: a
    /// run's direction is the line the things on it lie along, which is true of
    /// a generated truss, a measured GLB and a wall alike, and needs no catalog.
    fn lie(&self) -> Lie {
        let positions: Vec<[f64; 3]> = self
            .members
            .iter()
            .filter_map(|fixture| Some(fixture.placement.as_ref()?.position))
            .collect();
        let spread = |axis: usize| {
            let (min, max) = positions.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
                (lo.min(p[axis]), hi.max(p[axis]))
            });
            max - min
        };
        if positions.len() > 1 && spread(2) > spread(0) && spread(2) > spread(1) {
            Lie::Vertical
        } else {
            Lie::Horizontal
        }
    }
}

impl VenueFacts {
    fn structure_of(&self, row: &Row<'_>) -> Option<&StructureFact> {
        let node = row.structure.as_deref()?;
        self.structures.iter().find(|s| s.node == node)
    }

    fn label_of(&self, row: &Row<'_>) -> Option<&str> {
        self.structure_of(row)?.label.as_deref()
    }
}

/// Which class a row belongs to. A structure attached exactly at the stage
/// centre is a right wing — see the module header on that tie.
fn class_of(facts: &VenueFacts, row: &Row<'_>) -> Class {
    let Some(structure) = facts.structure_of(row) else {
        return Class::Unplaced;
    };
    if structure.on_stage {
        return Class::Wing(if structure.position[0] < facts.stage_centre_x {
            Wing::Left
        } else {
            Wing::Right
        });
    }
    Class::Run(row.lie())
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

/// Make a run of selection names distinct, in place.
///
/// A name is what an expression selects by, and two nodes can normalize to one
/// name — a piece labelled `Truss 1` and one labelled `Truss-1` are both
/// `truss_1`, and an expression naming it would union two rows into one set
/// without saying so. The first claimant in iteration order keeps the plain
/// name; every later one takes the lowest free `_2`, `_3`, … suffix.
///
/// Every plain name is reserved before any suffix is minted, so a minted
/// `truss_1_2` can never shadow a node that spells `truss_1_2` itself.
/// Deterministic in the order it is handed, which is the tree's own order, so
/// the same rig derives the same names every time.
///
/// Ids are untouched: they are path-based ([`derived_id`]), so a set that
/// ended up suffixed keeps its identity, and relabelling the piece — which
/// changes the path, not the id — is what gives it its own word back.
///
/// An empty name is left empty: it names nothing, and everything downstream
/// skips it. Suffixing it would mint `_2`, a name no expression can spell.
fn make_distinct<'a>(names: impl IntoIterator<Item = &'a mut String>) {
    let names: Vec<&'a mut String> = names.into_iter().collect();
    let mut taken: BTreeSet<String> = names.iter().map(|name| (**name).clone()).collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for name in names {
        if name.is_empty() || seen.insert(name.clone()) {
            continue;
        }
        for n in 2.. {
            let candidate = format!("{name}_{n}");
            if taken.insert(candidate.clone()) {
                *name = candidate;
                break;
            }
        }
    }
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
///
/// The socket supply is here for one reason: a stage's *centre* is a fact about
/// its geometry, and geometry in this codebase is sockets — a deck's origin is a
/// corner of its mesh, and its sockets are what say where its middle is.
#[must_use]
pub fn facts_from<S: NodeSockets + ?Sized>(
    venue_id: &str,
    solved: &ResolvedVenue,
    graph: &VenueGraph,
    sockets: &S,
    fixtures: &[FixtureIdentity],
) -> VenueFacts {
    let mut facts = VenueFacts {
        venue_id: venue_id.to_string(),
        stage_centre_x: stage_centre_x(solved, graph, sockets),
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
                    on_stage: hangs_off_a_stage(graph, &placement.parent),
                    label: pose.and_then(|pose| pose.label.clone()),
                    position: pose.map_or([0.0; 3], |pose| attachment_point(graph, sockets, pose)),
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
///
/// The middle, not the origin. A deck is a measured GLB whose origin is a
/// corner of its mesh, so averaging origins puts "centre" half a deck to one
/// side and files every fixture on the deck under the same wing. A piece's
/// sockets sit on its faces — `bottom` and `top` in the middle of them, the
/// four edges opposite each other — so their centroid *is* the piece's centre,
/// with no bounds accessor to invent and no socket name to hard-code.
fn stage_centre_x<S: NodeSockets + ?Sized>(
    solved: &ResolvedVenue,
    graph: &VenueGraph,
    sockets: &S,
) -> f64 {
    let xs: Vec<f64> = solved
        .poses()
        .filter(|pose| pose.kind == NodeKind::Stage)
        .map(|pose| {
            let local = graph
                .node(&pose.node)
                .map(|node| sockets.sockets(node))
                .unwrap_or_default();
            if local.is_empty() {
                return pose.data_pose().0[0];
            }
            let centroid: DVec3 =
                local.iter().map(|socket| socket.position).sum::<DVec3>() / local.len() as f64;
            data_pose_of_d(pose.world * DMat4::from_translation(centroid)).0[0]
        })
        .collect();
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

/// Where a run is bolted: the world position of the socket it mates with, or
/// its pose origin when the graph does not say. The point a wing's side is read
/// from — which side of the stage a tower stands on is a fact about the joint,
/// not about where its heads happen to point.
fn attachment_point<S: NodeSockets + ?Sized>(
    graph: &VenueGraph,
    sockets: &S,
    pose: &luma_scene::venue::NodePose,
) -> [f64; 3] {
    let local = graph
        .edge(&pose.node)
        .zip(graph.node(&pose.node))
        .and_then(|(edge, node)| {
            sockets
                .sockets(node)
                .into_iter()
                .find(|socket| socket.name == edge.my_socket)
                .map(|socket| socket.position)
        });
    match local {
        Some(local) => data_pose_of_d(pose.world * DMat4::from_translation(local)).0,
        None => pose.data_pose().0,
    }
}

/// Whether `node` *hangs off* a stage — what makes a run a wing rather than
/// house structure.
///
/// The walk starts at the parent, so a stage is not its own wing: fixtures
/// clamped straight to the deck are a run on the deck, and calling that
/// "the right wing" because the deck's origin sits right of its own centre is
/// how a symmetric rig came out lopsided.
fn hangs_off_a_stage(graph: &VenueGraph, node: &str) -> bool {
    let mut at = match graph.edge(node) {
        Some(edge) => edge.parent.clone(),
        None => return false,
    };
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
/// Merge is also **total here**, whatever the rows say. A chain — a merged into
/// b, b into c — lands every fixture in c, because a set that was folded away
/// cannot be somewhere the tree does not show. A cycle folds nothing, because
/// there is no terminal set to fold into. And the children of a node that was
/// merged away re-parent onto its target rather than vanishing with it. The
/// write path refuses to *create* a cycle ([`merged_terminal`] is what it asks);
/// this reader still has to survive one, because rows outlive the code that
/// wrote them.
///
/// Parents come before children, and the authored groups come last — which is
/// what [`make_names_distinct`], the step after this one, reads to decide who
/// keeps a name two nodes spelled the same way.
#[must_use]
pub fn merge_tree(
    tree: &DerivedTree,
    overrides: &[GroupOverride],
    manual: &[ManualGroup],
) -> Vec<GroupTreeNode> {
    let lookup = |id: &str| overrides.iter().find(|row| row.group_id == id);
    // Where each merged node's fixtures actually end up, and what each surviving
    // node therefore absorbs. Read once: a merge is rare and a scan per node
    // would be quadratic in the tree.
    let mut folded: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut absorbed: BTreeMap<&str, &str> = BTreeMap::new();
    for row in overrides {
        if row.merged_into.is_none() {
            continue;
        }
        let Some(target) = merged_terminal(overrides, &row.group_id) else {
            continue;
        };
        folded.entry(target).or_default().push(&row.group_id);
        absorbed.insert(&row.group_id, target);
    }
    let own_members = |id: &str| -> &[String] {
        tree.groups
            .iter()
            .find(|group| group.id == id)
            .map(|group| group.members.as_slice())
            .or_else(|| {
                manual
                    .iter()
                    .find(|group| group.id == id)
                    .map(|group| group.fixtures.as_slice())
            })
            .unwrap_or_default()
    };
    let members_of = |id: &str, own: &[String]| -> Vec<String> {
        let mut members = own.to_vec();
        for source in folded.get(id).into_iter().flatten() {
            for fixture in own_members(source) {
                if !members.contains(fixture) {
                    members.push(fixture.clone());
                }
            }
        }
        members
    };
    // A parent that was merged away is not a place to hang from; the set that
    // absorbed it is.
    let parent_of = |parent: Option<String>| -> Option<String> {
        let parent = parent?;
        Some(
            absorbed
                .get(parent.as_str())
                .map_or(parent, |target| (*target).to_string()),
        )
    };

    let mut nodes = Vec::with_capacity(tree.groups.len() + manual.len());
    for group in &tree.groups {
        let row = lookup(&group.id);
        // Merged away: its fixtures are counted under the target, and showing
        // the husk would count them twice.
        if absorbed.contains_key(group.id.as_str()) {
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
            parent_id: parent_of(match row.and_then(|row| row.parent_id.as_deref()) {
                None => group.parent.clone(),
                Some("") => None,
                Some(parent) => Some(parent.to_string()),
            }),
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
        if absorbed.contains_key(group.id.as_str()) {
            continue;
        }
        let label = row
            .and_then(|row| row.label.clone())
            .unwrap_or_else(|| group.name.clone());
        nodes.push(GroupTreeNode {
            id: group.id.clone(),
            name: normalize_group_name(&label),
            label,
            parent_id: parent_of(
                row.and_then(|row| row.parent_id.as_deref())
                    .filter(|parent| !parent.is_empty())
                    .map(str::to_string),
            ),
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

/// Separate the names of a merged tree that two rules spelled the same way.
///
/// The second half of [`merge_tree`], and a step of its own because the tree
/// *before* it is the one a typed name is checked against: after this there is
/// no collision left to find, which is the whole point of it. A name someone
/// typed is refused ([`super::groups::GroupSources::clash_for`]); a name nobody
/// typed — a piece labelled `Truss-1` beside one labelled `Truss 1` — is
/// suffixed here.
///
/// Authored groups claim first. A score holds those words, and they are the
/// tail of a merged tree, which is what `role: None` marks.
pub fn make_names_distinct(nodes: &mut [GroupTreeNode]) {
    let boundary = nodes
        .iter()
        .position(|node| node.role.is_none())
        .unwrap_or(nodes.len());
    let (derived, authored) = nodes.split_at_mut(boundary);
    make_distinct(
        authored
            .iter_mut()
            .chain(derived.iter_mut())
            .map(|node| &mut node.name),
    );
}

/// Where `group_id`'s fixtures end up once every merge in `overrides` is
/// followed: the first node in the chain that is not itself merged away.
///
/// `None` when `group_id` is not merged at all, and `None` when the chain
/// closes on itself — a cycle has no terminal set, so a cycle folds nothing.
/// The write path uses this to refuse the row that would close one; the reader
/// uses it to stay total when a row already did.
#[must_use]
pub fn merged_terminal<'a>(overrides: &'a [GroupOverride], group_id: &str) -> Option<&'a str> {
    let target = |id: &str| {
        overrides
            .iter()
            .find(|row| row.group_id == id)
            .and_then(|row| row.merged_into.as_deref())
    };
    let mut seen: Vec<&str> = vec![];
    let mut at = target(group_id)?;
    if at == group_id {
        return None;
    }
    while let Some(next) = target(at) {
        if next == group_id || seen.contains(&next) {
            return None;
        }
        seen.push(at);
        at = next;
    }
    Some(at)
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

    /// A structure standing at `x`, which is where its wing side is read from.
    fn structure(node: &str, on_stage: bool, x: f64) -> StructureFact {
        StructureFact {
            node: node.into(),
            on_stage,
            label: None,
            position: [x, 0.0, 0.0],
        }
    }

    /// Two horizontal runs at different heights, three evenly spaced pars on
    /// each: one `horizontal` class, two rows named for the height that
    /// separates them, and no split inside either — an evenly spaced run is one
    /// distribution, not two halves.
    fn horizontal_facts() -> VenueFacts {
        VenueFacts {
            venue_id: "v".into(),
            stage_centre_x: 0.0,
            structures: vec![structure("high", false, 0.0), structure("low", false, 0.0)],
            fixtures: vec![
                fixture("a", FixtureRole::Wash, "high", [-2.0, 0.0, 5.0]),
                fixture("b", FixtureRole::Wash, "high", [0.0, 0.0, 5.0]),
                fixture("c", FixtureRole::Wash, "high", [2.0, 0.0, 5.0]),
                fixture("d", FixtureRole::Wash, "low", [-2.0, 0.0, 1.0]),
                fixture("e", FixtureRole::Wash, "low", [0.0, 0.0, 1.0]),
                fixture("f", FixtureRole::Wash, "low", [2.0, 0.0, 1.0]),
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
    fn a_horizontal_class_names_its_rows_by_height() {
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
    fn a_vertical_class_names_its_rows_by_side() {
        let facts = VenueFacts {
            venue_id: "v".into(),
            stage_centre_x: 0.0,
            structures: vec![
                structure("left", false, -4.0),
                structure("right", false, 4.0),
            ],
            fixtures: vec![
                fixture("a", FixtureRole::Wash, "left", [-4.0, 0.0, 1.0]),
                fixture("b", FixtureRole::Wash, "left", [-4.0, 0.0, 3.0]),
                fixture("c", FixtureRole::Wash, "left", [-4.0, 0.0, 5.0]),
                fixture("d", FixtureRole::Wash, "right", [4.0, 0.0, 1.0]),
                fixture("e", FixtureRole::Wash, "right", [4.0, 0.0, 3.0]),
                fixture("f", FixtureRole::Wash, "right", [4.0, 0.0, 5.0]),
            ],
        };
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

    /// Rows are never merged, however alike two runs look: two trusses at the
    /// same height are two rows, because they are two things — named here for
    /// the depth that separates them, since the height no longer does.
    #[test]
    fn two_runs_of_a_class_are_two_rows() {
        let mut facts = horizontal_facts();
        for fact in &mut facts.fixtures {
            if let Some(placement) = fact.placement.as_mut() {
                if placement.parent == "low" {
                    placement.position[2] = 5.0;
                    placement.position[1] = 4.0;
                }
            }
        }
        assert_eq!(
            paths(&derive_groups(&facts)).len(),
            4,
            "one class, two rows, and no merge by name"
        );
    }

    /// A labelled piece names its own row, whatever its position says — and it
    /// does so with a full run on it, which the old rule only managed for a
    /// structure carrying fewer than two fixtures.
    #[test]
    fn a_labelled_piece_names_its_row() {
        let mut facts = horizontal_facts();
        facts.structures[0].label = Some("front truss".into());
        assert_eq!(
            paths(&derive_groups(&facts)),
            vec![
                "washes",
                "washes/horizontal",
                "washes/horizontal/front truss",
                "washes/horizontal/bottom",
            ]
        );
    }

    /// One unlabelled row is the class under a second name, so the row node is
    /// not emitted at all: the class's children are that row's splits.
    #[test]
    fn a_class_of_one_unlabelled_row_hangs_its_splits_on_the_class() {
        let facts = VenueFacts {
            venue_id: "v".into(),
            stage_centre_x: 0.0,
            structures: vec![structure("tower", true, -4.0)],
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

    /// A wing's side is the *structure's*, not its fixtures'. A tower bolted
    /// stage left whose heads happen to hang past centre is still stage left.
    #[test]
    fn a_wing_takes_its_side_from_the_structure() {
        let facts = VenueFacts {
            venue_id: "v".into(),
            stage_centre_x: 0.0,
            structures: vec![structure("tower", true, -4.0)],
            fixtures: vec![
                fixture("a", FixtureRole::Spot, "tower", [3.0, 0.0, 1.0]),
                fixture("b", FixtureRole::Spot, "tower", [3.0, 0.0, 5.0]),
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

    /// More than two rows is a stack, and a stack's rungs are numbered.
    #[test]
    fn three_rows_are_numbered_not_halved() {
        let mut facts = VenueFacts {
            venue_id: "v".into(),
            stage_centre_x: 0.0,
            structures: Vec::new(),
            fixtures: Vec::new(),
        };
        for (n, z) in [(0, 1.0), (1, 3.0), (2, 5.0)] {
            let node = format!("run{n}");
            facts.structures.push(structure(&node, false, 0.0));
            facts.fixtures.push(fixture(
                &format!("a{n}"),
                FixtureRole::Wash,
                &node,
                [0.0, 0.0, z],
            ));
        }
        assert_eq!(
            paths(&derive_groups(&facts)),
            vec![
                "washes",
                "washes/horizontal",
                "washes/horizontal/row 1",
                "washes/horizontal/row 2",
                "washes/horizontal/row 3",
            ]
        );
    }

    /// Facts with nothing in them yet, for the tests that build their own rig.
    fn empty() -> VenueFacts {
        VenueFacts {
            venue_id: "v".into(),
            stage_centre_x: 0.0,
            structures: Vec::new(),
            fixtures: Vec::new(),
        }
    }

    /// One structure and the spots on it. `at` is the structure's *attachment*
    /// x, which is all a wing's side is read from; the positions carry
    /// everything else.
    fn run(facts: &mut VenueFacts, node: &str, on_stage: bool, at: f64, spots: &[[f64; 3]]) {
        facts.structures.push(structure(node, on_stage, at));
        for (n, position) in spots.iter().enumerate() {
            facts.fixtures.push(fixture(
                &format!("{node}_{n}"),
                FixtureRole::Spot,
                node,
                *position,
            ));
        }
    }

    /// Three evenly spaced spots up a tower standing at `x`, `y`.
    fn tower(x: f64, y: f64) -> [[f64; 3]; 3] {
        [[x, y, 1.0], [x, y, 3.0], [x, y, 5.0]]
    }

    /// Rank is not a name. Two towers standing side by side at one trim are not
    /// a top and a bottom — what separates them is which side of the other each
    /// stands on, and that is what they are called.
    #[test]
    fn rows_at_one_height_are_named_across_not_ranked() {
        let mut facts = empty();
        run(&mut facts, "inner", true, -4.0, &tower(-1.0, 0.0));
        run(&mut facts, "outer", true, -4.0, &tower(-4.0, 0.0));
        assert_eq!(
            paths(&derive_groups(&facts)),
            vec![
                "spots",
                "spots/left wing",
                "spots/left wing/left",
                "spots/left wing/right",
            ]
        );
    }

    /// Two towers one behind the other are named for the depth that separates
    /// them, in the room's words rather than the rig's build order.
    #[test]
    fn rows_that_separate_in_depth_are_downstage_and_upstage() {
        let mut facts = empty();
        run(&mut facts, "back", true, -4.0, &tower(-4.0, 3.0));
        run(&mut facts, "front", true, -4.0, &tower(-4.0, 0.0));
        assert_eq!(
            paths(&derive_groups(&facts)),
            vec![
                "spots",
                "spots/left wing",
                "spots/left wing/downstage",
                "spots/left wing/upstage",
            ]
        );
    }

    /// Nothing separates them, so nothing is claimed about where they hang:
    /// they are numbered in the order they were built.
    #[test]
    fn rows_that_separate_on_no_axis_are_numbered() {
        let mut facts = empty();
        run(&mut facts, "one", true, -4.0, &tower(-4.0, 0.0));
        run(&mut facts, "two", true, -4.0, &tower(-4.3, 0.2));
        assert_eq!(
            paths(&derive_groups(&facts)),
            vec![
                "spots",
                "spots/left wing",
                "spots/left wing/row 1",
                "spots/left wing/row 2",
            ]
        );
    }

    /// Both sides of the half-metre floor. A pair of fixtures has no spacing of
    /// its own to be measured against, so [`MIN_SEPARATION`] is the whole test:
    /// under it they are one distribution, over it they are two halves.
    #[test]
    fn a_split_needs_half_a_metre() {
        let cut = |apart: f64| {
            let mut facts = empty();
            run(
                &mut facts,
                "tower",
                true,
                -4.0,
                &[[-4.0, 0.0, 1.0], [-4.0, 0.0, 1.0 + apart]],
            );
            paths(&derive_groups(&facts))
        };
        assert_eq!(cut(0.4), vec!["spots", "spots/left wing"]);
        assert_eq!(
            cut(0.6),
            vec![
                "spots",
                "spots/left wing",
                "spots/left wing/top",
                "spots/left wing/bottom",
            ]
        );
    }

    /// Both sides of the spacing ratio. Four pars a metre apart are one run;
    /// move the last one out and the hole in the middle is what makes two.
    #[test]
    fn an_evenly_spaced_run_is_not_two_halves() {
        let cut = |last: f64| {
            let mut facts = empty();
            run(
                &mut facts,
                "truss",
                false,
                0.0,
                &[
                    [0.0, 0.0, 4.0],
                    [1.0, 0.0, 4.0],
                    [2.0, 0.0, 4.0],
                    [last, 0.0, 4.0],
                ],
            );
            paths(&derive_groups(&facts))
        };
        assert_eq!(cut(3.0), vec!["spots", "spots/horizontal"]);
        assert_eq!(
            cut(4.0),
            vec![
                "spots",
                "spots/horizontal",
                "spots/horizontal/left",
                "spots/horizontal/right",
            ]
        );
    }

    /// A cross-cut gathers leaves, and the leaves under a collapsed class are
    /// its splits: two wings, one tower each, two spots up each, and `top`
    /// means something no single wing says.
    #[test]
    fn a_cross_cut_gathers_the_halves_of_a_row_that_split() {
        let mut facts = empty();
        for (node, x) in [("left_tower", -4.0), ("right_tower", 4.0)] {
            run(&mut facts, node, true, x, &[[x, 0.0, 2.0], [x, 0.0, 4.0]]);
        }
        let tree = derive_groups(&facts);
        let top = tree
            .groups
            .iter()
            .find(|group| group.path == ["spots", "top"])
            .expect("both wings carry a top");
        assert_eq!(top.members, ["left_tower_1", "right_tower_1"]);
    }

    /// A row that split contributes its halves and not itself: counting both
    /// would file its fixtures under one name twice.
    #[test]
    fn a_row_that_split_does_not_also_cross_cut_itself() {
        let mut facts = empty();
        for (side, x) in [("left", -4.0), ("right", 4.0)] {
            for (depth, y) in [("front", 0.0), ("back", 3.0)] {
                run(
                    &mut facts,
                    &format!("{side}_{depth}"),
                    true,
                    x,
                    &[[x, y, 2.0], [x, y, 4.0]],
                );
            }
        }
        let paths = paths(&derive_groups(&facts));
        assert!(paths.contains(&"spots/left wing/downstage/top".to_string()));
        assert!(paths.contains(&"spots/top".to_string()));
        assert!(
            !paths.contains(&"spots/downstage".to_string()),
            "a row that split cross-cut itself as well as its halves: {paths:?}"
        );
    }

    #[test]
    fn a_cross_cut_needs_two_classes() {
        // One class's `top` is that class's child; naming it twice is what the
        // cross-cut rule refuses.
        let one = derive_groups(&horizontal_facts());
        assert!(!paths(&one).contains(&"washes/top".to_string()));

        // Two wings, two rows up each — now `top` means something no single
        // class says.
        let mut facts = VenueFacts {
            venue_id: "v".into(),
            stage_centre_x: 0.0,
            structures: Vec::new(),
            fixtures: Vec::new(),
        };
        for (side, x) in [("l", -4.0), ("r", 4.0)] {
            for (n, z) in [(0, 1.0), (1, 5.0)] {
                let node = format!("{side}{n}");
                facts.structures.push(structure(&node, true, x));
                facts.fixtures.push(fixture(
                    &format!("{side}{n}"),
                    FixtureRole::Spot,
                    &node,
                    [x, 0.0, z],
                ));
            }
        }
        let paths = paths(&derive_groups(&facts));
        assert!(paths.contains(&"spots/top".to_string()));
        assert!(paths.contains(&"spots/bottom".to_string()));
        // No class named its rows by side, so there is no side cross-cut.
        assert!(!paths.contains(&"spots/left".to_string()));
    }

    #[test]
    fn unplaced_fixtures_get_a_class_and_no_rows() {
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
        assert_eq!(tree.fixture_labels["f"], "Bar 5");
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
        // One more par on the high run, in step with the others so the run is
        // still one evenly spaced distribution: the sets change, the paths do
        // not.
        facts
            .fixtures
            .push(fixture("g", FixtureRole::Wash, "high", [4.0, 0.0, 5.0]));
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
            .is_some_and(|g| g.members.len() == 4));
    }

    #[test]
    fn ids_are_namespaced_by_venue() {
        let path = vec!["washes".to_string()];
        assert_ne!(derived_id("a", &path), derived_id("b", &path));
    }

    // -----------------------------------------------------------------------
    // Names
    // -----------------------------------------------------------------------

    fn names(tree: &DerivedTree) -> Vec<String> {
        tree.groups.iter().map(|group| group.name.clone()).collect()
    }

    /// The two labels a class carries, in patch order.
    fn labelled(first: &str, second: &str) -> VenueFacts {
        let mut facts = horizontal_facts();
        facts.structures[0].label = Some(first.into());
        facts.structures[1].label = Some(second.into());
        facts
    }

    /// Defect: a name was minted from the path and never checked against the
    /// rest of the tree, so a piece labelled `Truss 1` beside one labelled
    /// `Truss-1` derived one `washes_horizontal_truss_1` twice — and an
    /// expression naming it selected both runs as one set.
    #[test]
    fn two_labels_that_spell_one_name_do_not_share_it() {
        let tree = derive_groups(&labelled("Truss 1", "Truss-1"));
        assert_eq!(
            names(&tree),
            [
                "washes",
                "washes_horizontal",
                "washes_horizontal_truss_1",
                "washes_horizontal_truss_1_2",
            ]
        );
        for group in &tree.groups[2..] {
            assert_eq!(group.members.len(), 3, "{} is not one run", group.name);
        }
    }

    /// The suffix is minted around the names already spoken for, not on top of
    /// them: a third piece *labelled* `Truss 1 2` keeps that name and the
    /// duplicate goes to `_3`.
    #[test]
    fn a_minted_suffix_never_shadows_a_name_the_rig_spells_itself() {
        let mut facts = labelled("Truss 1", "Truss-1");
        facts.structures.push(StructureFact {
            node: "third".into(),
            on_stage: false,
            label: Some("Truss 1 2".into()),
            position: [0.0, 0.0, 0.0],
        });
        for (n, at) in [-2.0f64, 0.0, 2.0].into_iter().enumerate() {
            facts.fixtures.push(fixture(
                &format!("g{n}"),
                FixtureRole::Wash,
                "third",
                [at, 0.0, 9.0],
            ));
        }
        assert_eq!(
            names(&derive_groups(&facts))[2..],
            [
                "washes_horizontal_truss_1",
                "washes_horizontal_truss_1_3",
                "washes_horizontal_truss_1_2",
            ]
        );
    }

    /// Relabelling is the way out: the path changes, so the name does, and the
    /// id does not — whoever holds the suffixed set still holds it.
    #[test]
    fn a_relabel_gives_the_suffixed_set_its_own_word_back() {
        let before = derive_groups(&labelled("Truss 1", "Truss-1"));
        let after = derive_groups(&labelled("Truss 1", "Truss 2"));
        assert_eq!(
            names(&after)[3],
            "washes_horizontal_truss_2",
            "the second row is still borrowing the first's name"
        );
        assert_eq!(before.groups[2].id, after.groups[2].id);
    }

    /// An authored group's name is a word a score holds. A derived path that
    /// comes to spell it yields, rather than the other way round.
    #[test]
    fn an_authored_name_outranks_a_derived_path_that_grew_into_it() {
        let tree = derive_groups(&labelled("Truss 1", "Truss 2"));
        let manual = ManualGroup {
            id: "authored".into(),
            name: "washes_horizontal_truss_1".into(),
            fixtures: vec!["a".into()],
        };
        let mut merged = merge_tree(&tree, &[], &[manual]);
        make_names_distinct(&mut merged);
        let named = |id: &str| {
            merged
                .iter()
                .find(|node| node.id == id)
                .expect("in the tree")
                .name
                .clone()
        };
        assert_eq!(named("authored"), "washes_horizontal_truss_1");
        assert_eq!(named(&tree.groups[2].id), "washes_horizontal_truss_1_2");
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
            .push(fixture("g", FixtureRole::Wash, "high", [4.0, 0.0, 5.0]));
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
        assert!(node.fixtures.contains(&"g".to_string()));

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
        assert_eq!(target.fixtures, ["d", "e", "f", "a", "b", "c"]);
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
        assert_eq!(top_before, ["a", "b", "c"]);
        assert_eq!(top_of(&after).members, ["d", "e", "f"]);
    }

    // -----------------------------------------------------------------------
    // Merge, as a total operation
    // -----------------------------------------------------------------------

    /// The three nodes of `horizontal_facts`'s wash class: the class and its
    /// two rows.
    fn class_and_rows(tree: &DerivedTree) -> (String, String, String) {
        let id = |path: &[&str]| {
            tree.groups
                .iter()
                .find(|group| group.path == path)
                .expect("derives")
                .id
                .clone()
        };
        (
            id(&["washes", "horizontal"]),
            id(&["washes", "horizontal", "top"]),
            id(&["washes", "horizontal", "bottom"]),
        )
    }

    fn merged_row(from: &str, into: &str) -> GroupOverride {
        let mut row = override_row(from, "");
        row.merged_into = Some(into.into());
        row
    }

    /// A into B into C lands everything in C. Following one hop and stopping
    /// left A's fixtures in a node the tree does not show.
    #[test]
    fn a_merge_chain_resolves_to_its_terminal() {
        let facts = horizontal_facts();
        let mut grown = facts.clone();
        grown
            .fixtures
            .push(fixture("g", FixtureRole::Wash, "high", [4.0, 0.0, 5.0]));
        let tree = derive_groups(&grown);
        let (class, top, bottom) = class_and_rows(&tree);

        let merged = merge_tree(
            &tree,
            &[merged_row(&top, &bottom), merged_row(&bottom, &class)],
            &[],
        );
        assert!(!merged.iter().any(|node| node.id == top));
        assert!(!merged.iter().any(|node| node.id == bottom));
        let target = merged
            .iter()
            .find(|node| node.id == class)
            .expect("the terminal stays");
        for fixture in ["a", "b", "c", "d", "e", "f", "g"] {
            assert!(
                target.fixtures.contains(&fixture.to_string()),
                "{fixture} fell out of the chain"
            );
        }
    }

    /// A cycle has no terminal set, so it folds nothing — rather than deleting
    /// both nodes and every fixture in them.
    #[test]
    fn a_merge_cycle_folds_nothing() {
        let tree = derive_groups(&horizontal_facts());
        let (_, top, bottom) = class_and_rows(&tree);
        let merged = merge_tree(
            &tree,
            &[merged_row(&top, &bottom), merged_row(&bottom, &top)],
            &[],
        );
        assert_eq!(merged.len(), tree.groups.len(), "no node disappeared");
        for id in [&top, &bottom] {
            let node = merged.iter().find(|node| &node.id == id).expect("stays");
            assert_eq!(node.fixtures.len(), 3, "and neither absorbed the other");
        }
    }

    /// Merging a parent into its own child cannot orphan the child: whatever
    /// hung off the husk hangs off the target instead.
    #[test]
    fn children_of_a_merged_parent_reparent_onto_the_target() {
        let tree = derive_groups(&horizontal_facts());
        let (class, top, bottom) = class_and_rows(&tree);
        let merged = merge_tree(&tree, &[merged_row(&class, &top)], &[]);

        assert!(!merged.iter().any(|node| node.id == class));
        let sibling = merged
            .iter()
            .find(|node| node.id == bottom)
            .expect("the other row is still in the tree");
        assert_eq!(sibling.parent_id.as_deref(), Some(top.as_str()));
        // And the target counts everything the class did.
        let target = merged.iter().find(|node| node.id == top).expect("stays");
        assert_eq!(target.fixtures.len(), 6);
    }

    /// A node merged into itself is a no-op, not a disappearance.
    #[test]
    fn merging_a_node_into_itself_folds_nothing() {
        let tree = derive_groups(&horizontal_facts());
        let (_, top, _) = class_and_rows(&tree);
        let merged = merge_tree(&tree, &[merged_row(&top, &top)], &[]);
        assert_eq!(merged.len(), tree.groups.len());
    }
}

/// The two canonical group goldens.
///
/// Both are **seeded venues built through the graph API** — real nodes, real
/// sockets, a real solve, and real `.qxf` definitions whose roles come out of
/// [`FixtureRole::of`] rather than being handed in — so a change to the
/// resolver that moves a fixture across a class, or to the role table that
/// moves a bar out of `led bars`, shows up here as a group moving rather than
/// as nothing at all.
#[cfg(test)]
mod goldens {
    use std::collections::{BTreeMap, HashMap};
    use std::path::{Path, PathBuf};

    use luma_render::catalog::{VenueSockets, FIXTURE_CLAMP_SOCKET};
    use luma_scene::venue::{resolve, Edge, Node, NodeKind, Params, VenueGraph, FLOOR_SOCKET};
    use serde_json::{json, Value};

    use crate::fixtures::parser::parse_definition;

    use super::{derive_groups, facts_from, FixtureIdentity, FixtureRole};

    /// The one deck in the catalog every seeded venue is built out of: it has a
    /// `bottom` that sits on the floor and a `top` a fixture can clamp to,
    /// which is the whole of what these goldens need from geometry.
    const DECK: &str = "stage_lab/stage_praticavel_1x1.glb";
    /// A deck twice as wide, for the one thing a square one cannot show: that
    /// the centre `left` and `right` are measured against is the middle of the
    /// surface and not the corner its mesh is modelled from.
    const WIDE_DECK: &str = "stage_lab/stage_praticavel_2x1x1.glb";

    /// A real bar and a real mover, so the goldens pin the role table too.
    const BAR: (&str, &str) = ("Chauvet/Chauvet-COLORband-T3-BT.qxf", "13 Channel");
    const MOVER: (&str, &str) = ("Chauvet/Chauvet-Rogue-R2-Spot.qxf", "18 Channel");

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

    /// `(model, role)` for one shipped definition in one of its modes, read off
    /// the file. A golden that hand-passed the role would pass with the role
    /// table broken.
    fn shipped(definition: (&str, &str)) -> (String, FixtureRole) {
        /// Keyed on the definition's path and the mode read from it.
        type Roles = HashMap<(String, String), (String, FixtureRole)>;
        static CACHE: std::sync::OnceLock<std::sync::Mutex<Roles>> = std::sync::OnceLock::new();
        let cache = CACHE.get_or_init(Default::default);
        let key = (definition.0.to_string(), definition.1.to_string());
        if let Some(hit) = cache.lock().unwrap().get(&key) {
            return hit.clone();
        }
        let path = repo_root()
            .join("resources/fixtures/2511260420")
            .join(definition.0);
        let def = parse_definition(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mode = def
            .modes
            .iter()
            .find(|mode| mode.name == definition.1)
            .unwrap_or_else(|| panic!("{} has no `{}` mode", definition.0, definition.1));
        let answer = (def.model.clone(), FixtureRole::of(&def, mode));
        cache.lock().unwrap().insert(key, answer.clone());
        answer
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
            self.piece_of(id, kind, DECK, host, at);
        }

        fn piece_of(
            &mut self,
            id: &str,
            kind: NodeKind,
            catalog_ref: &str,
            host: (&str, &str),
            at: &[(&str, f64)],
        ) {
            self.graph.insert(Node {
                id: id.into(),
                kind,
                catalog_ref: Some(catalog_ref.into()),
                label: None,
                params: Self::params(at),
            });
            self.attach(id, "bottom", host);
        }

        /// A fixture patched from a shipped definition and clamped to a host
        /// surface at `(u, v, trim)`.
        fn fixture(
            &mut self,
            id: &str,
            definition: (&str, &str),
            host: (&str, &str),
            at: &[(&str, f64)],
        ) {
            let (model, role) = shipped(definition);
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
                model,
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
            let facts = facts_from("golden", &solved, &self.graph, catalog(), &self.fixtures);
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
            // Rounded: the centre is a solved quantity and lands on 5e-17
            // rather than zero, which is float noise a golden should not pin.
            json!({
                "stageCentreX": (facts.stage_centre_x * 1e6).round() / 1e6,
                "groups": groups,
            })
        }

        fn paths(&self) -> Vec<String> {
            self.tree()
                .1
                .groups
                .iter()
                .map(|group| group.path.join("/"))
                .collect()
        }

        /// `path -> the fixtures in it`, sorted — the whole answer, not just
        /// its vocabulary.
        fn sets(&self) -> BTreeMap<String, Vec<String>> {
            self.tree()
                .1
                .groups
                .iter()
                .map(|group| {
                    let mut members = group.members.clone();
                    members.sort();
                    (group.path.join("/"), members)
                })
                .collect()
        }
    }

    /// (a) LED bars around a wall: two horizontal runs at different heights and
    /// two vertical ones down the sides. The horizontal class names its two
    /// rows by height, the vertical one names its two by side, and neither
    /// cross-cut is emitted because each name is carried by exactly one row.
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
                BAR,
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

    /// (b) Spot wings: one tower bolted to each side of the stage, two spots up
    /// each. A wing holds one unlabelled row, so the wing node *is* that row and
    /// its children are the row's splits — and because `top` is then carried by
    /// a half on each side, it becomes a cross-cut, which is exactly when a
    /// cross-cut earns its name.
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
        // side reads a resolved x), but it is why these numbers look backwards.
        let mut n = 0;
        for (id, u) in [("wing_left", 5.0), ("wing_right", -5.0)] {
            venue.piece(
                id,
                NodeKind::Piece,
                ("stage", "top"),
                &[("u", u * mirror), ("v", 0.0)],
            );
            for i in 0..2 {
                n += 1;
                venue.fixture(
                    &format!("spot_{n}"),
                    MOVER,
                    (id, "top"),
                    &[("u", 0.0), ("v", 0.0), ("trim", 1.0 + f64::from(i) * 1.5)],
                );
            }
        }
        venue
    }

    /// (c) Spot towers: two towers a side, one behind the other and hung at
    /// different heights, two spots up each.
    ///
    /// The pair of rows in a wing separates further in depth than in height, so
    /// depth is what names them — a rule that ranked rows by trim would have
    /// called the shorter tower "bottom" and meant nothing by it. Each row then
    /// splits by height, and it is those halves the role's `top` and `bottom`
    /// gather: a row that split does not also cross-cut itself.
    fn spot_towers(mirror: f64) -> Venue {
        let mut venue = Venue::new();
        venue.piece(
            "stage",
            NodeKind::Stage,
            ("venue", FLOOR_SOCKET),
            &[("u", 0.0), ("v", 0.0)],
        );
        let mut n = 0;
        for (side, u) in [("left", 5.0), ("right", -5.0)] {
            for (depth, v, base) in [("front", 0.0, 1.0), ("back", 3.0, 3.0)] {
                let id = format!("{side}_{depth}");
                venue.piece(
                    &id,
                    NodeKind::Piece,
                    ("stage", "top"),
                    &[("u", u * mirror), ("v", v)],
                );
                for i in 0..2 {
                    n += 1;
                    venue.fixture(
                        &format!("spot_{n}"),
                        MOVER,
                        (&id, "top"),
                        &[("u", 0.0), ("v", 0.0), ("trim", base + f64::from(i) * 1.5)],
                    );
                }
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

    const SPOT_TOWERS: [&str; 17] = [
        "spots",
        "spots/left wing",
        "spots/left wing/downstage",
        "spots/left wing/downstage/top",
        "spots/left wing/downstage/bottom",
        "spots/left wing/upstage",
        "spots/left wing/upstage/top",
        "spots/left wing/upstage/bottom",
        "spots/right wing",
        "spots/right wing/downstage",
        "spots/right wing/downstage/top",
        "spots/right wing/downstage/bottom",
        "spots/right wing/upstage",
        "spots/right wing/upstage/top",
        "spots/right wing/upstage/bottom",
        "spots/top",
        "spots/bottom",
    ];

    /// What the role table makes of the whole shipped catalogue, printed rather
    /// than asserted: the counts move whenever a definition is added, and a
    /// test that pinned them would fail on a library update rather than on a
    /// rule that broke. Run it when the table changes.
    ///
    /// ```sh
    /// cargo test --lib the_shipped_catalogue_survey -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "a survey, not an assertion"]
    fn the_shipped_catalogue_survey() {
        let mut counts: std::collections::BTreeMap<FixtureRole, usize> = Default::default();
        let mut by_type: std::collections::BTreeMap<(String, FixtureRole), usize> =
            Default::default();
        let mut unreadable = 0usize;
        let mut total = 0usize;
        let mut stack = vec![repo_root().join("resources/fixtures")];
        while let Some(directory) = stack.pop() {
            for entry in std::fs::read_dir(&directory)
                .into_iter()
                .flatten()
                .flatten()
            {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "qxf") {
                    continue;
                }
                total += 1;
                let Ok(def) = parse_definition(&path) else {
                    unreadable += 1;
                    continue;
                };
                let Some(mode) = def.modes.first() else {
                    continue;
                };
                let role = FixtureRole::of(&def, mode);
                *counts.entry(role).or_default() += 1;
                *by_type.entry((def.type_.clone(), role)).or_default() += 1;
            }
        }
        println!("{total} definitions, {unreadable} unreadable");
        for (role, n) in &counts {
            println!("{:>8}  {n}", role.as_str());
        }
        println!("--- Type -> role");
        for ((type_, role), n) in &by_type {
            println!("{type_:>20} -> {:<8} {n}", role.as_str());
        }
    }

    /// Set this to rewrite the capture instead of checking it — the same split
    /// `render-goldens` draws between `--check` and a capture run, except that
    /// checking is what a plain `cargo test` does. A test that rewrote a shared
    /// checkout's golden and then failed left the next agent unable to tell a
    /// stale capture from a fresh one.
    const REGENERATE: &str = "LUMA_UPDATE_GOLDENS";

    #[test]
    fn the_venue_groups_golden_is_current() {
        let path = repo_root().join("harness/goldens/venue-groups.json");
        let mut want = serde_json::to_string_pretty(&json!({
            "ledBarsWall": led_bars_wall(1.0).capture(),
            "spotWings": spot_wings(1.0).capture(),
            "spotTowers": spot_towers(1.0).capture(),
        }))
        .expect("the capture serializes");
        want.push('\n');

        if std::env::var_os(REGENERATE).is_some() {
            std::fs::write(&path, &want).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            return;
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            have == want,
            "{} is stale.\n{}\nRerun with {REGENERATE}=1 to recapture.",
            path.display(),
            first_difference(&have, &want),
        );
    }

    /// The first line that differs, with its neighbours — enough to name the
    /// rule that moved without printing two hundred lines of JSON.
    fn first_difference(have: &str, want: &str) -> String {
        let (have, want): (Vec<&str>, Vec<&str>) = (have.lines().collect(), want.lines().collect());
        let at = (0..have.len().max(want.len()))
            .find(|i| have.get(*i) != want.get(*i))
            .unwrap_or(0);
        let line =
            |lines: &[&str], i: usize| lines.get(i).copied().unwrap_or("<end of file>").to_string();
        format!(
            "line {}:\n  on disk: {}\n  derived: {}",
            at + 1,
            line(&have, at),
            line(&want, at),
        )
    }

    #[test]
    fn the_goldens_produce_the_canonical_trees() {
        assert_eq!(led_bars_wall(1.0).paths(), LED_BARS_WALL);
        assert_eq!(spot_wings(1.0).paths(), SPOT_WINGS);
        assert_eq!(spot_towers(1.0).paths(), SPOT_TOWERS);
    }

    /// Reflect a rig about stage centre and the tree reflects with it: the same
    /// sets, under the names their mirrored positions earn.
    ///
    /// Compared as `path -> members`, not as a list of paths. Equal path lists
    /// only prove the vocabulary survived; which fixtures went where is the
    /// half of the answer a sign error actually breaks.
    #[test]
    fn a_mirrored_rig_derives_the_same_tree() {
        for (built, mirrored) in [
            (led_bars_wall(1.0), led_bars_wall(-1.0)),
            (spot_wings(1.0), spot_wings(-1.0)),
            (spot_towers(1.0), spot_towers(-1.0)),
        ] {
            assert_eq!(mirrored.sets(), reflect(&built.sets()));
        }
    }

    /// A tree with `left` and `right` swapped — the whole of what a reflection
    /// about stage centre does to a name. Height and depth keep their words,
    /// because the reflection does not touch either axis.
    fn reflect(sets: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
        sets.iter()
            .map(|(path, members)| {
                let path = path
                    .split('/')
                    .map(|segment| match segment {
                        "left wing" => "right wing",
                        "right wing" => "left wing",
                        "left" => "right",
                        "right" => "left",
                        other => other,
                    })
                    .collect::<Vec<_>>()
                    .join("/");
                (path, members.clone())
            })
            .collect()
    }

    /// Defect: stage centre averaged the stage nodes' *origins*, and a deck's
    /// origin is a corner of its mesh — half a deck off the middle. A rig hung
    /// symmetrically about the real centre of a 2 m deck therefore came out
    /// entirely on one side of it.
    ///
    /// Built on the wide deck because a square one hides the bug: half of 1 m
    /// is inside the spacing here, and half of 2 m is not.
    #[test]
    fn a_rig_symmetric_about_the_stage_derives_a_symmetric_tree() {
        let mut venue = Venue::new();
        venue.piece_of(
            "stage",
            NodeKind::Stage,
            WIDE_DECK,
            ("venue", FLOOR_SOCKET),
            &[("u", 0.0), ("v", 0.0)],
        );
        let mut n = 0;
        for u in [-0.75, 0.75] {
            let tower = format!("tower_{}", if u < 0.0 { "a" } else { "b" });
            venue.piece(
                &tower,
                NodeKind::Piece,
                ("stage", "top"),
                &[("u", u), ("v", 0.0)],
            );
            for i in 0..2 {
                n += 1;
                venue.fixture(
                    &format!("spot_{n}"),
                    MOVER,
                    (&tower, "top"),
                    &[("u", 0.0), ("v", 0.0), ("trim", 1.0 + f64::from(i))],
                );
            }
        }
        let paths = venue.paths();
        assert!(
            paths.contains(&"spots/left wing".to_string())
                && paths.contains(&"spots/right wing".to_string()),
            "both sides of the deck's middle, not both on one: {paths:?}"
        );
    }
}
