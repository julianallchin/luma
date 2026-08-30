//! The Gauntlet view: a resolved venue as a top-down text map.
//!
//! The cheapest verification channel that still says *where things are*
//! (`docs/design/venue-graph.md`, Verification channels). A render costs a GPU
//! and a few hundred kilobytes to answer "does the room look right"; this
//! answers "is the rig the shape I asked for" in something a diff can localise
//! to one row.
//!
//! # Why it lives here
//!
//! A footprint is the resolver's pose *and* the piece's measured extent: a
//! deck's outline comes from a GLB's bounding box and a truss's from the
//! generator's end frames at that node's own span. [`crate::catalog`] is the
//! one place either is read — re-opening the GLB to ask again would be a
//! second reading of one number — so the map is drawn beside them rather than
//! in a layer that would have to be handed both.
//!
//! # The orientation, once
//!
//! Data space is `+x` stage right, `+y` upstage, `+z` up
//! (`fixture_kinematics::StageDirection`). The map is the plan **as the house
//! sees it**: columns run `+x` to `-x` and rows run `+y` to `-y`, which puts
//! upstage at the top, the house at the bottom, stage right on the left and
//! stage left on the right — the audience's own left and right, because the
//! performer faces them. The header says so on every map, so a map pasted into
//! a transcript carries its own convention.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use glam::{DVec2, DVec3};
use luma_scene::catalog::{Family, Geometry, PieceKind};
use luma_scene::venue::{NodeKind, NodePose, ResolvedVenue};

use crate::catalog::{node_params, procedural_sockets, CatalogSockets};

/// How the map is quantized and labelled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileMap {
    /// Metres per cell, both axes. One cell is one character.
    pub cell_m: f64,
    /// How far apart the edge coordinates are written, in metres.
    pub label_every_m: f64,
}

impl Default for TileMap {
    /// Half-metre cells — a truss panel is 0.5 m and a deck is a whole number
    /// of them, so structure lands on cell boundaries rather than straddling
    /// them — labelled every two metres.
    fn default() -> Self {
        Self {
            cell_m: 0.5,
            label_every_m: 2.0,
        }
    }
}

impl TileMap {
    /// Smallest cell the map will draw at. Below this a room is thousands of
    /// characters wide, which is not a picture anyone reads.
    pub const MIN_CELL_M: f64 = 0.05;

    /// Largest cell the map will draw at: past this a whole rig is one glyph.
    pub const MAX_CELL_M: f64 = 10.0;

    /// The map of one solved venue.
    ///
    /// Total: an empty venue, a venue whose every node is unplaced, and a
    /// nonsense `cell_m` all produce a map rather than an error — `cell_m` is
    /// clamped to `MIN_CELL_M..=MAX_CELL_M` and a non-finite one falls back to
    /// the default.
    #[must_use]
    pub fn draw(self, venue: &ResolvedVenue, catalog: &CatalogSockets) -> String {
        let options = self.sanitized();
        let marks: Vec<Mark> = venue
            .poses()
            .filter_map(|pose| Mark::of(pose, catalog))
            .collect();
        let mut out = String::new();
        if let Some(mut grid) = Grid::over(&marks, options.cell_m) {
            for mark in &marks {
                grid.stamp(mark);
            }
            write_header(&mut out, &grid, options);
            grid.write(&mut out, options);
        } else {
            writeln!(out, "gauntlet view · nothing is placed in this venue").ok();
            writeln!(out, "{ORIENTATION}").ok();
        }
        write_unplaced(&mut out, venue);
        out
    }

    fn sanitized(self) -> Self {
        let cell_m = if self.cell_m.is_finite() {
            self.cell_m.clamp(Self::MIN_CELL_M, Self::MAX_CELL_M)
        } else {
            Self::default().cell_m
        };
        // Rounded to whole cells: the rulers label lattice lines, so a step
        // that is not a whole number of cells would land between them and
        // print nothing at all.
        let requested = if self.label_every_m.is_finite() {
            self.label_every_m
        } else {
            Self::default().label_every_m
        };
        let steps = (requested / cell_m).round().max(MIN_LABEL_CELLS);
        let label_every_m = steps * cell_m;
        Self {
            cell_m,
            label_every_m,
        }
    }
}

