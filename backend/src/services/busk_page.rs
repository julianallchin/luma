//! A busk page, made from a venue's rig.
//!
//! The page never names a fixture: its effects are shapes over a group's
//! space and where they play is chosen by group cells, so a small rig and a
//! large one get the same page and only the groups row grows. [`make`] is
//! the whole rule, from the rig alone to patterns and a page; [`add`] puts
//! them in the set beside what is there, never over it.
//!
//! The rows, top to bottom, each left out when the rig cannot use it: the
//! groups, the base color (a list with no targets, so it colors the groups
//! chosen), the accent color (a list setting the accent effects' own
//! colors), the effects (the base ones take the color below, the accent
//! ones their own), movement (one at a time) and hits (held, or struck by
//! each press and fading on their own). Select, Clear, tap, the master, the
//! effects' levels and their speed stand in a column at the right. Every
//! pattern is an ordinary Perform pattern, edited like any other.

use std::collections::BTreeSet;

use luma_patterns::clip_graph::{ClipGraph, Input, Kind, Node};
use luma_patterns::press::TimeUnit;
use luma_patterns::{presets, BlendMode, ClipPreset, Selection};
use sqlx::SqlitePool;

use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::error::CommandError;
use crate::models::groups::FixtureRole;
use crate::models::perform::{
    Cell, CellContent, ControlTarget, LayoutPage, ListValue, PerformPattern,
};
use crate::services::groups::GroupSources;
use crate::services::perform_set::Set;

/// What a busk page reads of a venue: each fixture's role and whether it
/// aims, and each named group by its fixtures.
#[derive(Clone, Debug, Default)]
pub struct Rig {
    pub fixtures: Vec<RigFixture>,
    pub groups: Vec<RigGroup>,
}

#[derive(Clone, Debug)]
pub struct RigFixture {
    pub id: String,
    pub role: FixtureRole,
    pub moves: bool,
}

#[derive(Clone, Debug)]
pub struct RigGroup {
    pub name: String,
    pub fixtures: Vec<String>,
}

/// The base effects, which take the color below, under the accent
/// effects, which have their own.
const BASE: [&str; 3] = ["Wash", "Pulse", "Breathe"];
const ACCENT: [&str; 3] = ["Chase", "Sparkle", "Ripple"];
const MOVEMENT: [&str; 4] = ["Sweep", "Circle", "Figure-8", "Nod wave"];

/// The most group cells: `all` and seven groups.
const MOST_GROUPS: usize = 8;
/// The roles a busk page has a group cell for, in the order it shows them.
const ROLES: [FixtureRole; 6] = [
    FixtureRole::Wash,
    FixtureRole::Pixel,
    FixtureRole::Spot,
    FixtureRole::Beam,
    FixtureRole::Strobe,
    FixtureRole::Blinder,
];
/// The roles the effects light: those that mix a color.
const COLORED: [FixtureRole; 2] = [FixtureRole::Wash, FixtureRole::Pixel];
/// The words a group named for where it hangs has in its name.
const PLACES: [&str; 8] = [
    "left",
    "right",
    "front",
    "back",
    "top",
    "bottom",
    "downstage",
    "upstage",
];

/// Red, blue, amber, violet, white and green.
const BASE_SWATCHES: [&str; 6] = [
    "#ff2a1a", "#2050ff", "#ffa000", "#8a2be2", "#ffffff", "#20e040",
];
/// White first: what a live color is before a value is tapped.
const ACCENT_SWATCHES: [&str; 6] = [
    "#ffffff", "#ff2a1a", "#2050ff", "#ffa000", "#8a2be2", "#20e040",
];

impl Rig {
    /// The group holding exactly every fixture of `role`, when the rig has
    /// any.
    fn role_group(&self, role: FixtureRole) -> Option<&RigGroup> {
        let members: BTreeSet<&str> = self
            .fixtures
            .iter()
            .filter(|fixture| fixture.role == role)
            .map(|fixture| fixture.id.as_str())
            .collect();
        if members.is_empty() {
            return None;
        }
        self.groups
            .iter()
            .find(|group| members_of(group) == members)
    }

    /// The role groups the rig has, in [`ROLES`] order.
    fn role_groups(&self) -> impl Iterator<Item = &RigGroup> {
        ROLES.iter().filter_map(|role| self.role_group(*role))
    }

