use super::*;
use crate::models::perform::{Cell, ControlTarget, LayoutPage};
use luma_patterns::BlendMode;
use luma_patterns::ClipGraph;

/// `graph` with a pad ([`ClipGraph::with_pad`]) that plays at once, not
/// on the next beat, for a test that presses it and looks straight away.
pub(crate) fn with_pad_at_once(graph: ClipGraph) -> ClipGraph {
    let mut graph = graph.with_pad();
    if let Some(pad) = graph.pad() {
        if let Some(node) = graph.nodes.get_mut(&pad.node) {
            node.settings.insert("start".into(), "now".into());
        }
    }
    graph
}

/// A pattern of `id` with no pad.
fn pattern(id: &str) -> PerformPattern {
    PerformPattern {
        id: id.into(),
        venue_id: "venue".into(),
        name: id.into(),
        selection: luma_patterns::Selection {
            expression: "all".into(),
        },
        z_index: 0,
        blend_mode: luma_patterns::BlendMode::Replace,
        seed: 1,
        graph: luma_patterns::ClipGraph::default(),
    }
}

/// A pattern whose pad plays as `mode` ("toggle" or "flash"), at once,
/// fading in over `attack` and out over `release`, linearly, in `unit`.
fn faded(id: &str, mode: &str, attack: f64, release: f64, unit: &str) -> PerformPattern {
    let mut graph = with_pad_at_once(
        ClipGraph::from_json(r#"{"version": 3, "nodes": {"color1": {"kind": "color"}}}"#).unwrap(),
    );
    let node = graph.pad().unwrap().node;
    let settings = &mut graph.nodes.get_mut(&node).unwrap().settings;
    for (name, value) in [
        ("mode", mode.to_owned()),
        ("attack", attack.to_string()),
        ("release", release.to_string()),
        ("unit", unit.to_owned()),
        ("shape", "linear".to_owned()),
    ] {
        settings.insert(name.into(), value);
    }
    PerformPattern {
        graph,
        ..pattern(id)
    }
}

fn pad(pattern: &PerformPattern) -> String {
    perform::pad(pattern).expect("a pad")
}

fn cell(id: &str, column: u16, row: u16, content: CellContent) -> Cell {
    Cell {
        id: id.into(),
        column,
        row,
        width: 1,
        height: 1,
        content,
    }
}

/// `rows` of pattern ids, the first row one at a time, and `extra`
/// cells.
fn layout(rows: &[&[&str]], extra: Vec<Cell>) -> PerformLayout {
    let mut cells: Vec<Cell> = rows
        .iter()
        .enumerate()
        .flat_map(|(row, ids)| {
            ids.iter().enumerate().map(move |(column, id)| {
                cell(
                    &format!("cell-{id}"),
                    column as u16,
                    row as u16,
                    CellContent::Pattern {
                        pattern_id: (*id).into(),
                    },
                )
            })
        })
        .collect();
    cells.extend(extra);
    PerformLayout {
        pages: vec![LayoutPage {
            id: "page".into(),
            name: "Main".into(),
            columns: 8,
            rows: 4,
            one_at_a_time: vec![0],
            cells,
        }],
    }
}

fn after(start: Instant, seconds: f64) -> Instant {
    start + Duration::from_secs_f64(seconds)
}

/// Whether button `name` is on, or going on, as of its last press.
fn lit(controls: &VenueControls, name: &str) -> bool {
    controls.read().buttons[name].on()
}

#[test]
fn a_pad_fades_in_and_out_by_its_envelope() {
    let controls = VenueControls::default();
    let wash = faded("wash", "toggle", 1.0, 2.0, "seconds");
    let on = pad(&wash);
    install(&controls, &[wash], &PerformLayout::default());
    let start = Instant::now();
    assert_eq!(controls.value_at(&on, start), 0.0, "never pressed");
    controls.press(&on, true, start);
    controls.press(&on, false, after(start, 0.1));
    assert!(lit(&controls, &on), "a toggle holds after release");
    let rising = controls.value_at(&on, after(start, 0.5));
    assert!(0.0 < rising && rising < 1.0, "halfway up: {rising}");
    assert_eq!(controls.value_at(&on, after(start, 1.5)), 1.0);

    controls.press(&on, true, after(start, 2.0));
    assert!(!lit(&controls, &on), "the next press turns it off");
    let falling = controls.value_at(&on, after(start, 3.0));
    assert!(0.0 < falling && falling < 1.0, "releasing: {falling}");
    assert_eq!(controls.value_at(&on, after(start, 4.5)), 0.0);
}

#[test]
fn a_flash_is_on_while_held() {
    let controls = VenueControls::default();
    let hit = faded("hit", "flash", 0.0, 0.0, "seconds");
    let on = pad(&hit);
    install(&controls, &[hit], &PerformLayout::default());
    let start = Instant::now();
    controls.press(&on, true, start);
    assert_eq!(controls.value_at(&on, after(start, 0.5)), 1.0);
    controls.press(&on, false, after(start, 1.0));
    assert_eq!(controls.value_at(&on, after(start, 1.0)), 0.0);
    assert!(!lit(&controls, &on));
}

#[test]
fn beats_follow_the_deck_the_set_follows() {
    let controls = VenueControls::default();
    let slow = faded("wash", "toggle", 4.0, 0.0, "beats");
    let on = pad(&slow);
    install(&controls, &[slow], &PerformLayout::default());
    let start = Instant::now();
    controls.press(&on, true, start);
    // Four beats: two seconds at the tapped 120, one at a deck's 240.
    let tapped = controls.value_at(&on, after(start, 1.0));
    controls.follow(Some(240.0));
    let unread = controls.value_at(&on, after(start, 1.0));
    assert_eq!(unread, tapped, "a deck not read yet is no clock");
    controls.tick(Some((8.0, 4.0)), start);
    let followed = controls.value_at(&on, after(start, 1.0));
    assert!(tapped < followed, "{tapped} < {followed}");
    assert_eq!(followed, 1.0);
    controls.follow(None);
    assert_eq!(controls.value_at(&on, after(start, 1.0)), tapped);
}

#[test]
fn a_followed_deck_not_read_yet_leaves_the_tapped_clock_whole() {
    let start = Instant::now();
    let wash = quantised("wash", "toggle", "beat");
    let on = pad(&wash);
    let controls = tapped(&[wash], &PerformLayout::default(), start);
    // No live set reads the deck: a press half a beat in waits for the
    // tapped tempo's next beat, counted at its tempo, not the deck's.
    controls.follow(Some(240.0));
    controls.press(&on, true, beat(start, 0.5));
    assert_eq!(state_at(&controls, &on, beat(start, 0.9)), (false, true));
    assert_eq!(state_at(&controls, &on, beat(start, 1.0)), (true, false));
}

#[test]
fn a_one_at_a_time_row_releases_the_others() {
    let controls = VenueControls::default();
    let set = [
        faded("red", "toggle", 0.0, 1.0, "seconds"),
        faded("blue", "toggle", 0.0, 0.0, "seconds"),
        faded("chase", "toggle", 0.0, 0.0, "seconds"),
        pattern("plain"),
    ];
    let [red, blue, chase] = [&set[0], &set[1], &set[2]].map(pad);
    install(
        &controls,
        &set,
        &layout(&[&["red", "blue", "plain"], &["chase"]], Vec::new()),
    );
    assert_eq!(
        controls.value(&perform::level("red")),
        1.0,
        "levels start full"
    );
    let start = Instant::now();
    let press = |name: &str, at| controls.press(name, true, at);
    press(&red, start);
    press(&chase, start);
    press(&blue, after(start, 1.0));
    assert!(!lit(&controls, &red), "blue released red");
    assert!(lit(&controls, &blue));
    assert!(lit(&controls, &chase), "in another row");
    let fading = controls.value_at(&red, after(start, 1.5));
    assert!(0.0 < fading && fading < 1.0, "red fades by its own release");
}

#[test]
fn a_fader_cell_moves_every_target() {
    let controls = VenueControls::default();
    let speeds = CellContent::Fader {
        label: "Speed".into(),
        targets: vec![
            ControlTarget::Level {
                pattern_id: "red".into(),
            },
            ControlTarget::Node {
                pattern_id: "chase".into(),
                node: "control1".into(),
            },
            ControlTarget::Named {
                name: "drop".into(),
            },
        ],
    };
    install(
        &controls,
        &[pattern("red"), pattern("chase")],
        &layout(&[], vec![cell("speeds", 0, 1, speeds)]),
    );
    controls.set(&perform::cell("speeds"), 0.25);
    for name in [
        perform::cell("speeds"),
        perform::level("red"),
        perform::node("chase", "control1"),
        "drop".to_owned(),
    ] {
        assert_eq!(controls.value(&name), 0.25, "{name}");
    }
}

/// A list `id` of three colors, red, green and blue, fading over `fade`
/// beats into pattern `wash`'s node `hue`, stepping as `step` says.
fn colors(id: &str, fade: f64, step: Option<Step>) -> Cell {
    cell(
        id,
        0,
        2,
        CellContent::List {
            label: "Colors".into(),
            values: vec![
                ListValue::Color([1., 0., 0.]),
                ListValue::Color([0., 1., 0.]),
                ListValue::Color([0., 0., 1.]),
            ],
            fade,
            unit: TimeUnit::Beats,
            targets: vec![ControlTarget::Node {
                pattern_id: "wash".into(),
                node: "hue".into(),
            }],
            step,
        },
    )
}

/// Tap control `name` at `at`: press and let go.
fn tap(controls: &VenueControls, name: &str, at: Instant) {
    controls.press(name, true, at);
    controls.press(name, false, at);
}

/// Tap value `index` of list `id` at `at`.
fn choose(controls: &VenueControls, id: &str, index: usize, at: Instant) {
    tap(controls, &perform::value(id, index), at);
}

#[test]
fn a_value_fades_a_lists_targets_to_it_in_time() {
    let controls = VenueControls::default();
    install(&controls, &[], &layout(&[], vec![colors("cell", 2., None)]));
    let hue = perform::node("wash", "hue");
    let start = Instant::now();
    assert_eq!(controls.color_at(&hue, start), UNSET_COLOR, "never set");
    choose(&controls, "cell", 0, start);
    // Two beats at the untapped 120 BPM is one second.
    let halfway = controls.color_at(&hue, after(start, 0.5));
    assert_eq!(
        halfway,
        controls.color_at(&hue, after(start, 0.5)),
        "a pure function of time"
    );
    assert!(
        halfway[0] == 1. && 0. < halfway[1] && halfway[1] < 1.,
        "on the way from white to red: {halfway:?}"
    );
    assert_eq!(controls.color_at(&hue, after(start, 1.)), [1., 0., 0.]);

    // A value tapped midway fades from where the color is then.
    let again = after(start, 2.);
    choose(&controls, "cell", 2, again);
    let mid = controls.color_at(&hue, after(again, 0.5));
    assert!(
        mid[0] > 0. && mid[2] > 0. && mid[1] == 0.,
        "red to blue: {mid:?}"
    );
    assert_eq!(controls.color_at(&hue, after(again, 1.)), [0., 0., 1.]);
    assert_eq!(controls.playing().lists["cell"], 2);
}

#[test]
fn a_stepping_list_moves_on_at_its_bars() {
    let controls = VenueControls::default();
    let step = |loops| Step { bars: 2, loops };
    install(
        &controls,
        &[],
        &layout(
            &[],
            vec![
                colors("looping", 0., Some(step(true))),
                colors("once", 0., Some(step(false))),
            ],
        ),
    );
    let start = Instant::now();
    let on = |id: &str| controls.playing().lists.get(id).copied();
    // A deck's clock, read at its beats.
    controls.follow(Some(120.));
    let read = |beat| controls.tick(Some((beat, 4.)), start);
    read(1.5);
    assert_eq!(
        on("looping"),
        None,
        "nothing steps before a value is tapped"
    );
    choose(&controls, "looping", 0, start);
    choose(&controls, "once", 1, start);
    // Chosen in the bar from beat 4; two bars later is beat 12.
    for beat in [5., 8., 11.9] {
        read(beat);
        assert_eq!(on("looping"), Some(0), "still in its bars at beat {beat}");
    }
    read(12.);
    assert_eq!(on("looping"), Some(1), "on at the bar");
    assert_eq!(on("once"), Some(2));
    read(20.);
    assert_eq!(on("looping"), Some(2));
    assert_eq!(on("once"), Some(2), "it stays on the last");
    read(28.);
    assert_eq!(on("looping"), Some(0), "back to the first");

    // Recompiling the set keeps the value on.
    install(
        &controls,
        &[],
        &layout(&[], vec![colors("looping", 0., Some(step(true)))]),
    );
    assert_eq!(on("looping"), Some(0));
}

#[test]
fn a_button_node_is_declared_and_read_by_its_pattern() {
    let graph = luma_patterns::ClipGraph::from_json(
        r#"{"version": 3, "nodes": {
            "button1": {"kind": "button", "settings": {"name": "hit", "mode": "toggle"}},
            "curve1": {"kind": "curve", "inputs": {"x": {"node": "button1"}}},
            "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}}"#,
    )
    .unwrap();
    let controls = Arc::new(VenueControls::default());
    install(
        &controls,
        &[PerformPattern {
            graph: graph.clone(),
            ..pattern("chase")
        }],
        &PerformLayout::default(),
    );
    let input = &graph.live_inputs()[0];
    let source = PatternControls::new(controls.clone(), "chase");
    use luma_patterns::ControlSource as _;
    assert_eq!(source.value(input), 0.0);
    let name = perform::node("chase", "button1");
    controls.press(&name, true, Instant::now());
    controls.press(&name, false, Instant::now());
    assert_eq!(source.value(input), 1.0, "a toggle node holds");
    assert_eq!(
        controls.value("hit"),
        0.0,
        "the venue's control of its name is another"
    );
}

#[test]
fn an_undeclared_name_is_momentary() {
    let controls = VenueControls::default();
    controls.press("drop", true, Instant::now());
    assert_eq!(controls.value("drop"), 1.0);
    controls.press("drop", false, Instant::now());
    assert_eq!(controls.value("drop"), 0.0);
}

#[test]
fn taps_set_the_tempo_and_land_on_a_beat() {
    let controls = VenueControls::default();
    let start = Instant::now();
    for tap in 0..4 {
        let at = start + Duration::from_millis(500 * tap);
        controls.press(perform::TAP, true, at);
        controls.press(perform::TAP, false, at + Duration::from_millis(50));
    }
    let last = start + Duration::from_millis(1500);
    let tempo = controls.tempo(last);
    assert!((tempo.bpm - 120.0).abs() < 1e-6, "{}", tempo.bpm);
    assert_eq!(tempo.beat.fract(), 0.0, "the last tap is on a beat");
    let half = controls.tempo(last + Duration::from_millis(250)).beat;
    assert!((half - tempo.beat - 0.5).abs() < 1e-6);
}

#[test]
fn the_first_tap_of_a_run_starts_a_bar() {
    let controls = VenueControls::default();
    let start = Instant::now();
    let tap = |at: Instant| {
        controls.press(perform::TAP, true, at);
        controls.press(perform::TAP, false, at + Duration::from_millis(50));
    };
    tap(start);
    tap(start + Duration::from_millis(500));
    // A pause, then a tap a beat and a bit later: a new run, so the 1.
    let again = start + Duration::from_millis(3_200);
    tap(again);
    let beat = controls.tempo(again).beat;
    assert_eq!(beat.rem_euclid(TAPPED_BEATS_PER_BAR), 0.0, "{beat}");
    tap(again + Duration::from_millis(500));
    let next = controls.tempo(again + Duration::from_millis(500)).beat;
    assert_eq!(next - beat, 1.0, "a later tap is the next beat");
}

/// A pattern whose pad, a toggle, starts on `start` ("beat" or "bar"),
/// at once on and off.
fn quantised(id: &str, mode: &str, start: &str) -> PerformPattern {
    let mut pattern = faded(id, mode, 0.0, 0.0, "seconds");
    let node = pattern.graph.pad().unwrap().node;
    pattern
        .graph
        .nodes
        .get_mut(&node)
        .unwrap()
        .settings
        .insert("start".into(), start.into());
    pattern
}

/// The instant of beat `beat` of the tapped tempo, 120 BPM from a
/// first tap at `start`, which is beat 0.
fn beat(start: Instant, beat: f64) -> Instant {
    after(start, beat * 0.5)
}

/// A bank whose tapped tempo starts a bar at `start`.
fn tapped(set: &[PerformPattern], layout: &PerformLayout, start: Instant) -> VenueControls {
    let controls = VenueControls::default();
    install(&controls, set, layout);
    controls.press(perform::TAP, true, start);
    controls.press(perform::TAP, false, start);
    controls
}

/// Whether button `name` is on at `at`, and whether it waits then.
fn state_at(controls: &VenueControls, name: &str, at: Instant) -> (bool, bool) {
    let button = controls.read().buttons[name].at(at);
    (button.on(), button.waiting())
}

#[test]
fn a_press_on_the_beat_opens_at_the_next_and_lets_go_on_one_too() {
    let start = Instant::now();
    let wash = quantised("wash", "toggle", "beat");
    let on = pad(&wash);
    let controls = tapped(&[wash], &PerformLayout::default(), start);
    controls.press(&on, true, beat(start, 2.3));
    assert_eq!(
        state_at(&controls, &on, beat(start, 2.9)),
        (false, true),
        "waits for beat 3"
    );
    assert_eq!(controls.value_at(&on, beat(start, 2.9)), 0.0);
    assert_eq!(state_at(&controls, &on, beat(start, 3.01)), (true, false));
    assert_eq!(controls.value_at(&on, beat(start, 3.01)), 1.0);

    // Its release lands on a beat too: on until beat 6.
    controls.press(&on, true, beat(start, 5.5));
    assert_eq!(state_at(&controls, &on, beat(start, 5.9)), (true, true));
    assert_eq!(controls.value_at(&on, beat(start, 5.9)), 1.0);
    assert_eq!(state_at(&controls, &on, beat(start, 6.01)), (false, false));
    assert_eq!(controls.value_at(&on, beat(start, 6.01)), 0.0);
}

#[test]
fn a_press_on_the_bar_opens_at_the_next_bar_of_the_taps() {
    let start = Instant::now();
    let wash = quantised("wash", "toggle", "bar");
    let on = pad(&wash);
    let controls = tapped(&[wash], &PerformLayout::default(), start);
    controls.press(&on, true, beat(start, 2.3));
    assert!(
        state_at(&controls, &on, beat(start, 3.9)).1,
        "waits for the bar"
    );
    assert_eq!(state_at(&controls, &on, beat(start, 4.01)), (true, false));
}

#[test]
fn a_press_lands_on_the_followed_decks_bars() {
    let start = Instant::now();
    let wash = quantised("wash", "toggle", "bar");
    let on = pad(&wash);
    let controls = tapped(&[wash], &PerformLayout::default(), start);
    // A deck at 240 BPM in bars of 3, read at beat 4.5 of its grid: its
    // next bar is beat 6, a beat and a half, 0.375 s, on.
    controls.follow(Some(240.));
    controls.tick(Some((4.5, 3.)), start);
    controls.press(&on, true, start);
    assert!(state_at(&controls, &on, after(start, 0.37)).1);
    assert_eq!(state_at(&controls, &on, after(start, 0.38)), (true, false));
}

#[test]
fn a_change_still_to_come_is_called_off_by_the_opposite() {
    let start = Instant::now();
    let set = [
        quantised("toggle", "toggle", "beat"),
        quantised("flash", "flash", "beat"),
    ];
    let [toggle, flash] = [&set[0], &set[1]].map(pad);
    let controls = tapped(&set, &PerformLayout::default(), start);
    controls.press(&toggle, true, beat(start, 2.3));
    controls.press(&toggle, true, beat(start, 2.6));
    let (on, waiting) = state_at(&controls, &toggle, beat(start, 3.5));
    assert!(!on && !waiting, "pressed twice before its beat: nothing");

    // A flash let go before its beat still plays for one, so a tap lands.
    controls.press(&flash, true, beat(start, 2.3));
    controls.press(&flash, false, beat(start, 2.4));
    assert_eq!(state_at(&controls, &flash, beat(start, 3.5)), (true, true));
    assert_eq!(
        state_at(&controls, &flash, beat(start, 4.01)),
        (false, false)
    );
}

#[test]
fn a_buttons_turns_on_are_counted_on_the_beats_they_land_on() {
    let start = Instant::now();
    let set = [
        quantised("hit", "flash", "now"),
        quantised("bar", "toggle", "bar"),
    ];
    let [hit, bar] = [&set[0], &set[1]].map(pad);
    let controls = tapped(&set, &PerformLayout::default(), start);
    let near = |presses: luma_patterns::press::Presses, count: u64, beats: &[f64]| {
        presses.count == count
            && presses.beats.len() == beats.len()
            && presses
                .beats
                .iter()
                .zip(beats)
                .all(|(a, b)| (a - b).abs() < 1e-6)
    };
    assert!(near(controls.presses(&hit), 0, &[]), "nobody has pressed");
    // A flash: every press is a turn on, however fast.
    for at in [1.0, 1.25, 1.5] {
        controls.press(&hit, true, beat(start, at));
        controls.press(&hit, false, beat(start, at + 0.1));
    }
    assert!(near(controls.presses(&hit), 3, &[1.0, 1.25, 1.5]));
    // A toggle on the bar: a press that turns it on counts on the bar it
    // waits for, one that turns it off does not count, and one called off
    // before its bar is no press.
    controls.press(&bar, true, beat(start, 5.3));
    assert!(
        near(controls.presses(&bar), 1, &[8.0]),
        "on the bar to come"
    );
    controls.press(&bar, true, beat(start, 9.0));
    controls.press(&bar, true, beat(start, 13.0));
    assert!(near(controls.presses(&bar), 2, &[8.0, 16.0]));
    controls.press(&bar, true, beat(start, 14.0));
    assert!(near(controls.presses(&bar), 1, &[8.0]), "called off");
    // A recompile keeps them.
    install(&controls, &set, &PerformLayout::default());
    assert!(near(controls.presses(&hit), 3, &[1.0, 1.25, 1.5]));
}

#[test]
fn a_quantised_press_releases_its_row_when_it_goes_on() {
    let start = Instant::now();
    let set = [
        quantised("a", "toggle", "beat"),
        quantised("b", "toggle", "bar"),
        quantised("c", "toggle", "beat"),
    ];
    let [a, b, c] = [&set[0], &set[1], &set[2]].map(pad);
    let controls = tapped(&set, &layout(&[&["a", "b", "c"]], Vec::new()), start);
    controls.press(&a, true, beat(start, 0.5));
    controls.press(&b, true, beat(start, 1.5));
    assert!(
        state_at(&controls, &a, beat(start, 3.9)).0,
        "a plays to b's bar"
    );
    assert_eq!(state_at(&controls, &a, beat(start, 4.01)), (false, false));
    assert_eq!(state_at(&controls, &b, beat(start, 4.01)), (true, false));

    // b let go, to its bar; a pressed, then c, both on beat 5.
    controls.press(&b, true, beat(start, 4.5));
    controls.press(&a, true, beat(start, 4.6));
    controls.press(&c, true, beat(start, 4.7));
    let (on, waiting) = state_at(&controls, &a, beat(start, 5.5));
    assert!(
        !on && !waiting,
        "a, still to come when c was pressed, is called off"
    );
    assert_eq!(state_at(&controls, &c, beat(start, 5.01)), (true, false));
    assert_eq!(
        state_at(&controls, &b, beat(start, 5.01)),
        (false, false),
        "b goes as c comes, not at its own bar"
    );
}

/// Each group's lights: front has two, back one, all three.
fn lights() -> HashMap<String, Arc<[String]>> {
    let lights = |ids: &[&str]| ids.iter().map(|id| (*id).to_owned()).collect();
    HashMap::from([
        ("front".to_owned(), lights(&["f1", "f2"])),
        ("back".to_owned(), lights(&["b1"])),
        (ALL.to_owned(), lights(&["f1", "f2", "b1"])),
    ])
}

/// Install `set` laid out by `layout`, the groups' lights as [`lights`].
fn install(controls: &VenueControls, set: &[PerformPattern], layout: &PerformLayout) {
    controls.install(set, layout, lights());
}

/// A group cell `id` on group `group`.
fn grouped(id: &str, group: &str) -> Cell {
    cell(
        id,
        7,
        3,
        CellContent::Group {
            label: String::new(),
            group: group.into(),
        },
    )
}

/// Front and back's group cells, and a list of red, blue and the color
/// below that sets no targets: the base color.
fn stage() -> Vec<Cell> {
    vec![
        grouped("front", "front"),
        grouped("back", "back"),
        cell(
            "base",
            0,
            2,
            CellContent::List {
                label: "Base".into(),
                values: vec![
                    ListValue::Color([1., 0., 0.]),
                    ListValue::Color([0., 0., 1.]),
                    ListValue::Below,
                ],
                fade: 0.,
                unit: TimeUnit::Beats,
                targets: Vec::new(),
                step: None,
            },
        ),
    ]
}

/// Where pattern `id` plays, sorted; none is its own selection.
fn groups(controls: &VenueControls, id: &str) -> Vec<String> {
    controls.groups(id).expect("a pattern of the set")
}

fn names(groups: &[&str]) -> Vec<String> {
    groups.iter().map(|group| (*group).to_owned()).collect()
}

/// The groups chosen.
fn chosen(controls: &VenueControls) -> Vec<String> {
    controls.playing().chosen
}

#[test]
fn a_group_tapped_is_chosen_and_held_acts_on_it_alone() {
    let controls = VenueControls::default();
    let wash = faded("wash", "toggle", 0.0, 0.0, "seconds");
    let own = pad(&wash);
    install(&controls, &[wash], &layout(&[], stage()));
    let at = Instant::now();
    let (front, back) = (perform::cell("front"), perform::cell("back"));
    tap(&controls, &front, at);
    assert_eq!(
        chosen(&controls),
        names(&["front"]),
        "tapped, it stays chosen"
    );

    // Back held while the pad is tapped: the pad plays on back alone, and
    // back's release is no tap.
    controls.press(&back, true, at);
    tap(&controls, &own, at);
    controls.press(&back, false, at);
    assert_eq!(groups(&controls, "wash"), names(&["back"]));
    assert_eq!(
        chosen(&controls),
        names(&["front"]),
        "the choice is as it was"
    );

    // Tapped with front chosen: front too.
    tap(&controls, &own, at);
    assert_eq!(groups(&controls, "wash"), names(&["back", "front"]));

    // Two held at once: their union, and neither release is a tap.
    controls.press(&front, true, at);
    controls.press(&back, true, at);
    tap(&controls, &own, at);
    controls.press(&back, false, at);
    controls.press(&front, false, at);
    assert!(!lit(&controls, &own), "both there already: both leave");
    assert_eq!(chosen(&controls), names(&["front"]));

    tap(&controls, &front, at);
    assert!(chosen(&controls).is_empty(), "tapped again, no longer");
}

#[test]
fn an_effects_groups_follow_the_taps() {
    let controls = VenueControls::default();
    let wash = faded("wash", "toggle", 0.0, 0.0, "seconds");
    let own = pad(&wash);
    install(&controls, &[wash], &layout(&[], stage()));
    let at = Instant::now();
    let choose = |groups: &[&str]| controls.choose(&names(groups));

    // Nothing chosen: on and off where it was made to play.
    tap(&controls, &own, at);
    assert!(lit(&controls, &own));
    assert_eq!(groups(&controls, "wash"), Vec::<String>::new());
    tap(&controls, &own, at);
    assert!(!lit(&controls, &own));

    // Off, front chosen: on there.
    choose(&["front"]);
    tap(&controls, &own, at);
    assert!(lit(&controls, &own));
    assert_eq!(groups(&controls, "wash"), names(&["front"]));
    // On, back chosen: back too.
    choose(&["back"]);
    tap(&controls, &own, at);
    assert_eq!(groups(&controls, "wash"), names(&["back", "front"]));
    // On there already: it leaves front, then back, and goes off.
    choose(&["front"]);
    tap(&controls, &own, at);
    assert_eq!(groups(&controls, "wash"), names(&["back"]));
    assert!(lit(&controls, &own));
    choose(&["back"]);
    tap(&controls, &own, at);
    assert!(!lit(&controls, &own), "left nowhere, it goes off");
    assert_eq!(
        groups(&controls, "wash"),
        names(&["back"]),
        "and fades out where it was"
    );

    // Front and back chosen, one of them new: both.
    choose(&["front"]);
    tap(&controls, &own, at);
    choose(&["back", "front"]);
    tap(&controls, &own, at);
    assert_eq!(groups(&controls, "wash"), names(&["back", "front"]));
    tap(&controls, &own, at);
    assert!(!lit(&controls, &own), "both there: both leave");
}

#[test]
fn a_flash_plays_on_the_groups_chosen_while_held() {
    let controls = VenueControls::default();
    let hit = faded("hit", "flash", 0.0, 0.0, "seconds");
    let own = pad(&hit);
    install(&controls, &[hit], &layout(&[], stage()));
    let at = Instant::now();
    controls.choose(&names(&["front"]));
    controls.press(&own, true, at);
    assert!(lit(&controls, &own));
    assert_eq!(groups(&controls, "hit"), names(&["front"]));
    controls.press(&own, false, at);
    assert!(!lit(&controls, &own));
}

#[test]
fn a_one_at_a_time_row_takes_the_groups_an_effect_comes_on_at() {
    let controls = VenueControls::default();
    let set = [
        faded("a", "toggle", 0.0, 0.0, "seconds"),
        faded("b", "toggle", 0.0, 0.0, "seconds"),
    ];
    let [a, b] = [&set[0], &set[1]].map(pad);
    install(&controls, &set, &layout(&[&["a", "b"]], stage()));
    let at = Instant::now();
    controls.choose(&names(&["back", "front"]));
    tap(&controls, &a, at);
    controls.choose(&names(&["front"]));
    tap(&controls, &b, at);
    assert_eq!(groups(&controls, "a"), names(&["back"]), "b took front");
    assert!(lit(&controls, &a) && lit(&controls, &b));
    controls.choose(&names(&["back"]));
    tap(&controls, &b, at);
    assert!(!lit(&controls, &a), "left nowhere, a goes off");

    // On its own selection, an effect takes the whole row.
    controls.choose(&[]);
    tap(&controls, &a, at);
    assert!(lit(&controls, &a) && !lit(&controls, &b));
}

/// Light `light`'s base color at `at`.
fn base(controls: &VenueControls, light: &str, at: Instant) -> [f64; 3] {
    controls.read().base_at(light, at)
}

#[test]
fn a_color_on_a_list_with_no_targets_is_the_base_color_of_the_groups_chosen() {
    let controls = VenueControls::default();
    let mut layout = layout(&[], stage());
    if let CellContent::List { fade, .. } = &mut layout.pages[0].cells[2].content {
        *fade = 2.;
    }
    install(&controls, &[], &layout);
    let start = Instant::now();
    assert!(!controls.has_base(), "nothing set: no base");
    // Nothing chosen: every light, fading over two beats, a second.
    choose(&controls, "base", 0, start);
    let halfway = base(&controls, "b1", after(start, 0.5));
    assert!(
        halfway[0] == 1. && 0. < halfway[1] && halfway[1] < 1.,
        "white to red: {halfway:?}"
    );
    for light in ["f1", "f2", "b1"] {
        assert_eq!(
            base(&controls, light, after(start, 1.)),
            [1., 0., 0.],
            "{light}"
        );
    }
    // Front chosen: its lights alone.
    let later = after(start, 2.);
    tap(&controls, &perform::cell("front"), later);
    choose(&controls, "base", 1, later);
    let done = after(later, 1.);
    assert_eq!(base(&controls, "f1", done), [0., 0., 1.]);
    assert_eq!(
        base(&controls, "b1", done),
        [1., 0., 0.],
        "back is as it was"
    );
    assert_eq!(controls.playing().lists["base"], 1);

    // The base colors a light and lights nothing.
    let mut frame = UniverseState::default();
    controls.color_base(&mut frame, done);
    let head = &frame.primitives["f1"];
    assert_eq!((head.color, head.dimmer), ([0., 0., 1.], 0.));
}

#[test]
fn select_gives_an_effect_its_own_color_then_the_color_below_again() {
    let controls = VenueControls::default();
    let (restyles, mut restyled) = tokio::sync::mpsc::unbounded_channel();
    controls.restyle_to(restyles);
    let graph = ClipGraph::from_json(
        r#"{"version": 3, "nodes": {
            "hue": {"kind": "control", "settings": {"name": "hue", "kind": "color"}},
            "color1": {"kind": "color", "inputs": {"color": {"node": "hue"}}}}}"#,
    )
    .unwrap();
    let wash = PerformPattern {
        graph: with_pad_at_once(graph),
        blend_mode: BlendMode::Below,
        ..pattern("wash")
    };
    let own = pad(&wash);
    install(
        &controls,
        std::slice::from_ref(&wash),
        &layout(&[], stage()),
    );
    let at = Instant::now();
    let below = |controls: &VenueControls| controls.playing().effects["wash"].below;
    assert!(below(&controls));

    // Select held, the pad chooses the effect instead of playing it.
    controls.press(perform::SELECT, true, at);
    tap(&controls, &own, at);
    controls.press(perform::SELECT, false, at);
    assert!(!lit(&controls, &own), "chosen, not played");
    assert_eq!(controls.playing().selected, names(&["wash"]));
    assert!(!controls.playing().selecting, "a hold is no latch");

    // Blue: its own color, at once, and the base is untouched. Its blend
    // goes to be stored as a normal one; until the set is installed again,
    // the bank plays the blend stored.
    choose(&controls, "base", 1, at);
    assert_eq!(
        controls.color_at(&perform::node("wash", "hue"), at),
        [0., 0., 1.]
    );
    assert_eq!(
        restyled.try_recv().ok(),
        Some(("wash".to_owned(), BlendMode::Replace)),
        "own color, a normal blend, to store"
    );
    assert!(
        below(&controls),
        "the stored blend stands until stored anew"
    );
    assert!(!controls.has_base());
    assert!(
        controls.playing().selected.is_empty(),
        "then no longer chosen"
    );
    assert_eq!(controls.playing().lists.get("base"), None);
    let stored = PerformPattern {
        blend_mode: BlendMode::Replace,
        ..wash.clone()
    };
    install(
        &controls,
        std::slice::from_ref(&stored),
        &layout(&[], stage()),
    );
    assert!(!below(&controls));

    // Its own color again changes no blend: nothing to store.
    tap(&controls, perform::SELECT, at);
    tap(&controls, &own, at);
    choose(&controls, "base", 0, at);
    assert!(restyled.try_recv().is_err(), "already its own");

    // Tapped, Select latches: tap it, the effect, the color below.
    tap(&controls, perform::SELECT, at);
    assert!(controls.playing().selecting);
    tap(&controls, &own, at);
    choose(&controls, "base", 2, at);
    assert_eq!(
        restyled.try_recv().ok(),
        Some(("wash".to_owned(), BlendMode::Below))
    );
    assert!(!controls.playing().selecting, "the latch is spent");
}