/// Fewest columns between two labels on the ruler. A coordinate is up to four
/// characters (`-10`, `-2.5`), so a step tighter than this cannot print them
/// both and the ruler comes out with holes in it.
const MIN_LABEL_CELLS: f64 = 4.0;

/// The one sentence that makes a map readable out of context. See the module
/// docs for why it reads the way it does.
const ORIENTATION: &str =
    "plan as the house sees it: columns run +x → −x (stage right → stage left), \
     rows run +y → −y (upstage → house)";

// ---------------------------------------------------------------------------
// what a node puts on the map
// ---------------------------------------------------------------------------

/// Which glyph survives when several nodes land in one cell.
///
/// Ascending: a deck is the ground everything else stands on, structure is
/// drawn over it, and a light is drawn over that — a map that hid the movers
/// under the truss they hang from would answer the question nobody asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Layer {
    Deck,
    Equipment,
    Structure,
    Fixture,
}

/// The area a node covers, in data-space metres.
enum Footprint {
    /// A convex outline — a measured bounding box, projected.
    Hull(Vec<DVec2>),
    /// A stick between its two ends.
    Segment(DVec2, DVec2),
    /// Something smaller than a cell, or with no measured extent.
    Point(DVec2),
}

/// One node, ready to stamp.
struct Mark {
    layer: Layer,
    glyph: char,
    footprint: Footprint,
}

impl Mark {
    /// What one resolved pose puts on the map, or `None` for a pose that is a
    /// frame rather than a thing: the venue root, and an array's anchor.
    ///
    /// [`NodePose::is_set_piece`] is the predicate for the second, so the map
    /// draws exactly the objects the renderer and `venue.pieces` do — one
    /// answer to "what is in this room", not a third.
    fn of(pose: &NodePose, catalog: &CatalogSockets) -> Option<Self> {
        if pose.kind == NodeKind::Fixture {
            return Some(Self {
                layer: Layer::Fixture,
                glyph: FIXTURE,
                footprint: Footprint::Point(origin_of(pose)),
            });
        }
        if !pose.is_set_piece() {
            return None;
        }
        let catalog_ref = pose.catalog_ref.as_deref()?;
        // A venue outlives a catalog: a row naming a piece that has since been
        // dropped still has a pose, and a `?` on the map is the honest answer.
        let Some(piece) = luma_scene::catalog::piece(catalog_ref) else {
            return Some(Self {
                layer: Layer::Structure,
                glyph: UNKNOWN,
                footprint: Footprint::Point(origin_of(pose)),
            });
        };
        Some(match piece.geometry {
            Geometry::Procedural(Family::Truss) => {
                let (a, b) = truss_ends(pose);
                Self {
                    layer: Layer::Structure,
                    glyph: axis_glyph(a, b),
                    footprint: Footprint::Segment(a, b),
                }
            }
            Geometry::Procedural(Family::Corner | Family::Hinge) => Self {
                layer: Layer::Structure,
                glyph: CORNER,
                footprint: Footprint::Point(origin_of(pose)),
            },
            Geometry::Mesh { .. } => {
                let hull = hull_of(pose, catalog.bounds(catalog_ref));
                let (layer, glyph) = mesh_glyph(piece.kind, &hull);
                Self {
                    layer,
                    glyph,
                    footprint: match hull.len() {
                        0 => Footprint::Point(origin_of(pose)),
                        _ => Footprint::Hull(hull),
                    },
                }
            }
        })
    }
}

/// A deck.
const DECK: char = '▓';
/// A truss lying across the room, `x`-dominant.
const TRUSS_ACROSS: char = '═';
/// A truss running upstage/downstage, `y`-dominant.
const TRUSS_DEPTH: char = '║';
/// A truss standing up: shorter in plan than one cell.
const TOWER: char = 'T';
/// A corner block or a hinge.
const CORNER: char = '╬';
/// A patched, placed fixture.
const FIXTURE: char = '·';
/// A row naming geometry the catalog no longer has.
const UNKNOWN: char = '?';

