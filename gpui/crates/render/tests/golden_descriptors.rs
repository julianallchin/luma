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
    assert_eq!(canonical["camera"]["position"][0], 5.5);
    assert_eq!(canonical["camera"]["position"][2], 5.5);
    assert!(canonical["render"].get("legacyShadowEye").is_none());
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
    assert_eq!(value["schema"], "luma.renderer-frame/1");
    assert_eq!(value["image"], "single-mover-1.370.png");
    assert_eq!(value["outputSize"], serde_json::json!([1600, 1000]));
    assert_eq!(value["subframes"], DEFAULT_SUBFRAMES);
    assert_eq!(value["timeSeconds"], 1.37);
    assert_eq!(
        value["scene"]["camera"]["target"],
        serde_json::json!([0.0, 1.5, 0.0])
    );

    let definitions = value["definitions"].as_object().unwrap();
    assert_eq!(
        definitions.len(),
        2,
        "only referenced definitions belong in a frame"
    );
    assert!(definitions.contains_key("golden/moving-head.qxf"));
    assert!(definitions.contains_key("golden/hazer.qxf"));
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