#[test]
fn clear_tapped_chooses_nothing_and_held_lets_every_effect_go() {
    let controls = VenueControls::default();
    let set = [
        faded("a", "toggle", 0.0, 0.0, "seconds"),
        faded("b", "toggle", 0.0, 0.0, "seconds"),
    ];
    let [a, b] = [&set[0], &set[1]].map(pad);
    install(&controls, &set, &layout(&[], stage()));
    let at = Instant::now();
    tap(&controls, &a, at);
    tap(&controls, &perform::cell("front"), at);
    tap(&controls, &b, at);
    tap(&controls, perform::SELECT, at);
    tap(&controls, &a, at);
    let playing = controls.playing();
    assert_eq!((playing.chosen.len(), playing.selected.len()), (1, 1));

    tap(&controls, perform::CLEAR, at);
    let playing = controls.playing();
    assert!(playing.chosen.is_empty() && playing.selected.is_empty() && !playing.selecting);
    assert!(
        lit(&controls, &a) && lit(&controls, &b),
        "a tap lets nothing go"
    );

    controls.press(perform::CLEAR, true, at);
    controls.press(perform::CLEAR, false, at + CLEAR_HOLD);
    assert!(!lit(&controls, &a) && !lit(&controls, &b), "held, all go");
}

#[test]
fn a_number_list_sets_its_targets_and_a_lists_own_control_taps_the_next() {
    let controls = VenueControls::default();
    let speeds = CellContent::List {
        label: "Speed".into(),
        values: vec![ListValue::Number(0.25), ListValue::Number(1.)],
        fade: 1.,
        unit: TimeUnit::Beats,
        targets: vec![
            ControlTarget::Level {
                pattern_id: "red".into(),
            },
            ControlTarget::Named {
                name: "drop".into(),
            },
        ],
        step: None,
    };
    install(
        &controls,
        &[pattern("red")],
        &layout(&[], vec![cell("speeds", 0, 1, speeds)]),
    );
    let at = Instant::now();
    let next = perform::cell("speeds");
    tap(&controls, &next, at);
    assert_eq!(controls.value("drop"), 0.25, "none on: the first");
    assert_eq!(controls.value(&perform::level("red")), 0.25);
    tap(&controls, &next, at);
    assert_eq!(controls.value("drop"), 1.);
    tap(&controls, &next, at);
    assert_eq!(controls.value("drop"), 0.25, "after the last, the first");
    assert_eq!(controls.playing().lists["speeds"], 0);
}