/// The legend, in the order it prints. The closed vocabulary of the map: every
/// glyph any node can produce appears here exactly once, so a map cannot show a
/// character the legend cannot name.
const LEGEND: [(char, &str); 12] = [
    (DECK, "deck"),
    (TRUSS_ACROSS, "truss across"),
    (TRUSS_DEPTH, "truss up/downstage"),
    (TOWER, "tower"),
    (CORNER, "corner"),
    (FIXTURE, "fixture"),
    ('S', "speaker"),
    ('C', "cdj"),
    ('M', "mixer"),
    ('r', "guardrail"),
    ('t', "stand"),
    ('_', "cable cover"),
];

/// Glyph and layer for a piece drawn from a mesh.
///
/// Total over [`PieceKind`], so a new catalog kind is a compile error here
/// rather than a blank cell on the map.
fn mesh_glyph(kind: PieceKind, hull: &[DVec2]) -> (Layer, char) {
    match kind {
        PieceKind::Floor => (Layer::Deck, DECK),
        PieceKind::Truss => {
            let (a, b) = extent_axis(hull);
            (Layer::Structure, axis_glyph(a, b))
        }
        PieceKind::Guardrail => (Layer::Structure, 'r'),
        PieceKind::Speaker => (Layer::Equipment, 'S'),
        PieceKind::Cdj => (Layer::Equipment, 'C'),
        PieceKind::Mixer => (Layer::Equipment, 'M'),
        PieceKind::Stand => (Layer::Equipment, 't'),
        PieceKind::CableCover => (Layer::Equipment, '_'),
    }
}

/// Which way a stick lies, as one character.
///
/// A stick whose two ends are within a few centimetres of each other in plan is
/// standing up — that is what a tower *is*, and the map says so rather than
/// drawing a one-cell truss nobody can tell from a joint.
fn axis_glyph(a: DVec2, b: DVec2) -> char {
    let d = b - a;
    if d.length() < STANDING_M {
        TOWER
    } else if d.x.abs() >= d.y.abs() {
        TRUSS_ACROSS
    } else {
        TRUSS_DEPTH
    }
}

/// How much plan-view length a stick needs before it counts as lying down. A
/// truss section is 0.29 m across, so anything shorter than this is the section
/// itself seen end-on.
const STANDING_M: f64 = 0.3;

/// The pose's own origin, in data-space plan metres.
fn origin_of(pose: &NodePose) -> DVec2 {
    plan(pose.world.w_axis.truncate())
}

/// A three-space point (Y-up) as data-space plan metres.
///
/// `luma_scene::coords::three_from_data` is an involution, so it is also
/// `data <- three`; the plan is that point's `x` and `y`.
fn plan(three: DVec3) -> DVec2 {
    let data = luma_scene::coords::three_from_data_d(three);
    DVec2::new(data.x, data.y)
}

/// The two ends of a generated stick, in plan.
///
/// Read off [`procedural_sockets`] at the node's own parameters rather than
/// from a span constant: a placed 6 m truss has its ends 6 m apart, and the
/// socket supply is where that is already known.
fn truss_ends(pose: &NodePose) -> (DVec2, DVec2) {
    let sockets = procedural_sockets(node_params(Family::Truss, &pose.params));
    let end = |name: &str| {
        sockets
            .iter()
            .find(|s| s.name == name)
            .map_or(DVec3::ZERO, |s| s.position)
    };
    (
        plan(pose.world.transform_point3(end("end_a"))),
        plan(pose.world.transform_point3(end("end_b"))),
    )
}

/// The plan outline of a measured piece: its bounding box's eight corners,
/// posed and projected.
///
/// The projection of a convex body is convex, so the hull of the projected
/// corners *is* the outline — exact for a box under any rotation, not just yaw.
fn hull_of(pose: &NodePose, bounds: Option<luma_scene::aabb::DAabb>) -> Vec<DVec2> {
    let Some(b) = bounds else {
        return Vec::new();
    };
    let corners = [b.min.x, b.max.x].into_iter().flat_map(move |x| {
        [b.min.y, b.max.y].into_iter().flat_map(move |y| {
            [b.min.z, b.max.z]
                .into_iter()
                .map(move |z| DVec3::new(x, y, z))
        })
    });
    convex_hull(
        corners
            .map(|c| plan(pose.world.transform_point3(c)))
            .collect(),
    )
}

