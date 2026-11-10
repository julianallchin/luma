//! A profile read, drawn and mapped.

use super::*;
use crate::controls::AMBER;
use crate::models::perform::Cell;

fn note(number: u8) -> Address {
    Address::MidiNote { channel: 0, number }
}

fn light(color: [f64; 3], bright: bool, shine: Shine) -> Light {
    Light {
        color,
        bright,
        shine,
    }
}

const RED: [f64; 3] = [1., 0., 0.];

#[test]
fn a_profile_is_read_from_its_json_and_refused_when_it_cannot_be_drawn() {
    let profile = Profile::parse(
        r#"{"controls": [
              {"address": {"kind": "midi_note", "channel": 0, "number": 81}, "column": 0, "row": 0},
              {"address": {"kind": "midi_cc", "channel": 0, "number": 48}, "column": 0, "row": 8, "fader": true}],
            "color": {"palette": {"0": [0, 0, 0], "5": [255, 0, 0]}},
            "modes": {"blink": {"channel": 1}},
            "dim": 0.2}"#,
    )
    .unwrap();
    assert_eq!(profile.controls.len(), 2);
    assert!(profile.controls[1].fader);
    assert_eq!(profile.modes.blink.unwrap().channel, Some(1));
    assert_eq!(
        Profile::parse("{}").unwrap(),
        Profile::default(),
        "nothing said: echo"
    );

    for (bad, why) in [
        (r#"{"colour": {}}"#, "unknown field"),
        (r#"{"color": {"palette": {}}}"#, "palette"),
        (r#"{"color": {"palette": {"200": [0, 0, 0]}}}"#, "palette"),
        (r#"{"color": {"sysex": "F0 {red} F7"}}"#, "SysEx"),
        (r#"{"color": {"sysex": "00 {r} F7"}}"#, "F0"),
        (r#"{"modes": {"pulse": {"channel": 16}}}"#, "channel"),
        (r#"{"dim": 2}"#, "dim"),
        (
            r#"{"controls": [
                {"address": {"kind": "midi_note", "channel": 0, "number": 1}, "column": 0, "row": 0},
                {"address": {"kind": "midi_note", "channel": 0, "number": 2}, "column": 0, "row": 0}]}"#,
            "share",
        ),
        (
            r#"{"controls": [{"address": {"kind": "osc", "address": "/a"}, "column": 0, "row": 0}]}"#,
            "MIDI",
        ),
    ] {
        let error = Profile::parse(bad).unwrap_err();
        assert!(error.contains(why), "{bad}: {error}");
    }
}

#[test]
fn with_no_color_a_light_is_echoed_lit_or_dark() {
    let echo = Profile::default();
    let lit = echo.show(&note(36), light(RED, true, Shine::Steady), true);
    assert_eq!(lit, Some(vec![0x90, 36, 127]));
    let dim = echo.show(&note(36), light(RED, false, Shine::Steady), true);
    assert_eq!(dim, Some(vec![0x90, 36, 0]), "a dim light is dark");
    let cc = Address::MidiCc {
        channel: 2,
        number: 7,
    };
    assert_eq!(
        echo.show(&cc, light(RED, true, Shine::Steady), true),
        Some(vec![0xb2, 7, 127])
    );
    let osc = Address::Osc {
        address: "/pad".into(),
        arg: 0,
    };
    assert_eq!(echo.show(&osc, light(RED, true, Shine::Steady), true), None);
    // A blink it cannot show is drawn: dark half the time.
    let blink = light(RED, true, Shine::Blink);
    assert_eq!(echo.show(&note(36), blink, true).unwrap()[2], 127);
    assert_eq!(echo.show(&note(36), blink, false).unwrap()[2], 0);
}

fn palette() -> Profile {
    Profile {
        color: Some(ColorOut::Palette(BTreeMap::from([
            (0, [0, 0, 0]),
            (5, [255, 0, 0]),
            (7, [60, 0, 0]),
            (9, [255, 120, 0]),
            (21, [0, 255, 0]),
        ]))),
        modes: Modes {
            steady: None,
            pulse: Some(ModeOut {
                channel: Some(2),
                ..ModeOut::default()
            }),
            blink: Some(ModeOut {
                channel: Some(1),
                ..ModeOut::default()
            }),
        },
        ..Profile::default()
    }
}

#[test]
fn a_palette_device_is_sent_the_nearest_color_on_the_channel_of_its_mode() {
    let profile = palette();
    let at = |light| profile.show(&note(81), light, true).unwrap();
    assert_eq!(at(light(RED, true, Shine::Steady)), vec![0x90, 81, 5]);
    assert_eq!(
        at(light(RED, false, Shine::Steady)),
        vec![0x90, 81, 7],
        "dim: the dim red"
    );
    assert_eq!(at(light(AMBER, true, Shine::Steady)), vec![0x90, 81, 9]);
    assert_eq!(
        at(light([0., 1., 0.], true, Shine::Blink)),
        vec![0x91, 81, 21]
    );
    assert_eq!(at(light(RED, true, Shine::Pulse)), vec![0x92, 81, 5]);
    // The device blinks by itself: never drawn dark here.
    assert_eq!(
        profile.show(&note(81), light(RED, true, Shine::Blink), false),
        Some(vec![0x91, 81, 5])
    );
    let offset = Profile {
        modes: Modes {
            blink: Some(ModeOut {
                offset: 64,
                ..ModeOut::default()
            }),
            ..Modes::default()
        },
        ..palette()
    };
    assert_eq!(
        offset.show(&note(81), light(RED, true, Shine::Blink), true),
        Some(vec![0x90, 81, 69]),
        "a velocity range per mode"
    );
}

#[test]
fn an_rgb_device_is_sent_its_sysex_with_the_color_filled_in() {
    let profile = Profile {
        color: Some(ColorOut::Sysex(
            "F0 00 20 29 02 0C 03 03 {number} {r} {g} {b} F7".into(),
        )),
        ..Profile::default()
    };
    // The red a screen shows as pure red is sent as pure red.
    let screen_red = color_space::from_srgb([1., 0., 0.]);
    let sent = profile
        .show(&note(81), light(screen_red, true, Shine::Steady), true)
        .unwrap();
    assert_eq!(
        sent,
        vec![0xf0, 0x00, 0x20, 0x29, 0x02, 0x0c, 0x03, 0x03, 81, 127, 0, 0, 0xf7]
    );
    // A color past what a screen shows is sent as the screen shows it.
    let deep = profile
        .show(&note(81), light(RED, true, Shine::Steady), true)
        .unwrap();
    let shown = color_space::to_display_srgb(RED).map(|c| (c * 127.).round() as u8);
    assert_eq!(&deep[9..12], &shown);
    let dim = profile
        .show(&note(81), light(RED, false, Shine::Steady), true)
        .unwrap();
    assert!(
        dim[9] > 0 && dim[9] < 127,
        "dim is darker, not dark: {dim:?}"
    );
    let off = profile
        .show(&note(81), light(RED, true, Shine::Blink), false)
        .unwrap();
    assert_eq!(&off[9..12], &[0, 0, 0], "it blinks by being drawn dark");
}

#[test]
fn a_fader_on_a_cc_is_sent_its_position() {
    let look = Look {
        job: Job::Fader,
        on: false,
        waiting: false,
        held: false,
        selected: false,
        color: [1.; 3],
        value: 0.5,
        beat: None,
    };
    let cc = Address::MidiCc {
        channel: 0,
        number: 48,
    };
    assert_eq!(palette().draw(&cc, &look, true), Some(vec![0xb0, 48, 64]));
}

/// Shaped like an Akai APC mini mk2: rgb pads that take a palette velocity
/// and shine by channel (6 steady, 9 pulse, 13 blink), and single-LED
/// buttons on channel 0 that take 0 off, 1 on and 2 blink, whatever color.
const MIXED: &str = r#"{
    "controls": [
        {"address": {"kind": "midi_note", "channel": 0, "number": 0}, "column": 0, "row": 0},
        {"address": {"kind": "midi_note", "channel": 0, "number": 100}, "column": 0, "row": 1, "light": "buttons"},
        {"address": {"kind": "midi_note", "channel": 0, "number": 101}, "column": 1, "row": 1, "light": "plain"}],
    "color": {"palette": {"0": [0, 0, 0], "5": [255, 0, 0], "7": [60, 0, 0], "21": [0, 255, 0]}},
    "modes": {"steady": {"channel": 6}, "pulse": {"channel": 9}, "blink": {"channel": 13}},
    "lights": {
        "buttons": {"modes": {"steady": {"velocity": 1}, "pulse": {"velocity": 2}, "blink": {"velocity": 2}}},
        "plain": {}}
}"#;

#[test]
fn a_control_lights_by_its_own_rule_beside_the_devices() {
    let profile = Profile::parse(MIXED).unwrap();
    let green = [0., 1., 0.];
    let (pad, button, plain) = (note(0), note(100), note(101));

    // A pad: the nearest palette color, on its mode's channel.
    let lit = |shine| light(RED, true, shine);
    assert_eq!(
        profile.show(&pad, lit(Shine::Steady), true),
        Some(vec![0x96, 0, 5])
    );
    assert_eq!(
        profile.show(&pad, lit(Shine::Pulse), true),
        Some(vec![0x99, 0, 5])
    );
    assert_eq!(
        profile.show(&pad, lit(Shine::Blink), false),
        Some(vec![0x9d, 0, 5])
    );
    let dim_pad = profile.show(&pad, light(RED, false, Shine::Steady), true);
    assert_ne!(dim_pad.unwrap()[2], 0, "a dim pad is a dim color");

    // A button: on, blink or off on its own channel, whatever the color.
    let on = profile.show(&button, lit(Shine::Steady), true);
    assert_eq!(on, Some(vec![0x90, 100, 1]));
    assert_eq!(
        profile.show(&button, light(green, true, Shine::Steady), true),
        on,
        "one color"
    );
    assert_eq!(
        profile.show(&button, lit(Shine::Blink), false),
        Some(vec![0x90, 100, 2]),
        "it blinks by itself: never drawn dark"
    );
    assert_eq!(
        profile.show(&button, lit(Shine::Pulse), true),
        Some(vec![0x90, 100, 2])
    );
    for dark in [Shine::Steady, Shine::Blink] {
        assert_eq!(
            profile.show(&button, light(RED, false, dark), true),
            Some(vec![0x90, 100, 0]),
            "dim is dark"
        );
    }

    // A rule is whole: one with nothing in it is echoed, not the palette.
    assert_eq!(
        profile.show(&plain, lit(Shine::Steady), true),
        Some(vec![0x90, 101, 127])
    );

    let again = Profile::parse(&serde_json::to_string(&profile).unwrap()).unwrap();
    assert_eq!(again, profile, "it round-trips");
}

#[test]
fn a_control_naming_no_light_or_a_bad_one_is_refused() {
    let control = |light: &str| {
        format!(
            r#"{{"controls": [{{"address": {{"kind": "midi_note", "channel": 0, "number": 1}}, "column": 0, "row": 0, "light": "{light}"}}],"#
        )
    };
    for (bad, why) in [
        (format!(r#"{} "lights": {{}}}}"#, control("lamps")), "lamps"),
        (
            r#"{"lights": {"buttons": {"modes": {"steady": {"velocity": 200}}}}}"#.into(),
            "\"buttons\"",
        ),
        (
            r#"{"lights": {"buttons": {"dim": 3}}}"#.into(),
            "\"buttons\"",
        ),
        (
            r#"{"lights": {"buttons": {"color": {"palette": {}}}}}"#.into(),
            "\"buttons\"",
        ),
        (
            r#"{"lights": {"buttons": {"colour": {}}}}"#.into(),
            "unknown field",
        ),
    ] {
        let error = Profile::parse(&bad).unwrap_err();
        assert!(error.contains(why), "{bad}: {error}");
    }
}

fn grid(columns: u16, rows: u16, fader_row: Option<u16>) -> Profile {
    let mut controls: Vec<Place> = (0..rows)
        .flat_map(|row| (0..columns).map(move |column| (column, row)))
        .map(|(column, row)| Place {
            address: note((row * columns + column) as u8),
            column,
            row,
            fader: false,
            light: None,
        })
        .collect();
    if let Some(row) = fader_row {
        controls.extend((0..columns).map(|column| Place {
            address: Address::MidiCc {
                channel: 0,
                number: 100 + column as u8,
            },
            column,
            row,
            fader: true,
            light: None,
        }));
    }
    Profile {
        controls,
        ..Profile::default()
    }
}

fn cell(id: &str, column: u16, row: u16, width: u16, height: u16, content: CellContent) -> Cell {
    Cell {
        id: id.into(),
        column,
        row,
        width,
        height,
        content,
    }
}

#[test]
fn a_page_maps_each_cell_onto_the_controls_at_its_places() {
    let wash = PerformPattern {
        id: "wash".into(),
        venue_id: "venue".into(),
        name: "Wash".into(),
        selection: luma_patterns::Selection::All,
        z_index: 0,
        blend_mode: luma_patterns::BlendMode::Replace,
        seed: 1,
        graph: luma_patterns::ClipGraph::from_json(
            r#"{"version": 3, "nodes": {"color1": {"kind": "color"}}}"#,
        )
        .unwrap()
        .with_pad(),
    };
    let list = |id: &str, width, count| {
        cell(
            id,
            2,
            0,
            width,
            1,
            CellContent::List {
                label: String::new(),
                values: vec![crate::models::perform::ListValue::Number(0.5); count],
                fade: 0.,
                unit: luma_patterns::press::TimeUnit::Beats,
                targets: Vec::new(),
                step: None,
            },
        )
    };
    let page = LayoutPage {
        id: "page".into(),
        name: "Main".into(),
        columns: 8,
        rows: 3,
        one_at_a_time: Vec::new(),
        cells: vec![
            // A pattern two places tall: its pad on top, and its level on
            // the fader under it.
            cell(
                "wash",
                0,
                1,
                1,
                2,
                CellContent::Pattern {
                    pattern_id: "wash".into(),
                },
            ),
            list("values", 2, 3),
            cell("tap", 7, 0, 1, 1, CellContent::Tap),
            // On a fader: a button is not mapped there.
            cell("clear", 7, 2, 1, 1, CellContent::Clear),
        ],
    };
    let profile = grid(8, 2, Some(2));
    let bound = profile.map(&page, std::slice::from_ref(&wash));
    let pad = names::pad(&wash).unwrap();
    assert_eq!(
        bound,
        vec![
            (pad, note(8), Mode::Momentary),
            (
                names::level("wash"),
                Address::MidiCc {
                    channel: 0,
                    number: 100
                },
                Mode::Fader
            ),
            (names::value("values", 0), note(2), Mode::Momentary),
            (names::value("values", 1), note(3), Mode::Momentary),
            (names::TAP.to_owned(), note(7), Mode::Momentary),
        ]
    );

    // A list one place big is its Next.
    let single = LayoutPage {
        cells: vec![list("next", 1, 3)],
        ..page
    };
    assert_eq!(
        profile.map(&single, &[]),
        vec![(names::cell("next"), note(2), Mode::Momentary)]
    );
}
