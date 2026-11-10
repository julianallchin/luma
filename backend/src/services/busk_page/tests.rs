use luma_patterns::clip_graph::{Input, Kind};

use super::*;
use crate::models::groups::normalize_group_name;
use crate::models::perform::PerformLayout;

fn fixtures(prefix: &str, count: usize, role: FixtureRole, moves: bool) -> Vec<RigFixture> {
    (0..count)
        .map(|n| RigFixture {
            id: format!("{prefix}-{n}"),
            role,
            moves,
        })
        .collect()
}

fn group(name: &str, members: &[&RigFixture]) -> RigGroup {
    RigGroup {
        id: name.into(),
        name: name.into(),
        fixtures: members.iter().map(|fixture| fixture.id.clone()).collect(),
    }
}

/// `rig` with a group per role it has, named as derivation names them.
fn with_roles(fixtures: Vec<RigFixture>, mut places: Vec<(&str, Vec<usize>)>) -> Rig {
    let mut groups: Vec<RigGroup> = ROLES
        .iter()
        .filter_map(|role| {
            let members: Vec<&RigFixture> = fixtures.iter().filter(|f| f.role == *role).collect();
            (!members.is_empty())
                .then(|| group(&normalize_group_name(role.display_name()), &members))
        })
        .collect();
    groups.extend(places.drain(..).map(|(name, at)| {
        group(
            name,
            &at.iter().map(|at| &fixtures[*at]).collect::<Vec<_>>(),
        )
    }));
    Rig { fixtures, groups }
}

/// Pars and strobes only.
fn small() -> Rig {
    let mut fixtures = fixtures("par", 4, FixtureRole::Wash, false);
    fixtures.extend(self::fixtures("strobe", 2, FixtureRole::Strobe, false));
    with_roles(fixtures, Vec::new())
}

/// Pars, spot movers and beam movers, hung left and right.
fn medium() -> Rig {
    let mut fixtures = fixtures("par", 6, FixtureRole::Wash, false);
    fixtures.extend(self::fixtures("spot", 4, FixtureRole::Spot, true));
    fixtures.extend(self::fixtures("beam", 4, FixtureRole::Beam, true));
    let left = vec![0, 1, 2, 6, 7, 10, 11];
    let right = vec![3, 4, 5, 8, 9, 12, 13];
    with_roles(fixtures, vec![("left", left), ("right", right)])
}

/// Every role, and more groups named for where they hang than fit.
fn large() -> Rig {
    let mut fixtures = Vec::new();
    for (prefix, role, moves) in [
        ("par", FixtureRole::Wash, false),
        ("pixel", FixtureRole::Pixel, false),
        ("spot", FixtureRole::Spot, true),
        ("beam", FixtureRole::Beam, true),
        ("strobe", FixtureRole::Strobe, false),
        ("blinder", FixtureRole::Blinder, false),
    ] {
        fixtures.extend(self::fixtures(prefix, 8, role, moves));
    }
    // Ten groups named for a side, each smaller than the last.
    let mut rig = with_roles(fixtures, Vec::new());
    for n in 0..10 {
        let (side, from, size) = (["left", "right"][n % 2], (n % 2) * 24, 24 - n * 2);
        let members: Vec<&RigFixture> = rig.fixtures[from..from + size].iter().collect();
        let placed = group(&format!("washes_{side}_{n}"), &members);
        rig.groups.push(placed);
    }
    rig
}

fn busk(rig: &Rig) -> Busk {
    make(rig, "Busk".into(), 0)
}

/// What the page holds in `row`, left to right: a group cell's group, a
/// list's label or a pattern's name.
fn row(busk: &Busk, row: u16) -> Vec<String> {
    let mut cells: Vec<&Cell> = busk
        .page
        .cells
        .iter()
        .filter(|cell| cell.row == row && cell.column + 1 < busk.page.columns)
        .collect();
    cells.sort_by_key(|cell| cell.column);
    cells
        .iter()
        .map(|cell| match &cell.content {
            CellContent::Group { group, .. } => group.clone(),
            CellContent::Pattern { pattern_id } => pattern(busk, pattern_id).name.clone(),
            CellContent::List { label, .. } => label.clone(),
            other => format!("{other:?}"),
        })
        .collect()
}

fn pattern<'a>(busk: &'a Busk, id: &str) -> &'a PerformPattern {
    busk.patterns
        .iter()
        .find(|pattern| pattern.id == id)
        .expect("a pattern of the page")
}

fn named<'a>(busk: &'a Busk, name: &str) -> &'a PerformPattern {
    busk.patterns
        .iter()
        .find(|pattern| pattern.name == name)
        .unwrap_or_else(|| panic!("no pattern {name}"))
}