/// The longest diagonal of an outline, as a pair — what [`axis_glyph`] reads to
/// decide which way a piece lies.
fn extent_axis(hull: &[DVec2]) -> (DVec2, DVec2) {
    let mut best = (DVec2::ZERO, DVec2::ZERO);
    let mut longest = -1.0;
    for (i, a) in hull.iter().enumerate() {
        for b in &hull[i + 1..] {
            let d = a.distance_squared(*b);
            if d > longest {
                longest = d;
                best = (*a, *b);
            }
        }
    }
    best
}

/// Monotone-chain convex hull, counter-clockwise, no collinear points.
fn convex_hull(mut points: Vec<DVec2>) -> Vec<DVec2> {
    points.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    points.dedup();
    if points.len() < 3 {
        return points;
    }
    let mut hull: Vec<DVec2> = Vec::with_capacity(points.len() * 2);
    let turns_right = |hull: &[DVec2], p: DVec2, floor: usize| {
        hull.len() > floor && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0
    };
    for &p in &points {
        while turns_right(&hull, p, 1) {
            hull.pop();
        }
        hull.push(p);
    }
    // The upper hull may not eat into the lower one, so its floor is where the
    // lower one ended. The last point is already on both, hence the `skip`.
    let floor = hull.len();
    for &p in points.iter().rev().skip(1) {
        while turns_right(&hull, p, floor) {
            hull.pop();
        }
        hull.push(p);
    }
    // The first point closes the walk; it is already the hull's start.
    hull.pop();
    hull
}

fn cross(o: DVec2, a: DVec2, b: DVec2) -> f64 {
    (a - o).perp_dot(b - o)
}

// ---------------------------------------------------------------------------
// the grid
// ---------------------------------------------------------------------------

/// The rasterized map.
///
/// Cells are indexed on an **absolute** lattice — cell `i` on an axis covers
/// `[i·cell, (i+1)·cell)` in metres, whatever else is in the room — so moving a
/// piece one metre moves its glyph `1/cell` columns and nothing else shifts.
/// A lattice anchored to the room's own extent would move the whole map every
/// time its widest piece did.
struct Grid {
    /// Lattice index of the leftmost column, which is the **largest** `x`.
    x_index_max: i64,
    /// Lattice index of the top row, which is the largest `y`.
    y_index_max: i64,
    cols: usize,
    rows: usize,
    cell_m: f64,
    cells: Vec<Option<(Layer, char)>>,
}

impl Grid {
    /// A grid that contains every mark, or `None` if there are none.
    fn over(marks: &[Mark], cell_m: f64) -> Option<Self> {
        let mut lo = DVec2::splat(f64::INFINITY);
        let mut hi = DVec2::splat(f64::NEG_INFINITY);
        for point in marks.iter().flat_map(Mark::points) {
            lo = lo.min(point);
            hi = hi.max(point);
        }
        if !lo.x.is_finite() || !hi.x.is_finite() {
            return None;
        }
        let index = |v: f64| (v / cell_m).floor() as i64;
        let (x_index_min, x_index_max) = (index(lo.x), index(hi.x));
        let (y_index_min, y_index_max) = (index(lo.y), index(hi.y));
        let cols = (x_index_max - x_index_min + 1) as usize;
        let rows = (y_index_max - y_index_min + 1) as usize;
        Some(Self {
            x_index_max,
            y_index_max,
            cols,
            rows,
            cell_m,
            cells: vec![None; cols * rows],
        })
    }

    /// Where a metric point lands, or `None` if it is off the map — which only
    /// a rounding hair at the boundary can produce, since the grid was sized to
    /// hold every point.
    fn cell_of(&self, p: DVec2) -> Option<(usize, usize)> {
        let col = self.x_index_max - (p.x / self.cell_m).floor() as i64;
        let row = self.y_index_max - (p.y / self.cell_m).floor() as i64;
        (col >= 0 && row >= 0 && (col as usize) < self.cols && (row as usize) < self.rows)
            .then_some((col as usize, row as usize))
    }

    /// The lattice line at a cell's leading edge — its larger `x` and larger
    /// `y`, which is its left and top on the page. This is what the rulers
    /// label, so every printed coordinate is a multiple of `cell_m` rather than
    /// a cell centre landing on `3.25`.
    fn edge_of(&self, col: usize, row: usize) -> DVec2 {
        DVec2::new(
            (self.x_index_max - col as i64 + 1) as f64,
            (self.y_index_max - row as i64 + 1) as f64,
        ) * self.cell_m
    }

