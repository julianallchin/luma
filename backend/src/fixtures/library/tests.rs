use std::collections::HashMap;

use super::*;
use crate::fixtures::engine::generate_dmx;
use crate::models::fixtures::PatchedFixture;
use crate::models::universe::{PrimitiveState, UniverseState};

fn root() -> PathBuf {
    crate::resources::source_fixtures_root().expect("the shipped bundle")
}

fn definition(id: &str) -> Arc<FixtureDefinition> {
    crate::fixtures::definition(&root(), &fixture_path(id)).unwrap()
}

/// A primitive at `dimmer` of the colour `srgb` (linear sRGB) names, which
/// every RGB emitter can make exactly.
fn lit(dimmer: f32, srgb: [f32; 3]) -> PrimitiveState {
    let working = luma_patterns::color_space::from_linear_srgb(srgb.map(f64::from));
    PrimitiveState {
        dimmer,
        color: working.map(|v| v as f32),
        strobe: 0.0,
        position: [0.0, 0.0],
        speed: 0.0,
        aim: None,
    }
}

/// Primitives keyed by the suffix after the fixture id: `""` for the
/// fixture, `":3"` for cell 3.
fn cells(of: impl IntoIterator<Item = (usize, PrimitiveState)>) -> Vec<(String, PrimitiveState)> {
    of.into_iter().map(|(k, p)| (format!(":{k}"), p)).collect()
}

/// What `id` in `mode`, patched at address 1, sends for `primitives`.
fn dmx(id: &str, mode: &str, primitives: &[(String, PrimitiveState)]) -> [u8; 512] {
    dmx_at(id, mode, primitives, 0.0)
}

/// [`dmx`] for the output frame at `t` seconds on the output clock.
fn dmx_at(id: &str, mode: &str, primitives: &[(String, PrimitiveState)], t: f64) -> [u8; 512] {
    let def = definition(id);
    let footprint = def
        .modes
        .iter()
        .find(|m| m.name == mode)
        .unwrap()
        .channels
        .len();
    let fixture = PatchedFixture {
        id: "fx".into(),
        uid: None,
        venue_id: "v".into(),
        universe: 1,
        address: 1,
        num_channels: footprint as i64,
        manufacturer: def.manufacturer.clone(),
        model: def.model.clone(),
        mode_name: mode.into(),
        fixture_path: fixture_path(id),
        label: None,
        address_pinned: false,
    };
    let state = UniverseState {
        primitives: primitives
            .iter()
            .map(|(suffix, p)| (format!("fx{suffix}"), p.clone()))
            .collect(),
        lasers: Vec::new(),
    };
    let definitions = HashMap::from([(fixture.fixture_path.clone(), (*def).clone())]);
    generate_dmx(&state, &[fixture], &definitions, None, 1.0, t)[&1]
}

fn fixture_level(p: PrimitiveState) -> Vec<(String, PrimitiveState)> {
    vec![(String::new(), p)]
}

#[test]
fn a_library_fixture_loads_through_the_seam_once_with_every_shipped_mode() {
    let index = index(&crate::resources::fixture_library_root(Some(&root())).unwrap()).unwrap();
    for id in [
        "claypaky/sharpy",
        "venue/tetra-bar",
        "showtec/sunstrip-active",
        "martin/atomic-3000-led",
        "chauvet/rogue-r2-spot",
        "oppsk/hyt-006-384sl",
        "oppsk/960led-stage-strobe-light",
    ] {
        let def = definition(id);
        assert!(Arc::ptr_eq(&def, &definition(id)), "{id} parsed twice");
        let entry = index.fixtures.iter().find(|e| e.id == id).unwrap();
        let modes: Vec<(String, usize)> = def
            .modes
            .iter()
            .map(|m| (m.name.clone(), m.channels.len()))
            .collect();
        let shipped: Vec<(String, usize)> = entry
            .modes
            .iter()
            .map(|m| (m.name.clone(), m.footprint))
            .collect();
        assert_eq!(modes, shipped, "{id}");
        // One head per canonical cell, in every mode.
        let cells = def.library.as_ref().unwrap().built.cells.len();
        for m in &def.modes {
            let heads = if cells > 1 { cells } else { 0 };
            assert_eq!(m.heads.len(), heads, "{id} {}", m.name);
        }
    }
    assert!(crate::fixtures::definition(&root(), "lib:../../etc/passwd").is_err());
    assert!(crate::fixtures::definition(&root(), "lib:nobody/nothing").is_err());
}