/// The page and its patterns pass the checks a stored set must.
fn stores(busk: &Busk) {
    for pattern in &busk.patterns {
        pattern
            .check()
            .unwrap_or_else(|why| panic!("{}: {why}", pattern.name));
    }
    PerformLayout {
        pages: vec![busk.page.clone()],
    }
    .check(&busk.patterns)
    .unwrap();
    let cells = busk
        .page
        .cells
        .iter()
        .filter_map(Cell::pattern_id)
        .collect::<BTreeSet<_>>();
    assert_eq!(cells.len(), busk.patterns.len(), "one cell per pattern");
}

#[test]
fn a_small_rig_gets_no_movement_and_few_groups() {
    let busk = busk(&small());
    stores(&busk);
    assert_eq!(row(&busk, 0), ["all", "washes", "strobes"]);
    assert_eq!(row(&busk, 1), ["Base color"]);
    assert_eq!(row(&busk, 2), ["Accent color"]);
    assert_eq!(
        row(&busk, 3),
        ["Wash", "Pulse", "Breathe", "Chase", "Sparkle", "Ripple"]
    );
    assert_eq!(row(&busk, 4), ["Strobe", "White", "Black"], "no blinders");
    assert_eq!(busk.page.rows, 6, "the side column's six");
    assert!(busk.page.one_at_a_time.is_empty(), "nothing moves");
    assert_eq!(named(&busk, "Chase").selection.key(), "washes");
    assert_eq!(named(&busk, "Strobe").selection.key(), "strobes");
}

#[test]
fn movers_get_a_movement_row_that_plays_one_at_a_time() {
    let busk = busk(&medium());
    stores(&busk);
    assert_eq!(
        row(&busk, 0),
        ["all", "washes", "spots", "beams", "left", "right"]
    );
    assert_eq!(row(&busk, 4), ["Sweep", "Circle", "Figure-8", "Nod wave"]);
    assert_eq!(busk.page.one_at_a_time, [4]);
    assert_eq!(named(&busk, "Sweep").selection.key(), "spots|beams");
    assert_eq!(row(&busk, 5), ["Strobe", "White", "Black"]);
}

#[test]
fn past_the_groups_row_roles_come_before_places_and_big_before_small() {
    let rig = large();
    let busk = busk(&rig);
    stores(&busk);
    let targets = row(&busk, 0);
    assert_eq!(targets.len(), MOST_GROUPS);
    assert_eq!(
        targets[..7],
        ["all", "washes", "led_bars", "spots", "beams", "strobes", "blinders"]
    );
    let size = |name: &str| {
        rig.groups
            .iter()
            .find(|group| group.name == name)
            .map_or(0, |group| group.fixtures.len())
    };
    let biggest = rig
        .groups
        .iter()
        .filter(|group| group.name.contains("_left_") || group.name.contains("_right_"))
        .map(|group| group.fixtures.len())
        .max();
    assert_eq!(Some(size(&targets[7])), biggest);
    assert_eq!(named(&busk, "Blinder").selection.key(), "blinders");
    assert_eq!(
        named(&busk, "Pulse").selection.key(),
        "washes|led_bars"
    );
}

#[test]
fn a_rig_with_no_groups_plays_everything_on_all() {
    let mut rig = small();
    rig.groups.clear();
    let busk = busk(&rig);
    stores(&busk);
    assert_eq!(row(&busk, 0), ["all"]);
    assert!(busk
        .patterns
        .iter()
        .all(|pattern| pattern.selection == luma_patterns::Selection::All));
}

#[test]
fn the_effects_stack_base_under_accent_under_hits_with_black_on_top() {
    let busk = make(&medium(), "Busk".into(), 10);
    let z = |name| named(&busk, name).z_index;
    assert!(z("Breathe") < z("Chase"));
    assert!(z("Ripple") < z("Strobe"));
    let top = busk.patterns.iter().map(|p| p.z_index).max();
    assert_eq!(Some(z("Black")), top);
    assert!(busk.patterns.iter().all(|pattern| pattern.z_index >= 10));
}