    /// The metric centre of one cell.
    fn centre_of(&self, col: usize, row: usize) -> DVec2 {
        DVec2::new(
            (self.x_index_max - col as i64) as f64 + 0.5,
            (self.y_index_max - row as i64) as f64 + 0.5,
        ) * self.cell_m
    }

    fn plot(&mut self, p: DVec2, layer: Layer, glyph: char) {
        let Some((col, row)) = self.cell_of(p) else {
            return;
        };
        let slot = &mut self.cells[row * self.cols + col];
        if slot.is_none_or(|(existing, _)| layer >= existing) {
            *slot = Some((layer, glyph));
        }
    }

    fn stamp(&mut self, mark: &Mark) {
        match &mark.footprint {
            Footprint::Point(p) => self.plot(*p, mark.layer, mark.glyph),
            Footprint::Segment(a, b) => {
                // Half-cell steps: the shortest step that cannot skip a cell
                // whichever way the stick lies.
                let steps = ((b - a).length() / (self.cell_m * 0.5)).ceil().max(1.0);
                for i in 0..=(steps as usize) {
                    let t = i as f64 / steps;
                    self.plot(a.lerp(*b, t), mark.layer, mark.glyph);
                }
            }
            Footprint::Hull(hull) => {
                let mut lo = DVec2::splat(f64::INFINITY);
                let mut hi = DVec2::splat(f64::NEG_INFINITY);
                for p in hull {
                    lo = lo.min(*p);
                    hi = hi.max(*p);
                }
                let Some((c0, r0)) = self.cell_of(DVec2::new(hi.x, hi.y)) else {
                    return;
                };
                let Some((c1, r1)) = self.cell_of(DVec2::new(lo.x, lo.y)) else {
                    return;
                };
                for row in r0..=r1 {
                    for col in c0..=c1 {
                        if inside(hull, self.centre_of(col, row)) {
                            self.plot(self.centre_of(col, row), mark.layer, mark.glyph);
                        }
                    }
                }
                // A piece narrower than a cell can contain no cell centre at
                // all; it is still in the room, so its own centre is drawn.
                if !self.rect_has(c0, r0, c1, r1, mark.glyph) {
                    self.plot((lo + hi) * 0.5, mark.layer, mark.glyph);
                }
            }
        }
    }

    fn rect_has(&self, c0: usize, r0: usize, c1: usize, r1: usize, glyph: char) -> bool {
        (r0..=r1).any(|row| {
            (c0..=c1).any(|col| self.cells[row * self.cols + col].is_some_and(|(_, g)| g == glyph))
        })
    }

    /// The metric span the map covers, `(min, max)` per axis.
    fn extent(&self) -> (DVec2, DVec2) {
        let hi =
            DVec2::new((self.x_index_max + 1) as f64, (self.y_index_max + 1) as f64) * self.cell_m;
        let lo = DVec2::new(
            (self.x_index_max + 1 - self.cols as i64) as f64,
            (self.y_index_max + 1 - self.rows as i64) as f64,
        ) * self.cell_m;
        (lo, hi)
    }

    /// The ruler, then the rows.
    ///
    /// A ruler labels **lattice lines, not column indices**: a tick goes on the
    /// leading edge of a cell only when that edge is a whole multiple of the
    /// label step. Stepping by index instead would print whatever coordinate
    /// the room's widest piece happened to start the grid at — `2.5`, `-1.5` —
    /// which is a number about the map rather than about the room.
    fn write(&self, out: &mut String, options: TileMap) {
        let mut ticks = " ".repeat(GUTTER);
        let mut labels = " ".repeat(GUTTER);
        for col in 0..self.cols {
            let x = self.edge_of(col, 0).x;
            if !on_step(x, options.label_every_m) {
                continue;
            }
            let at = GUTTER + col;
            while ticks.chars().count() < at {
                ticks.push(' ');
            }
            ticks.push('╷');
            let text = metres(x);
            let start = at.saturating_sub(text.chars().count() / 2);
            if labels.chars().count() < start {
                while labels.chars().count() < start {
                    labels.push(' ');
                }
                labels.push_str(&text);
            }
        }
        writeln!(out, "{}", ticks.trim_end()).ok();
        writeln!(out, "{}", labels.trim_end()).ok();
        for row in 0..self.rows {
            let y = self.edge_of(0, row).y;
            let mut line = if on_step(y, options.label_every_m) {
                format!("{:>width$} ", metres(y), width = GUTTER - 1)
            } else {
                " ".repeat(GUTTER)
            };
            for col in 0..self.cols {
                line.push(self.cells[row * self.cols + col].map_or(' ', |(_, g)| g));
            }
            writeln!(out, "{}", line.trim_end()).ok();
        }
    }
}

