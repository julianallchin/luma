use std::collections::BTreeSet;
use std::path::PathBuf;

use luma_render::{scene_desc::Scene, Catalogue, DEFAULT_SUBFRAMES};

fn catalogue() -> Catalogue {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens/scenes.json");
    Catalogue::load(&path).expect("golden catalogue should load")
}

#[test]
fn scene_contract_round_trips_in_canonical_form() {
    let catalogue = catalogue();
    let scene = &catalogue.scenes[0];

    let canonical = serde_json::to_value(scene).expect("scene should serialize");
    let restored: Scene =
        serde_json::from_value(canonical.clone()).expect("serialized scene should deserialize");

    assert_eq!(
        serde_json::to_value(restored).unwrap(),
        canonical,
        "the serialized contract must be a stable round trip"
    );
    assert!(canonical["render"].get("goldenShadowEye").is_none());
}

#[test]
fn frame_descriptor_is_deterministic_and_self_contained() {
    let catalogue = catalogue();
    let scene = &catalogue.scenes[0];
    let descriptor = catalogue
        .frame_descriptor(scene, 1.37, DEFAULT_SUBFRAMES)
        .unwrap();

    let first = serde_json::to_vec_pretty(&descriptor).unwrap();
    let second = serde_json::to_vec_pretty(
        &catalogue
            .frame_descriptor(scene, 1.37, DEFAULT_SUBFRAMES)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first, second);

    let value: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(value["subframes"], DEFAULT_SUBFRAMES);
    assert_eq!(value["timeSeconds"], 1.37);
    let carried: Scene = serde_json::from_value(value["scene"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&carried.camera).unwrap(),
        serde_json::to_value(&scene.camera).unwrap(),
        "a frame carries the scene's own camera"
    );

    let definitions: BTreeSet<&str> = value["definitions"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let referenced: BTreeSet<&str> = scene
        .fixtures
        .iter()
        .map(|fixture| fixture.fixture_path.as_str())
        .collect();
    assert_eq!(
        definitions, referenced,
        "a frame carries exactly the definitions its fixtures reference"
    );
}

#[test]
fn descriptor_rejects_an_unresolved_fixture_definition() {
    let mut catalogue = catalogue();
    let mut scene = catalogue.scenes.remove(0);
    scene.fixtures[0].fixture_path = "missing/fixture.qxf".into();

    let error = catalogue
        .frame_descriptor(&scene, 0.0, DEFAULT_SUBFRAMES)
        .unwrap_err();
    assert!(error.to_string().contains("missing/fixture.qxf"));
}