    /// The groups named for where they hang (`left_movers`, `washes_top`),
    /// the biggest first. A group that is the whole rig, a role's group or
    /// another such group's fixtures again is left out.
    fn place_groups(&self) -> Vec<&RigGroup> {
        let all: BTreeSet<&str> = self.fixtures.iter().map(|f| f.id.as_str()).collect();
        let mut seen: Vec<BTreeSet<&str>> = vec![all];
        seen.extend(self.role_groups().map(members_of));
        let mut found: Vec<&RigGroup> = Vec::new();
        for group in &self.groups {
            let members = members_of(group);
            let placed = group.name.split('_').any(|word| PLACES.contains(&word));
            if placed && !members.is_empty() && !seen.contains(&members) {
                seen.push(members);
                found.push(group);
            }
        }
        found.sort_by_key(|group| std::cmp::Reverse(group.fixtures.len()));
        found
    }

    /// The groups to choose: `all`, then the role groups, then the groups
    /// named for where they hang. Past [`MOST_GROUPS`], roles are kept
    /// before places, and bigger places before smaller.
    fn chosen(&self) -> Vec<&str> {
        std::iter::once("all")
            .chain(
                self.role_groups()
                    .chain(self.place_groups())
                    .map(|group| group.name.as_str()),
            )
            .take(MOST_GROUPS)
            .collect()
    }

    /// Whether every fixture of `group` aims.
    fn all_move(&self, group: &RigGroup) -> bool {
        group.fixtures.iter().all(|id| {
            self.fixtures
                .iter()
                .any(|fixture| &fixture.id == id && fixture.moves)
        })
    }
}

/// `groups` as one selection; `all` when there are none.
fn either<'a>(groups: impl Iterator<Item = &'a RigGroup>) -> String {
    let names: Vec<&str> = groups.map(|group| group.name.as_str()).collect();
    if names.is_empty() {
        "all".into()
    } else {
        names.join(" | ")
    }
}

fn members_of(group: &RigGroup) -> BTreeSet<&str> {
    group.fixtures.iter().map(String::as_str).collect()
}

/// A busk page's patterns, the bottom of the set first, and the page.
pub struct Busk {
    pub patterns: Vec<PerformPattern>,
    pub page: LayoutPage,
}

/// An effect: its pattern, and its live color and speed nodes, when it has
/// them.
struct Effect {
    pattern: PerformPattern,
    color: Option<String>,
    speed: Option<String>,
}

/// The busk page `rig` gets, called `name`, its patterns from z `bottom` up.
pub fn make(rig: &Rig, name: String, bottom: i64) -> Busk {
    let colored = either(COLORED.iter().filter_map(|role| rig.role_group(*role)));
    let effects = |names: &[&str], selection: &str, below: bool| -> Vec<Effect> {
        names
            .iter()
            .filter_map(|name| presets().clip(name))
            .map(|preset| effect(preset, selection, below))
            .collect()
    };
    let base = effects(&BASE, &colored, true);
    let accent = effects(&ACCENT, &colored, false);
    let aims = rig.fixtures.iter().any(|fixture| fixture.moves);
    let movement = if aims {
        let movers = either(rig.role_groups().filter(|group| rig.all_move(group)));
        effects(&MOVEMENT, &movers, false)
    } else {
        Vec::new()
    };
    let hits = hits(rig);

    let groups = rig.chosen();
    let played = base.len() + accent.len();
    let width = [groups.len(), played, movement.len(), hits.len()]
        .into_iter()
        .max()
        .unwrap_or(1) as u16;
    let mut cells = Vec::new();
    let mut row = 0;
    cells.extend(groups.iter().enumerate().map(|(column, group)| {
        cell(
            column as u16,
            row,
            1,
            CellContent::Group {
                label: String::new(),
                group: (*group).to_owned(),
            },
        )
    }));
    row += 1;
    let accents: Vec<ControlTarget> = accent
        .iter()
        .filter_map(|effect| Some(node(&effect.pattern, effect.color.as_ref()?)))
        .collect();
    let swatches = |hexes: [&str; 6]| {
        hexes.map(|hex| {
            ListValue::Color(luma_patterns::color_space::from_hex(hex).expect("a hex color"))
        })
    };
    let base_colors = swatches(BASE_SWATCHES)
        .into_iter()
        .chain([ListValue::Below])
        .collect();
    let accent_colors = (!accents.is_empty()).then(|| swatches(ACCENT_SWATCHES).to_vec());
    for (label, values, targets) in [
        ("Base color", Some(base_colors), Vec::new()),
        ("Accent color", accent_colors, accents),
    ] {
        let Some(values) = values else {
            continue;
        };
        cells.push(cell(
            0,
            row,
            width,
            CellContent::List {
                label: label.into(),
                values,
                fade: 1.,
                unit: TimeUnit::Beats,
                targets,
                step: None,
            },
        ));
        row += 1;
    }
    let mut one_at_a_time = Vec::new();
    for (patterns, alone) in [
        (
            base.iter()
                .chain(&accent)
                .map(|effect| &effect.pattern)
                .collect::<Vec<_>>(),
            false,
        ),
        (
            movement.iter().map(|effect| &effect.pattern).collect(),
            true,
        ),
        (hits.iter().collect(), false),
    ] {
        if patterns.is_empty() {
            continue;
        }
        cells.extend(patterns.iter().enumerate().map(|(column, pattern)| {
            cell(
                column as u16,
                row,
                1,
                CellContent::Pattern {
                    pattern_id: pattern.id.clone(),
                },
            )
        }));
        if alone {
            one_at_a_time.push(row);
        }
        row += 1;
    }

    // The side column: Select, Clear, tap, the master, the effects' levels
    // and speeds.
    let rows = row.max(6);
    let levels = base
        .iter()
        .chain(&accent)
        .map(|effect| ControlTarget::Level {
            pattern_id: effect.pattern.id.clone(),
        })
        .collect();
    let speeds = base
        .iter()
        .chain(&accent)
        .chain(&movement)
        .filter_map(|effect| Some(node(&effect.pattern, effect.speed.as_ref()?)))
        .collect();
    for (at, content) in [
        (0, CellContent::Select),
        (1, CellContent::Clear),
        (2, CellContent::Tap),
    ] {
        cells.push(cell(width, at, 1, content));
    }
    cells.push(Cell {
        height: rows - 5,
        ..cell(width, 3, 1, CellContent::Master)
    });
    for (at, label, targets) in [(rows - 2, "Levels", levels), (rows - 1, "Speed", speeds)] {
        cells.push(cell(
            width,
            at,
            1,
            CellContent::Fader {
                label: label.into(),
                targets,
            },
        ));
    }

    let patterns = base
        .into_iter()
        .chain(accent)
        .chain(movement)
        .map(|effect| effect.pattern)
        .chain(hits)
        .zip(bottom..)
        .map(|(pattern, z_index)| PerformPattern { z_index, ..pattern })
        .collect();
    Busk {
        patterns,
        page: LayoutPage {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            columns: width + 1,
            rows,
            one_at_a_time,
            cells,
        },
    }
}

