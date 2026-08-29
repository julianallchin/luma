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
//!   <class>                  where its runs sit: a wing, or which way it runs
//!     <row>                  one run — one structure piece — of that class
//!   top | bottom             a cross-cut: the same row name across two classes
//!   left | right
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
//! runs: `horizontal` (spread along stage x) or `vertical` (spread up).
//! Unplaced fixtures are their own class, `unplaced`.
//!
//! **Row** — one per distribution: one structure piece, one row, never merged
//! with the piece beside it. Two towers on the left are two rows of one
//! `left wing`, because they are two things a human points at separately. A
//! row is named by the piece's authored label when it has one, and otherwise by
//! its position among the class's rows: `top`/`bottom` down a horizontal class
//! or a wing, `left`/`right` across a vertical one, and `row 1`…`row n` when
//! there are more than two and no labels to tell them apart. A class holding
//! **one** unlabelled row emits no row node — the class node already is that
//! set, and two names for one set is the thing this design refuses.
//!
//! **Cross-cuts** — under a role, `top`/`bottom`/`left`/`right` unioning rows of
//! that name across classes. Emitted only when **two or more** rows contribute,
//! for the same reason: a cross-cut drawing from one row is that row under a
//! second name.

use std::collections::BTreeMap;

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
    /// role and a row (`unplaced`); they cannot get a position split.
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
    /// The node's kind, kept for the diagnostics a caller may want; the rule
    /// itself never reads it.
    pub kind: String,
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

/// Which end of a class's ordering a row sits at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Half {
    Top,
    Bottom,
    Left,
    Right,
}

impl Half {
    /// The four names a cross-cut can carry, in the order the tree lists them.
    const ALL: [Half; 4] = [Half::Top, Half::Bottom, Half::Left, Half::Right];

    fn as_str(self) -> &'static str {
        match self {
            Half::Top => "top",
            Half::Bottom => "bottom",
            Half::Left => "left",
            Half::Right => "right",
        }
    }

    fn as_str_owned(self) -> String {
        self.as_str().to_string()
    }
}

/// The two axes a run can spread along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    /// Up: a tower, a vertical run.
    Level,
    /// Across the stage: a truss, a horizontal run.
    Side,
}

/// Where a run sits — the tree's second level, and the thing rows are ordered
/// and named within.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Bolted to a stage, on one side of its centre.
    Wing(Half),
    /// Free-standing, named for the way it runs.
    Run(Axis),
    /// Not in the room at all.
    Unplaced,
}