#[test]
fn an_effect_has_a_pad_a_live_color_and_a_speed_from_its_period() {
    let busk = busk(&small());
    let chase = named(&busk, "Chase");
    let live = chase.graph.live_inputs();
    let names: Vec<&str> = live.iter().map(|input| input.name.as_str()).collect();
    assert!(names.contains(&"pad") && names.contains(&"color") && names.contains(&"speed"));
    let pad = chase.graph.pad().expect("a pad");
    assert!(
        matches!(pad.kind, luma_patterns::clip_graph::LiveInputKind::Button(press)
        if press.mode == luma_patterns::press::PressMode::Toggle)
    );

    // Its clock's every steps from the preset's period to half of it.
    let preset = presets().clip("Chase").unwrap();
    let period = preset
        .graph
        .nodes
        .values()
        .find_map(|node| match (node.kind, node.inputs.get("every")) {
            (Kind::Time, Some(Input::Number(every))) => Some(*every),
            _ => None,
        })
        .unwrap();
    let curve = chase
        .graph
        .nodes
        .values()
        .find_map(|node| match (node.kind, node.inputs.get("every")) {
            (Kind::Time, Some(Input::Wire(curve))) => chase.graph.nodes.get(curve),
            _ => None,
        })
        .expect("a speed into its clock");
    let bound = |name| match curve.inputs.get(name) {
        Some(Input::Number(value)) => *value,
        other => panic!("{name}: {other:?}"),
    };
    assert_eq!(bound("low"), period, "at rest, its own period");
    assert_eq!(bound("high"), period / 2., "pushed up, twice as fast");

    // The accent list sets the accent effects' own colors; the base list
    // sets no targets, so it is the base color, and has the color below.
    let list = |label: &str| {
        busk.page
            .cells
            .iter()
            .find_map(|cell| match &cell.content {
                CellContent::List {
                    label: l,
                    values,
                    targets,
                    ..
                } if l == label => Some((values.clone(), targets.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no list {label}"))
    };
    let (base, unlinked) = list("Base color");
    assert!(unlinked.is_empty());
    assert_eq!(base.last(), Some(&ListValue::Below));
    let (_, accents) = list("Accent color");
    let accented: BTreeSet<&str> = accents
        .iter()
        .filter_map(|target| match target {
            ControlTarget::Node { pattern_id, .. } => {
                Some(pattern(&busk, pattern_id).name.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(accented, BTreeSet::from(ACCENT));
    for name in BASE {
        assert_eq!(named(&busk, name).blend_mode, BlendMode::Below, "{name}");
    }
    assert_ne!(chase.blend_mode, BlendMode::Below, "an accent has its own");

    // The side faders reach every effect.
    let fader = |label: &str| {
        busk.page
            .cells
            .iter()
            .find(|cell| matches!(&cell.content, CellContent::Fader { label: l, .. } if l == label))
            .map(|cell| cell.content.targets().len())
    };
    assert_eq!(fader("Levels"), Some(6));
    // Wash has no clock, so no speed.
    assert_eq!(fader("Speed"), Some(5));
    // Select and Clear stand at the top of the side column.
    let side = |content: CellContent| {
        busk.page
            .cells
            .iter()
            .find(|cell| cell.content == content)
            .map(|cell| (cell.column, cell.row))
    };
    let right = busk.page.columns - 1;
    assert_eq!(side(CellContent::Select), Some((right, 0)));
    assert_eq!(side(CellContent::Clear), Some((right, 1)));
}

#[test]
fn a_hit_flashes_from_the_press() {
    let busk = busk(&large());
    for name in ["Strobe", "Blinder", "White", "Black"] {
        let pad = named(&busk, name).graph.pad().expect("a pad");
        let luma_patterns::clip_graph::LiveInputKind::Button(press) = pad.kind else {
            panic!("{name}'s pad is a button");
        };
        assert_eq!(press.mode, luma_patterns::press::PressMode::Flash, "{name}");
        assert_eq!(press.start, luma_patterns::press::Start::Now, "{name}");
    }
}

#[test]
fn strobe_and_black_play_while_held_and_white_and_blinder_are_struck_by_each_press() {
    let busk = busk(&large());
    for name in ["Strobe", "Black"] {
        assert!(
            named(&busk, name).graph.pad_gates(),
            "{name} plays while held"
        );
    }
    for name in ["Blinder", "White"] {
        let graph = &named(&busk, name).graph;
        assert!(!graph.pad_gates(), "{name} plays out once struck");
        let clock = graph
            .nodes
            .iter()
            .find(|(_, node)| node.kind == Kind::Time)
            .map(|(id, _)| id.as_str())
            .expect("a clock of presses");
        assert!(graph.overlaps(clock), "{name}'s hits pressed fast overlap");
        assert!(graph.check().is_ok(), "{name}");
    }
}

#[test]
fn adding_a_page_keeps_every_page_and_pattern_there() {
    let rig = small();
    let mut set = Set {
        patterns: Vec::new(),
        layout: PerformLayout::default(),
    };
    let first = add(&mut set, &rig);
    let before = set.clone();
    let second = add(&mut set, &rig);
    assert_ne!(first, second);
    let names: Vec<&str> = set
        .layout
        .pages
        .iter()
        .map(|page| page.name.as_str())
        .collect();
    assert_eq!(names, ["Busk", "Busk 2"]);
    assert_eq!(set.layout.pages[0], before.layout.pages[0]);
    for pattern in &before.patterns {
        assert!(set.patterns.contains(pattern), "{} kept", pattern.name);
    }
    let old_top = before.patterns.iter().map(|p| p.z_index).max().unwrap();
    assert!(set
        .patterns
        .iter()
        .filter(|pattern| !before.patterns.contains(pattern))
        .all(|pattern| pattern.z_index > old_top));
}

#[test]
fn every_preset_a_busk_page_names_ships() {
    for name in BASE
        .iter()
        .chain(&ACCENT)
        .chain(&MOVEMENT)
        .chain(&["Strobe"])
    {
        assert!(presets().clip(name).is_some(), "{name}");
    }
}