#[test]
fn a_recompile_keeps_where_each_effect_plays_and_what_is_chosen() {
    let controls = VenueControls::default();
    let wash = faded("wash", "toggle", 0.0, 0.0, "seconds");
    let own = pad(&wash);
    install(
        &controls,
        std::slice::from_ref(&wash),
        &layout(&[], stage()),
    );
    let at = Instant::now();
    tap(&controls, &perform::cell("front"), at);
    tap(&controls, &own, at);
    install(
        &controls,
        std::slice::from_ref(&wash),
        &layout(&[], stage()),
    );
    assert!(lit(&controls, &own));
    assert_eq!(groups(&controls, "wash"), names(&["front"]));
    assert_eq!(chosen(&controls), names(&["front"]));
    // A group cell gone takes its group from the choice.
    install(&controls, &[wash], &layout(&[], Vec::new()));
    assert!(chosen(&controls).is_empty());
}

/// How control `name` looks at `at`.
fn look(controls: &VenueControls, name: &str, at: Instant) -> Look {
    controls.surface_at(at)[name].clone()
}

#[test]
fn an_effect_is_dim_off_bright_on_and_blinks_while_it_waits() {
    let start = Instant::now();
    let set = [quantised("bar", "toggle", "bar")];
    let controls = tapped(&set, &layout(&[&["bar"]], vec![]), start);
    let bar = pad(&set[0]);
    let off = look(&controls, &bar, beat(start, 1.));
    assert_eq!((off.job, off.on, off.waiting), (Job::Effect, false, false));
    assert_eq!(off.light().bright, false, "dim while off");

    controls.press(&bar, true, beat(start, 1.));
    let waiting = look(&controls, &bar, beat(start, 2.));
    assert!(waiting.waiting);
    assert_eq!(waiting.light().shine, Shine::Blink);

    let on = look(&controls, &bar, beat(start, 4.5));
    assert!(on.on && !on.waiting);
    assert_eq!((on.light().bright, on.light().shine), (true, Shine::Steady));
}