#[test]
fn search_offers_the_library_fixture_in_place_of_the_qlc_file_it_replaces() {
    let catalogue = crate::services::fixtures::catalogue(&root()).unwrap();
    let found = crate::services::fixtures::best_first(&catalogue.entries, "sharpy");
    assert_eq!(found[0].path, "lib:claypaky/sharpy");
    assert!(found
        .iter()
        .all(|e| e.path != "Clay_Paky/Clay-Paky-Sharpy.qxf"));
    let entry = found[0].library.as_ref().unwrap();
    assert!(entry
        .modes
        .iter()
        .any(|m| m.name == "16ch" && m.footprint == 16));
    assert!(!entry.quality.geometry.is_empty());
    // QLC+ files nothing in the library stands for are still offered.
    assert!(catalogue.entries.iter().any(|e| e.library.is_none()));
}

/// Sharpy 16ch: colour wheel 0, shutter 1, dimmer 2, pan 9–10, tilt 11–12.
#[test]
fn sharpy_sends_sixteen_bit_pan_and_tilt_an_open_shutter_and_its_dimmer() {
    const ID: &str = "claypaky/sharpy";
    let def = definition(ID);
    let [pan_travel, tilt_travel] = def.travel();
    let at = |position: [f32; 2]| {
        dmx(
            ID,
            "16ch",
            &fixture_level(PrimitiveState {
                position,
                ..lit(1.0, [1.0; 3])
            }),
        )
    };
    let word = |b: &[u8; 512], o: usize| u16::from(b[o]) << 8 | u16::from(b[o + 1]);
    // Each end of the travel is each end of the channel, and the degrees
    // between are proportional, to the fine byte.
    for (offset, travel, axis) in [(9, pan_travel, 0), (11, tilt_travel, 1)] {
        let pose = |deg: f32| {
            let mut p = [0.0; 2];
            p[axis] = deg;
            word(&at(p), offset)
        };
        assert_eq!(pose(travel[0]), 0);
        assert_eq!(pose(travel[1]), u16::MAX);
        for deg in [travel[0] + 1.0, -33.3, 0.0, 12.7, travel[1] - 1.0] {
            let expected = (deg - travel[0]) / (travel[1] - travel[0]) * 65535.0;
            assert!((f32::from(pose(deg)) - expected).abs() <= 1.0, "{deg}°");
        }
        assert_ne!(pose(0.0) & 0xff, pose(0.1) & 0xff, "the fine byte moves");
    }
    let open = at([0.0, 0.0]);
    let shutter = &def.channels[1];
    let lowered = shutter.lowered.as_ref().unwrap();
    assert_eq!(lowered.param.attr, Attr::Shutter);
    let range = lowered.param.state(State::Open).unwrap();
    assert!(
        (range.dmx[0]..=range.dmx[1]).contains(&open[1]),
        "{}",
        open[1]
    );
    assert_eq!(open[2], 255);
    let dark = dmx(ID, "16ch", &fixture_level(lit(0.0, [1.0; 3])));
    assert_eq!(dark[2], 0);
    // Strobing moves the shutter into its strobe range, faster higher.
    let strobing = |s: f32| {
        dmx(
            ID,
            "16ch",
            &fixture_level(PrimitiveState {
                strobe: s,
                ..lit(1.0, [1.0; 3])
            }),
        )[1]
    };
    let strobe = lowered.param.state(State::Strobe).unwrap();
    for s in [0.2, 0.5] {
        assert!((strobe.dmx[0]..=strobe.dmx[1]).contains(&strobing(s)));
    }
    assert!(strobing(0.2) < strobing(0.5));
    // White asks the wheel for open; a red for a slot past it.
    let white = dmx(ID, "16ch", &fixture_level(lit(1.0, [1.0; 3])))[0];
    let red = dmx(ID, "16ch", &fixture_level(lit(1.0, [0.9, 0.05, 0.05])))[0];
    assert!(white <= 4 && red > 4, "white {white}, red {red}");
}

