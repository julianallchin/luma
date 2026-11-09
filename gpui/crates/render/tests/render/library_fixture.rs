//! A library fixture on the stage: drawn from its own build, with its beam
//! leaving its own lens, and turning with pan and tilt the way a bundled
//! (`.qxf`) moving head does at the same angles.

use std::collections::BTreeMap;
use std::sync::Arc;

use luma_render::assets::Library;
use luma_render::scene_desc::{CameraPose, Fixture, PrimitiveState, RenderSettings, Scene};
use luma_render::{build_frame, Frame};
use luma_scene::definition::{Definition, Dimensions, Lens, Mode, Physical};

const LIBRARY: &str = "lib:claypaky/sharpy";
const BUNDLED: &str = "Clay_Paky/Clay-Paky-Sharpy.qxf";
const POSE: ([f32; 3], [f32; 3]) = ([0.5, 1.0, 5.0], [0.2, -0.4, 0.7]);

fn sharpy() -> Arc<luma_fixture::Built> {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../harness/fixture-specs/claypaky/sharpy.luma.json"),
    )
    .unwrap();
    Arc::new(luma_fixture::build(&luma_fixture::Fixture::from_json(&text).unwrap()).unwrap())
}

fn definition(built: Option<Arc<luma_fixture::Built>>) -> Definition {
    Definition {
        built,
        kind: "Moving Head".into(),
        modes: vec![Mode {
            name: "Standard".into(),
            heads: Vec::new(),
        }],
        physical: Some(Physical {
            dimensions: Some(Dimensions {
                width: 403.0,
                height: 473.0,
                depth: 339.0,
            }),
            layout: None,
            lens: Some(Lens {
                degrees_min: 3.8,
                degrees_max: 3.8,
                radius_m: None,
                offset_m: None,
            }),
        }),
    }
}

/// One lit head of `path` at `position` degrees.
fn frame(path: &str, position: [f32; 2]) -> Frame {
    let mut state = BTreeMap::new();
    state.insert(
        "head:0".to_string(),
        PrimitiveState {
            dimmer: 1.0,
            color: [1.0, 1.0, 1.0],
            strobe: 0.0,
            position,
            gobo: 0,
            gobo_rotation: 0.0,
        },
    );
    let scene = Scene {
        id: "library-fixture".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [5.0, 3.0, 5.0],
            target: [0.0, 1.5, 0.0],
        },
        editing: false,
        aim_arrows: false,
        render: RenderSettings::dark_stage(48.0, 0.5),
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
        fixtures: vec![Fixture {
            id: "head".into(),
            fixture_path: path.into(),
            mode_name: "Standard".into(),
            pos: POSE.0,
            rot: POSE.1,
            aim: [0.0; 2],
        }],
        pieces: Vec::new(),
        state,
    };
    let mut definitions = BTreeMap::new();
    definitions.insert(LIBRARY.to_string(), definition(Some(sharpy())));
    definitions.insert(BUNDLED.to_string(), definition(None));
    build_frame(
        &scene,
        &definitions,
        0.0,
        &mut Library::new(luma_scene::meshes_root()),
    )
    .expect("the scene builds")
}

#[test]
fn a_library_mover_points_where_a_bundled_one_does() {
    for position in [
        [0.0, 0.0],
        [60.0, 0.0],
        [0.0, 45.0],
        [-130.0, 70.0],
        [200.0, -40.0],
    ] {
        let library = frame(LIBRARY, position);
        let bundled = frame(BUNDLED, position);
        let [a, b] = [&library, &bundled].map(|f| f.fixture_cones.first().expect("one lit cone"));
        assert!(
            a.direction.dot(b.direction) > 0.999,
            "{position:?}: library {} vs bundled {}",
            a.direction,
            b.direction
        );
    }
}

#[test]
fn a_library_mover_is_drawn_from_its_build_and_its_lens_moves_with_its_head() {
    let built = sharpy();
    let meshes: usize = built.nodes.iter().map(|n| n.meshes.len()).sum();
    let rest = frame(LIBRARY, [0.0, 0.0]);
    let turned = frame(LIBRARY, [90.0, 60.0]);
    let own = |f: &Frame| {
        f.draws
            .iter()
            .filter(|d| {
                d.editor_object == Some(luma_render::frame::EditorObject::Fixture("head".into()))
            })
            .count()
    };
    assert_eq!(own(&rest), meshes);
    let [a, b] = [&rest, &turned].map(|f| f.fixture_cones[0]);
    assert!(
        a.position.distance(b.position) > 0.05,
        "the lens stayed put"
    );
    assert!(a.direction.dot(b.direction) < 0.9, "the beam stayed put");
}