/// Whether a lattice line carries a label. Compared against a tenth of a
/// millimetre, which is finer than any rig and coarser than `f64` drift over
/// the tens of metres a room spans.
fn on_step(v: f64, step: f64) -> bool {
    let offset = (v / step).round() * step - v;
    offset.abs() < 1e-4
}

/// A labelled coordinate. Whole metres read as `4`, anything finer keeps one
/// decimal, and a value that rounds to zero never prints as `-0`.
fn metres(v: f64) -> String {
    let text = if (v - v.round()).abs() < 1e-4 {
        format!("{:.0}", v.round())
    } else {
        format!("{v:.1}")
    };
    if text
        .trim_start_matches('-')
        .chars()
        .all(|c| c == '0' || c == '.')
    {
        text.trim_start_matches('-').to_string()
    } else {
        text
    }
}

/// Width of the left-hand coordinate column, including its trailing space.
const GUTTER: usize = 7;

/// Whether a point is inside a counter-clockwise convex outline. A degenerate
/// outline (a segment or a point) contains nothing, which is what
/// [`Grid::stamp`]'s fallback exists to catch.
fn inside(hull: &[DVec2], p: DVec2) -> bool {
    hull.len() >= 3
        && hull
            .iter()
            .zip(hull.iter().cycle().skip(1))
            .all(|(a, b)| cross(*a, *b, p) >= 0.0)
}

impl Mark {
    /// Every metric point the mark occupies, for sizing the grid.
    fn points(&self) -> Vec<DVec2> {
        match &self.footprint {
            Footprint::Point(p) => vec![*p],
            Footprint::Segment(a, b) => vec![*a, *b],
            Footprint::Hull(hull) => hull.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// the prose around the picture
// ---------------------------------------------------------------------------

fn write_header(out: &mut String, grid: &Grid, options: TileMap) {
    let (lo, hi) = grid.extent();
    writeln!(
        out,
        "gauntlet view · {:.1} m cells · x {:.1}…{:.1} m · y {:.1}…{:.1} m",
        options.cell_m, lo.x, hi.x, lo.y, hi.y
    )
    .ok();
    writeln!(out, "{ORIENTATION}").ok();
    let used: BTreeSet<char> = grid
        .cells
        .iter()
        .filter_map(|c| c.map(|(_, g)| g))
        .collect();
    let legend: Vec<String> = LEGEND
        .iter()
        .filter(|(glyph, _)| used.contains(glyph))
        .map(|(glyph, name)| format!("{glyph} {name}"))
        .chain(
            used.contains(&UNKNOWN)
                .then(|| format!("{UNKNOWN} unknown piece")),
        )
        .collect();
    writeln!(out, "legend: {}", legend.join("  ")).ok();
    out.push('\n');
}

/// What the room has but has not placed, by the root of each branch — the same
/// report `venue.unplaced` gives, so the map and the binding cannot disagree
/// about whether a wing is missing or merely in the tray.
fn write_unplaced(out: &mut String, venue: &ResolvedVenue) {
    out.push('\n');
    if venue.unplaced().is_empty() {
        writeln!(out, "unplaced: none").ok();
        return;
    }
    writeln!(out, "unplaced:").ok();
    for node in venue.unplaced() {
        let label = node.label.as_deref().unwrap_or(&node.node);
        let more = match node.descendants {
            0 => String::new(),
            n => format!(" + {n} more"),
        };
        writeln!(out, "  {label} ({}){more}", node.kind.as_str()).ok();
    }
}