/// A degree mapping that is not symmetric about home: 0° is not mid-channel.
#[test]
fn an_asymmetric_pan_maps_home_to_its_own_place() {
    let path = spec_path(
        &crate::resources::fixture_library_root(Some(&root())).unwrap(),
        "claypaky/sharpy",
    );
    let mut spec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    spec["control"]["tables"]["pan"] =
        serde_json::json!([{"dmx": [0, 255], "phys": [-270, 360], "unit": "deg"}]);
    let fixture = Fixture::from_json(&spec.to_string()).unwrap();
    let def = lower(&fixture, luma_fixture::build(&fixture).unwrap()).unwrap();
    let byte = |rank: usize, deg: f32| {
        def.channels[9 + rank]
            .lowered
            .as_ref()
            .unwrap()
            .position(deg)
    };
    let word = |deg: f32| f32::from(u16::from(byte(0, deg)) << 8 | u16::from(byte(1, deg)));
    assert!((word(0.0) / 65535.0 - 270.0 / 630.0).abs() < 1e-3);
    assert!(word(0.0) > word(-90.0) && word(90.0) > word(0.0));
    assert_eq!(word(-400.0), 0.0);
    assert_eq!(word(400.0), 65535.0);
}

/// Tetra Bar 24ch: per cell red, green, blue, amber, dimmer, shutter.
#[test]
fn tetra_bar_lands_each_cells_colour_on_its_own_slots() {
    const ID: &str = "venue/tetra-bar";
    let colours = [
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [1.0, 0.25, 0.0],
    ];
    let b = dmx(
        ID,
        "24ch",
        &cells(colours.iter().enumerate().map(|(k, c)| (k, lit(1.0, *c)))),
    );
    let slot = |cell: usize, i: usize| b[cell * 6 + i];
    for cell in 0..4 {
        assert!(slot(cell, 4) > 0, "cell {cell} dimmer");
        assert_eq!(slot(cell, 5), 0, "cell {cell} shutter open");
    }
    assert!(
        slot(0, 0) > 200 && slot(0, 1) < 30 && slot(0, 2) < 30,
        "{:?}",
        &b[..6]
    );
    assert!(
        slot(1, 1) > 200 && slot(1, 0) < 30 && slot(1, 2) < 30,
        "{:?}",
        &b[6..12]
    );
    assert!(
        slot(2, 2) > 200 && slot(2, 0) < 30 && slot(2, 1) < 30,
        "{:?}",
        &b[12..18]
    );
    // An orange is the amber emitter's to make.
    assert!(
        slot(3, 3) > slot(3, 1) && slot(3, 3) > 100,
        "{:?}",
        &b[18..24]
    );
    // One lit cell leaves its neighbours dark.
    let one = dmx(ID, "24ch", &cells([(2, lit(1.0, [0.0, 0.0, 1.0]))]));
    assert!(one[14] > 200 && one[16] > 0);
    assert!(
        one[..12].iter().chain(&one[18..24]).all(|&v| v == 0),
        "{:?}",
        &one[..24]
    );
}

/// HYT-006-384SL: 16 columns of three cells, top RGB, bottom RGB and a white
/// strip between them; 112ch sends every top RGB, then every bottom RGB, then
/// every white.
#[test]
fn oppsk_wall_washer_orders_top_bottom_and_white_cells() {
    const ID: &str = "oppsk/hyt-006-384sl";
    let def = definition(ID);
    let built = &def.library.as_ref().unwrap().built;
    assert_eq!(built.cells.len(), 48);
    let centres = luma_scene::library::cell_centres(built);
    for k in 0..16 {
        let [top, bottom, white] = [3 * k, 3 * k + 1, 3 * k + 2].map(|i| centres[i]);
        let span = top.distance(bottom);
        assert!(span > 0.0);
        assert!(
            white.distance((top + bottom) / 2.0) < span * 0.1,
            "column {k}"
        );
        assert_eq!(
            built.cells[3 * k + 2].emitters[0].colour.as_deref(),
            Some("white")
        );
    }
    // Columns run across the face in order.
    let across = (centres[45] - centres[0]).normalize();
    for k in 1..16 {
        assert!((centres[3 * k] - centres[3 * (k - 1)]).dot(across) > 0.0);
    }
    // Cell 15 is column 5's top, 16 its bottom, 17 its white.
    let b = dmx(
        ID,
        "112ch",
        &cells([
            (15, lit(1.0, [1.0, 0.0, 0.0])),
            (16, lit(1.0, [0.0, 1.0, 0.0])),
            (17, lit(1.0, [1.0, 1.0, 1.0])),
        ]),
    );
    let lit_slots: Vec<usize> = (0..112).filter(|&o| b[o] > 100).collect();
    assert_eq!(lit_slots, [15, 48 + 16, 96 + 5]);
}