/// Put a busk page from `rig`, and its patterns, in the set: a new page
/// ("Busk", or "Busk 2" when that is taken), its patterns over the set's.
/// Answers the page's id.
pub fn add(set: &mut Set, rig: &Rig) -> String {
    let name = (1..)
        .map(|n| match n {
            1 => "Busk".to_owned(),
            n => format!("Busk {n}"),
        })
        .find(|name| !set.layout.pages.iter().any(|page| &page.name == name))
        .expect("a free name");
    let busk = make(rig, name, set.top());
    for pattern in busk.patterns {
        set.put(pattern);
    }
    let id = busk.page.id.clone();
    set.layout.pages.push(busk.page);
    id
}

/// The venue's rig, as [`make`] reads it.
pub async fn read_rig(
    pool: &SqlitePool,
    resource_root: &std::path::Path,
    venue_id: &str,
) -> Result<Rig, CommandError> {
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Venue(venue_id)).await?;
    Ok(GroupSources::read(resource_root, &mut access).await?.rig())
}

/// The hits, each on a pad that plays from the press: a strobe (on the
/// strobes, else everything) and black on top while held; a blinder (when
/// the rig has a group of them) and full white struck by each press.
fn hits(rig: &Rig) -> Vec<PerformPattern> {
    let strobe = presets().clip("Strobe").map(|preset| &preset.graph);
    let black = ClipGraph::new([(
        "color1".to_owned(),
        Node::new(Kind::Color)
            .with_input("color", Input::Color([0.; 3]))
            .with_input("brightness", 0.),
    )]);
    let strobes = rig
        .role_group(FixtureRole::Strobe)
        .map_or("all", |group| group.name.as_str());
    let blinders = rig
        .role_group(FixtureRole::Blinder)
        .map(|group| group.name.as_str());
    [
        strobe.map(|graph| hold("Strobe", strobes, graph.clone())),
        blinders.map(|group| strike("Blinder", group)),
        Some(strike("White", "all")),
        Some(hold("Black", "all", black)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// A pad that plays from the press.
fn flash_pad() -> Node {
    Node::new(Kind::Button)
        .with_setting("name", "pad")
        .with_setting("mode", "flash")
        .with_setting("start", "now")
}

/// `graph` with a pad that plays it while held.
fn hold(name: &str, selection: &str, graph: ClipGraph) -> PerformPattern {
    let mut graph = graph.with_pad();
    if let Some(pad) = graph.pad() {
        graph.nodes.insert(pad.node, flash_pad());
    }
    pattern(name, selection, BlendMode::Replace, graph)
}

/// Full white struck by each press of its pad and fading out over a beat,
/// however long the pad is held: a clock of presses, so hits pressed fast
/// overlap, each fading on its own.
fn strike(name: &str, selection: &str) -> PerformPattern {
    let fade = presets().curve("Ramp down").expect("the shipped Ramp down");
    let graph = ClipGraph::new([
        ("pad".to_owned(), flash_pad()),
        (
            "hit".to_owned(),
            Node::new(Kind::Time)
                .with_input("on", Input::wire("pad"))
                .with_input("duration", 1.),
        ),
        (
            "fade".to_owned(),
            Node::new(Kind::Curve)
                .with_input("x", Input::wire("hit"))
                .with_input("shape", Input::Points(fade.clone())),
        ),
        (
            "color1".to_owned(),
            Node::new(Kind::Color)
                .with_input("color", Input::Color([1.; 3]))
                .with_input("alpha", Input::wire("fade")),
        ),
    ]);
    pattern(name, selection, BlendMode::Replace, graph)
}

/// `preset` with its pad, a live color into its output's color, and a speed
/// fader into each of its clocks; taking the color below when `below`, else
/// by the preset's blend. Its live color is its own, for when Select gives
/// it one.
fn effect(preset: &ClipPreset, selection: &str, below: bool) -> Effect {
    let mut graph = preset.graph.clone().with_pad();
    let color = live_color(&mut graph);
    let speed = speed(&mut graph);
    let blend = if below {
        BlendMode::Below
    } else {
        preset.blend_mode
    };
    Effect {
        pattern: pattern(&preset.name, selection, blend, graph),
        color,
        speed,
    }
}

/// Wire a live color, named `color`, into the output's color, when the
/// output is a color. Answers its node.
fn live_color(graph: &mut ClipGraph) -> Option<String> {
    let output = graph
        .output()
        .filter(|(_, node)| node.kind == Kind::Color)
        .map(|(id, _)| id.to_owned())?;
    let id = graph.next_id(Kind::Control);
    graph.nodes.insert(
        id.clone(),
        Node::new(Kind::Control)
            .with_setting("name", "color")
            .with_setting("kind", "color"),
    );
    graph
        .nodes
        .get_mut(&output)?
        .inputs
        .insert("color".into(), Input::wire(&id));
    Some(id)
}

/// Wire a fader, named `speed`, into each clock whose `every` is a number,
/// through a step: the preset's period at rest, half of it (twice as fast)
/// pushed past half way. A fader rests at 0, so the step goes up from the
/// period the preset has. Answers its node, when there is a clock.
fn speed(graph: &mut ClipGraph) -> Option<String> {
    let clocks: Vec<(String, f64)> = graph
        .nodes
        .iter()
        .filter(|(_, node)| node.kind == Kind::Time)
        .filter_map(|(id, node)| match node.inputs.get("every") {
            Some(Input::Number(every)) => Some((id.clone(), *every)),
            _ => None,
        })
        .collect();
    if clocks.is_empty() {
        return None;
    }
    let fader = graph.next_id(Kind::Control);
    graph.nodes.insert(
        fader.clone(),
        Node::new(Kind::Control).with_setting("name", "speed"),
    );
    let step = presets().curve("Steps 2").expect("the shipped Steps 2");
    for (clock, every) in clocks {
        let curve = graph.next_id(Kind::Curve);
        graph.nodes.insert(
            curve.clone(),
            Node::new(Kind::Curve)
                .with_input("x", Input::wire(&fader))
                .with_input("shape", Input::Points(step.clone()))
                .with_input("low", every)
                .with_input("high", every / 2.),
        );
        if let Some(clock) = graph.nodes.get_mut(&clock) {
            clock.inputs.insert("every".into(), Input::wire(curve));
        }
    }
    Some(fader)
}

fn pattern(name: &str, selection: &str, blend_mode: BlendMode, graph: ClipGraph) -> PerformPattern {
    PerformPattern {
        id: uuid::Uuid::new_v4().to_string(),
        venue_id: String::new(),
        name: name.to_owned(),
        selection: Selection {
            expression: selection.to_owned(),
        },
        z_index: 0,
        blend_mode,
        seed: uuid::Uuid::new_v4().as_u64_pair().0,
        graph,
    }
}

fn node(pattern: &PerformPattern, node: &str) -> ControlTarget {
    ControlTarget::Node {
        pattern_id: pattern.id.clone(),
        node: node.to_owned(),
    }
}

fn cell(column: u16, row: u16, width: u16, content: CellContent) -> Cell {
    Cell {
        id: uuid::Uuid::new_v4().to_string(),
        column,
        row,
        width,
        height: 1,
        content,
    }
}

#[cfg(test)]
mod tests;