impl Class {
    fn name(self) -> &'static str {
        match self {
            Class::Wing(Half::Left) => "left wing",
            Class::Wing(_) => "right wing",
            Class::Run(Axis::Side) => "horizontal",
            Class::Run(Axis::Level) => "vertical",
            Class::Unplaced => UNPLACED,
        }
    }

    /// The two names this class's rows take when there are exactly two of them,
    /// in the order [`Class::order_key`] sorts them.
    ///
    /// A wing and a horizontal class both stack their rows by height, so the
    /// question a human asks of them is "which one is the high one"; a vertical
    /// class stands its rows side by side, so the question is "which side".
    fn row_names(self) -> Option<[Half; 2]> {
        match self {
            Class::Wing(_) | Class::Run(Axis::Side) => Some([Half::Top, Half::Bottom]),
            Class::Run(Axis::Level) => Some([Half::Left, Half::Right]),
            Class::Unplaced => None,
        }
    }

    /// Where one row sorts among its siblings: descending height for a class
    /// whose rows stack, ascending stage x for one whose rows stand abreast.
    /// Negated for the first so both read "first is the one the first name
    /// belongs to".
    fn order_key(self, row: &Row<'_>) -> f64 {
        match self {
            Class::Run(Axis::Level) => row.mean(0),
            _ => -row.mean(2),
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

        // `row name -> the rows that carried it`, for the cross-cuts below.
        let mut cross: BTreeMap<&'static str, (Vec<&FixtureFact>, usize)> = BTreeMap::new();

        for (class, mut rows) in classes {
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

            // One unlabelled row is the class itself under a second name.
            if rows.len() == 1 && facts.label_of(&rows[0]).is_none() {
                continue;
            }

            rows.sort_by(|a, b| {
                class
                    .order_key(a)
                    .partial_cmp(&class.order_key(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            for (index, row) in rows.iter().enumerate() {
                // A labelled row carries its label and nothing else — including
                // into the cross-cut, which unions rows *named* `top` and not
                // rows that happen to be the higher of two.
                let half = match (facts.label_of(row), class.row_names()) {
                    (None, Some(names)) if rows.len() == 2 => Some(names[index]),
                    _ => None,
                };
                let name = match facts.label_of(row) {
                    Some(label) => label.to_string(),
                    // More than two rows is a stack rather than a pair, and a
                    // stack's rungs are numbered: there is no third word for
                    // "between top and bottom" that means the same thing twice.
                    None => half.map_or_else(|| format!("row {}", index + 1), Half::as_str_owned),
                };
                let mut path = class_path.clone();
                path.push(name);
                push(&mut tree, facts, Some(&class_id), role, path, &row.members);
                if let Some(half) = half {
                    let entry = cross.entry(half.as_str()).or_default();
                    entry.0.extend(row.members.iter().copied());
                    entry.1 += 1;
                }
            }
        }

        // A cross-cut drawing from a single row *is* that row, under a second
        // name. Two names for one set is the thing this whole design is trying
        // not to do, so it is not emitted.
        for half in Half::ALL {
            let Some((side, contributors)) = cross.get(half.as_str()) else {
                continue;
            };
            if *contributors < 2 {
                continue;
            }
            let mut path = role_path.clone();
            path.push(half.as_str().to_string());
            push(&mut tree, facts, Some(&role_id), role, path, side);
        }
    }

    tree
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
    /// The mean of one coordinate over the row's placed fixtures, `0.0` when
    /// none are placed — which only happens for the unplaced row, and that row
    /// is never ordered against a sibling.
    fn mean(&self, axis: usize) -> f64 {
        let values: Vec<f64> = self
            .members
            .iter()
            .filter_map(|fixture| Some(fixture.placement.as_ref()?.position[axis]))
            .collect();
        if values.is_empty() {
            return 0.0;
        }
        values.iter().sum::<f64>() / values.len() as f64
    }

    /// Which way the row runs: along the stage, or up it. A run of one fixture
    /// has no direction and reads as horizontal, the class a lone par on a
    /// truss belongs to.
    ///
    /// Read off the fixtures rather than the structure's geometry on purpose: a
    /// run's direction is the line the things on it lie along, which is true of
    /// a generated truss, a measured GLB and a wall alike, and needs no catalog.
    fn axis(&self) -> Axis {
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
        if positions.len() > 1 && spread(2) > spread(0) {
            Axis::Level
        } else {
            Axis::Side
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

/// Which class a row belongs to.
fn class_of(facts: &VenueFacts, row: &Row<'_>) -> Class {
    let Some(structure) = facts.structure_of(row) else {
        return Class::Unplaced;
    };
    if structure.on_stage {
        return Class::Wing(if structure.position[0] < facts.stage_centre_x {
            Half::Left
        } else {
            Half::Right
        });
    }
    Class::Run(row.axis())
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
                    kind: pose.map_or("piece", |pose| pose.kind.as_str()).to_string(),
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
/// Parents come before children.
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
            kind: "run".into(),
            position: [x, 0.0, 0.0],
        }
    }

    /// Two horizontal runs at different heights: one `horizontal` class, two
    /// rows, named top and bottom by which one is higher.
    fn horizontal_facts() -> VenueFacts {
        VenueFacts {
            venue_id: "v".into(),
            stage_centre_x: 0.0,
            structures: vec![structure("high", false, 0.0), structure("low", false, 0.0)],
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
                fixture("b", FixtureRole::Wash, "left", [-4.0, 0.0, 5.0]),
                fixture("c", FixtureRole::Wash, "right", [4.0, 0.0, 1.0]),
                fixture("d", FixtureRole::Wash, "right", [4.0, 0.0, 5.0]),
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
    /// same height are two rows, because they are two things.
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

    /// One unlabelled row is the class under a second name, so it is not
    /// emitted twice.
    #[test]
    fn a_class_of_one_unlabelled_row_is_a_leaf() {
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
            vec!["spots", "spots/left wing"]
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
            vec!["spots", "spots/left wing"]
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
            for (id, x) in [("a", -2.0), ("b", 2.0)] {
                facts.fixtures.push(fixture(
                    &format!("{id}{n}"),
                    FixtureRole::Wash,
                    &node,
                    [x, 0.0, z],
                ));
            }
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
            .push(fixture("e", FixtureRole::Wash, "high", [0.0, 0.0, 5.0]));
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
        for fixture in ["a", "b", "c", "d", "e"] {
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
            assert_eq!(node.fixtures.len(), 2, "and neither absorbed the other");
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
        assert_eq!(target.fixtures.len(), 4);
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
    use std::collections::HashMap;
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

    /// (b) Spot wings: four towers bolted to the stage, two rows of spots up
    /// each side. Each wing class names its two rows by height, and because
    /// `top` is then carried by a row on each side it becomes a cross-cut —
    /// which is exactly when a cross-cut earns its name.
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
        let towers = [
            ("wing_left_far", 6.0, 1.0),
            ("wing_left_near", 4.0, 4.0),
            ("wing_right_far", -6.0, 1.0),
            ("wing_right_near", -4.0, 4.0),
        ];
        let mut n = 0;
        for (id, u, base) in towers {
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
                    &[("u", 0.0), ("v", 0.0), ("trim", base + f64::from(i) * 1.0)],
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