// HYT-006-384SL 64ch: slots 0–47 are 16 RGB triples, each driving a column's
// top and bottom cells together; slots 48–63 are the 16 white cells (2, 5, …,
// 47). There is no dimmer or shutter channel.
const HYT: &str = "oppsk/hyt-006-384sl";
const HYT_RGB: std::ops::Range<usize> = 0..48;
const HYT_WHITE: std::ops::Range<usize> = 48..64;

/// Every HYT cell, the white cells (index 2 mod 3) given `white` and the RGB
/// cells `rgb`.
fn hyt(rgb: PrimitiveState, white: PrimitiveState) -> Vec<(String, PrimitiveState)> {
    cells((0..48).map(|i| {
        (
            i,
            if i % 3 == 2 {
                white.clone()
            } else {
                rgb.clone()
            },
        )
    }))
}

/// White asked of a cell with only red, green and blue emitters is made by
/// them; the white emitters elsewhere on the fixture cannot light it.
#[test]
fn white_lights_cells_without_a_white_emitter() {
    let white = lit(1.0, [1.0; 3]);
    let b = dmx(HYT, "64ch", &hyt(white.clone(), white));
    assert!(b[HYT_RGB].iter().all(|&v| v == 255), "{:?}", &b[HYT_RGB]);
    assert!(
        b[HYT_WHITE].iter().all(|&v| v == 255),
        "{:?}",
        &b[HYT_WHITE]
    );
}

/// A fixture-level channel carries what the cells that can emit through it
/// ask for: in 6ch, red is the RGB cells' red and white the white cells'
/// white, not one average of every cell split afterwards.
#[test]
fn a_shared_channel_carries_the_cells_that_can_emit_through_it() {
    let b = dmx(
        HYT,
        "6ch",
        &hyt(lit(1.0, [1.0, 0.0, 0.0]), lit(1.0, [1.0; 3])),
    );
    // dimmer, shutter, red, green, blue, white
    assert_eq!([b[2], b[3], b[4], b[5]], [255, 0, 0, 255], "{:?}", &b[..6]);
}

/// Dimmer reaches the light on a mode without a dimmer channel: dark at 0,
/// brighter as it rises.
#[test]
fn dimmer_dims_a_mode_without_a_dimmer_channel() {
    let at = |dimmer: f32| {
        let p = lit(dimmer, [1.0, 0.5, 0.25]);
        dmx(HYT, "64ch", &hyt(p.clone(), p))
    };
    let (dark, half, full) = (at(0.0), at(0.5), at(1.0));
    assert!(dark[..64].iter().all(|&v| v == 0), "{:?}", &dark[..64]);
    for slot in 0..64 {
        assert!(half[slot] <= full[slot], "slot {slot}");
    }
    assert!(
        (0..64).any(|slot| half[slot] < full[slot]),
        "half {:?} full {:?}",
        &half[..64],
        &full[..64]
    );
}

/// Cells that disagree about strobing each get what they asked for: over a
/// second of output frames a strobing cell goes dark and lights again, and a
/// steady one never changes.
#[test]
fn cells_that_disagree_on_strobe_each_strobe_or_hold() {
    // Red, which every RGB cell makes on its own, so only the strobe is tested.
    let steady = lit(1.0, [1.0, 0.0, 0.0]);
    let strobing = PrimitiveState {
        strobe: 0.5,
        ..steady.clone()
    };
    // Column 0 (red slot 0) strobes; every other cell holds.
    let primitives = cells((0..48).map(|i| {
        (
            i,
            if i < 3 {
                strobing.clone()
            } else {
                steady.clone()
            },
        )
    }));
    let frames: Vec<[u8; 512]> = (0..44)
        .map(|n| dmx_at(HYT, "64ch", &primitives, f64::from(n) / 44.0))
        .collect();
    let seen = |slot: usize| -> Vec<u8> { frames.iter().map(|b| b[slot]).collect() };
    let strobing = seen(0);
    assert!(
        strobing.contains(&0) && strobing.iter().any(|&v| v > 0),
        "strobing slot 0: {strobing:?}"
    );
    for slot in [3, 45] {
        let s = seen(slot);
        assert!(
            s.iter().all(|&v| v == s[0]) && s[0] > 0,
            "steady slot {slot}: {s:?}"
        );
    }
}