#[test]
fn an_effect_shows_the_color_it_plays() {
    let graph = ClipGraph::from_json(
        r#"{"version": 3, "nodes": {
            "hue": {"kind": "control", "settings": {"name": "hue", "kind": "color"}},
            "color1": {"kind": "color", "inputs": {"color": {"node": "hue"}}}}}"#,
    )
    .unwrap();
    let own = PerformPattern {
        graph: with_pad_at_once(graph.clone()),
        ..pattern("own")
    };
    let below = PerformPattern {
        graph: with_pad_at_once(graph),
        blend_mode: BlendMode::Below,
        ..pattern("below")
    };
    let hit = faded("hit", "flash", 0.0, 0.0, "seconds");
    let controls = VenueControls::default();
    let set = [own.clone(), below.clone(), hit.clone()];
    install(
        &controls,
        &set,
        &layout(&[&[], &["own", "below", "hit"]], stage()),
    );
    let at = Instant::now();
    assert_eq!(
        look(&controls, &pad(&own), at).color,
        UNSET_COLOR,
        "white until set"
    );

    // Its own color as Select gave it.
    tap(&controls, perform::SELECT, at);
    tap(&controls, &pad(&own), at);
    assert!(look(&controls, &pad(&own), at).selected);
    assert_eq!(look(&controls, &pad(&own), at).light().shine, Shine::Pulse);
    choose(&controls, "base", 1, at);
    assert_eq!(look(&controls, &pad(&own), at).color, [0., 0., 1.]);

    // Below: the base color where it plays, here on front alone.
    tap(&controls, &perform::cell("front"), at);
    choose(&controls, "base", 0, at);
    tap(&controls, &pad(&below), at);
    assert_eq!(look(&controls, &pad(&below), at).color, [1., 0., 0.]);

    // A hit is amber, whatever it plays.
    assert_eq!(look(&controls, &pad(&hit), at).color, AMBER);
}