/// Atomic 3000 LED 14ch: slots 0–8 drive the strobe tube (cell 0), 9–13 the
/// Aura RGB backlight (cell 1).
#[test]
fn atomic_aura_channels_drive_cell_one_only() {
    const ID: &str = "martin/atomic-3000-led";
    let tube = dmx(
        ID,
        "14ch",
        &cells([(0, lit(1.0, [1.0; 3])), (1, lit(0.0, [0.0; 3]))]),
    );
    let aura = dmx(
        ID,
        "14ch",
        &cells([(0, lit(0.0, [0.0; 3])), (1, lit(1.0, [0.0, 0.0, 1.0]))]),
    );
    assert_eq!(tube[0], 255);
    assert!(tube[9..13].iter().all(|&v| v == 0), "{:?}", &tube[..14]);
    assert_eq!(aura[0], 0);
    assert!(aura[9] > 0);
    assert!(
        aura[12] > 200 && aura[10] < 30 && aura[11] < 30,
        "{:?}",
        &aura[..14]
    );
    // The Aura's colour presets stay home: the fixture mixes its own.
    assert_eq!(tube[13], aura[13]);
}

/// A channel over a range of cells reads their mean: 4ch drives cells 0–7's
/// red, green and blue together, so half red and half blue is both.
#[test]
fn a_ranged_channel_reads_the_mean_of_its_cells() {
    const ID: &str = "oppsk/960led-stage-strobe-light";
    let [red, blue] = [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
    let mixed = dmx(
        ID,
        "4ch",
        &cells((0..8).map(|k| (k, lit(1.0, if k < 4 { red } else { blue })))),
    );
    let one = dmx(ID, "4ch", &cells((0..8).map(|k| (k, lit(1.0, red)))));
    assert!(one[0] > 200 && one[2] == 0, "{:?}", &one[..4]);
    assert!(mixed[0] > 100 && mixed[2] > 100, "{:?}", &mixed[..4]);
}

/// Each head aims where its own cell asks: with every cell at a different
/// pan and tilt, every pan and tilt byte is what it would be with all cells
/// at the angle of the first cell it covers (the fixture's own pan reads cell
/// 0).
#[test]
fn every_head_aims_where_its_cell_asks() {
    let library = crate::resources::fixture_library_root(Some(&root())).unwrap();
    let mut failures = Vec::new();
    for entry in &index(&library).unwrap().fixtures {
        let def = definition(&entry.id);
        let count = def.library.as_ref().unwrap().built.cells.len().max(1);
        let at = |c: usize| PrimitiveState {
            position: [3.0 * c as f32 - 20.0, 2.0 * c as f32 - 15.0],
            ..lit(1.0, [1.0; 3])
        };
        let each: Vec<(String, PrimitiveState)> = if count > 1 {
            cells((0..count).map(|c| (c, at(c))))
        } else {
            fixture_level(at(0))
        };
        for mode in &def.modes {
            let positions: Vec<(usize, &Lowered)> = mode
                .channels
                .iter()
                .filter_map(|mc| {
                    let lowered = def
                        .channels
                        .iter()
                        .find(|c| c.name == mc.name)?
                        .lowered
                        .as_deref()?;
                    lowered
                        .param
                        .attr
                        .is_position()
                        .then_some((mc.number as usize, lowered))
                })
                .collect();
            if positions.is_empty() {
                continue;
            }
            let sent = dmx(&entry.id, &mode.name, &each);
            for (slot, lowered) in positions {
                let first = lowered.param.cell.map_or(0, |c| c.span()[0] as usize);
                let all: Vec<(String, PrimitiveState)> = if count > 1 {
                    cells((0..count).map(|c| (c, at(first))))
                } else {
                    fixture_level(at(first))
                };
                let want = dmx(&entry.id, &mode.name, &all)[slot];
                if sent[slot] != want {
                    failures.push(format!(
                        "{} {} slot {slot}: sent {}, cell {first} alone sends {want}",
                        entry.id, mode.name, sent[slot]
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