#[test]
fn groups_values_faders_and_utilities_look_as_they_stand() {
    let controls = VenueControls::default();
    let mut extra = stage();
    extra.push(cell("tap", 6, 0, CellContent::Tap));
    extra.push(cell("select", 6, 1, CellContent::Select));
    extra.push(cell("master", 5, 0, CellContent::Master));
    install(&controls, &[], &layout(&[], extra));
    let at = Instant::now();
    let front = perform::cell("front");

    // A group is lit while chosen, and while held.
    assert!(!look(&controls, &front, at).light().bright);
    controls.press(&front, true, at);
    assert!(look(&controls, &front, at).held);
    assert!(look(&controls, &front, at).light().bright);
    controls.press(&front, false, at);
    assert!(look(&controls, &front, at).on, "tapped: chosen");

    // A value shows its color, bright while on; Next while any is.
    let blue = perform::value("base", 1);
    assert_eq!(look(&controls, &blue, at).color, [0., 0., 1.]);
    assert!(!look(&controls, &blue, at).light().bright);
    assert!(!look(&controls, &perform::cell("base"), at).on);
    choose(&controls, "base", 1, at);
    assert!(look(&controls, &blue, at).light().bright);
    assert!(!look(&controls, &perform::value("base", 0), at).on);
    assert!(look(&controls, &perform::cell("base"), at).on);
    // The group shows its base color now.
    assert_eq!(look(&controls, &front, at).color, [0., 0., 1.]);

    // A fader its position; Select lit while on; tap on each beat.
    controls.set(perform::MASTER, 0.25);
    assert_eq!(look(&controls, perform::MASTER, at).value, 0.25);
    tap(&controls, perform::SELECT, at);
    assert!(look(&controls, perform::SELECT, at).light().bright);
    controls.press(perform::TAP, true, at);
    controls.press(perform::TAP, false, at);
    let tapped = look(&controls, perform::TAP, at);
    assert_eq!(
        (tapped.on, tapped.beat),
        (true, Some(0)),
        "a tap starts a bar"
    );
    let later = look(&controls, perform::TAP, beat(at, 1.5));
    assert_eq!((later.on, later.beat), (false, Some(1)));
}

#[test]
fn every_control_a_hand_plays_has_a_look() {
    let controls = VenueControls::default();
    let set = [faded("wash", "toggle", 0.0, 0.0, "seconds")];
    let mut extra = stage();
    extra.push(cell("clear", 6, 0, CellContent::Clear));
    let layout = layout(&[&["wash"]], extra);
    install(&controls, &set, &layout);
    let surface = controls.surface_at(Instant::now());
    for cell in layout.pages.iter().flat_map(|page| &page.cells) {
        for part in perform::parts(cell, &set) {
            assert_eq!(
                surface.contains_key(&part.control),
                part.kind.mode().is_some(),
                "{}",
                part.control
            );
        }
    }
}
