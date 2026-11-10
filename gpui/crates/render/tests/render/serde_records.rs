//! The stored shapes of the venue and fixture records, read back and written
//! again.
//!
//! A venue's `environment` and `haze` columns, a golden scene's pieces and
//! fixture definitions are data that outlives the code: a column written by
//! last month's build must read in this one, and a record this build writes
//! must be the one the next build reads. So every tracked file and every
//! stored record shape is deserialized and serialized again, and the result
//! must be the input — byte for byte for the column records, and after the
//! goldens' own canonicalization (keys sorted, pretty-printed) for the JSON
//! files.

use std::collections::BTreeMap;
use std::path::Path;

use luma_render::scene_desc::Scene;
use luma_render::Catalogue;
use luma_scene::repo_root;
use luma_scene::{
    definition::Definition,
    environment::{VenueEnvironment, VenueHaze},
};
use serde_json::Value;

/// `value` as a golden's text: keys sorted, pretty-printed.
fn canonical(mut value: Value) -> String {
    value.sort_all_objects();
    serde_json::to_string_pretty(&value).expect("a value serializes")
}

fn read(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// `value` through `T` and back, as the text `T` writes, canonicalized.
///
/// Through a string rather than [`serde_json::to_value`]: a `Value` widens an
/// `f32` to the nearest `f64`, which is not the number the file holds.
fn through<T: serde::de::DeserializeOwned + serde::Serialize>(value: &Value, what: &str) -> String {
    canonical(written::<T>(value, what))
}

fn written<T: serde::de::DeserializeOwned + serde::Serialize>(value: &Value, what: &str) -> Value {
    let typed: T = serde_json::from_value(value.clone()).unwrap_or_else(|e| panic!("{what}: {e}"));
    let text = serde_json::to_string(&typed).unwrap_or_else(|e| panic!("{what}: {e}"));
    serde_json::from_str(&text).expect("written JSON reads back")
}

/// Where `written` says something `stored` does not: a field `stored` holds
/// with another value, or one `stored` lacks. A field `written` drops is not
/// listed — the hand-written catalogue carries QLC+ blocks the renderer never
/// reads — and numbers compare as the `f32` both sides mean, so `419` and
/// `419.0` agree.
fn disagreements(written: &Value, stored: &Value, at: &str, out: &mut Vec<String>) {
    match (written, stored) {
        (Value::Object(w), Value::Object(s)) => {
            for (key, value) in w {
                match s.get(key) {
                    Some(held) => disagreements(value, held, &format!("{at}.{key}"), out),
                    None => out.push(format!("{at}.{key}: written {value}, not stored")),
                }
            }
        }
        (Value::Array(w), Value::Array(s)) if w.len() == s.len() => {
            for (i, (value, held)) in w.iter().zip(s).enumerate() {
                disagreements(value, held, &format!("{at}[{i}]"), out);
            }
        }
        (Value::Number(w), Value::Number(s)) => {
            let as_f32 = |n: &serde_json::Number| n.as_f64().map(|v| v as f32);
            if as_f32(w) != as_f32(s) {
                out.push(format!("{at}: written {w}, stored {s}"));
            }
        }
        _ if written == stored => {}
        _ => out.push(format!("{at}: written {written}, stored {stored}")),
    }
}

/// Every frame descriptor the renderer goldens carry.
fn descriptors() -> Vec<std::path::PathBuf> {
    let root = repo_root();
    let mut out = Vec::new();
    let dir = "gpui/crates/render/goldens/contracts";
    let entries = std::fs::read_dir(root.join(dir)).unwrap_or_else(|e| panic!("{dir}: {e}"));
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_some_and(|ext| ext == "json") {
            out.push(path);
        }
    }
    out.sort();
    out
}

#[test]
fn the_golden_catalogue_round_trips() {
    let path = repo_root().join("gpui/crates/render/goldens/scenes.json");
    let stored = read(&path);
    let once = written::<Catalogue>(&stored, "scenes.json");
    // The catalogue predates most render dials, so only the records under
    // test are held to "nothing written that is not stored": the pieces and
    // the fixture definitions.
    let mut found = Vec::new();
    for (i, scene) in stored["scenes"]
        .as_array()
        .expect("scenes")
        .iter()
        .enumerate()
    {
        let at = format!("scenes[{i}].pieces");
        disagreements(
            &once["scenes"][i]["pieces"],
            &scene["pieces"],
            &at,
            &mut found,
        );
    }
    let definitions = &stored["definitions"];
    disagreements(&once["definitions"], definitions, "definitions", &mut found);
    assert!(found.is_empty(), "{}", found.join("\n"));
    assert_eq!(
        through::<Catalogue>(&once, "scenes.json"),
        canonical(once),
        "what the catalogue writes must read and write back unchanged"
    );
}

#[test]
fn every_frame_descriptor_round_trips_its_records() {
    let paths = descriptors();
    assert!(!paths.is_empty(), "found no descriptors");
    let (mut appearances, mut generated) = (0, 0);
    for path in paths {
        let what = path.display().to_string();
        let stored = read(&path);
        let scene = &stored["scene"];
        // Older descriptors predate some render dials and piece fields, so the
        // scene as a whole is not held to its file; the records under test are.
        let rewritten = written::<Scene>(scene, &what);
        let pieces = scene["pieces"].as_array().map_or(0, Vec::len);
        let records = ["/render/haze/appearance", "/render/sky", "/render/house"]
            .into_iter()
            .map(String::from)
            .chain((0..pieces).map(|i| format!("/pieces/{i}/procedural")));
        for at in records {
            let Some(held) = scene.pointer(&at).filter(|v| !v.is_null()) else {
                continue;
            };
            if at.ends_with("appearance") {
                appearances += 1;
            } else if at.ends_with("procedural") {
                generated += 1;
            }
            assert_eq!(
                rewritten.pointer(&at).cloned().map(canonical),
                Some(canonical(held.clone())),
                "{what}: scene{at} must read and write back unchanged"
            );
        }
        let definitions = &stored["definitions"];
        assert_eq!(
            through::<BTreeMap<String, Definition>>(definitions, &what),
            canonical(definitions.clone()),
            "{what}: the definitions must read and write back unchanged"
        );
    }
    assert!(appearances > 0, "no descriptor carries a haze appearance");
    assert!(generated > 0, "no descriptor carries a generated piece");
}

/// What a venue row's `environment` column holds: the migration default and
/// the shapes the app writes, including real outdoor rows.
const ENVIRONMENT_RECORDS: &[&str] = &[
    r#"{"mode":"indoor","houseLevel":1.0}"#,
    r#"{"mode":"indoor","houseLevel":0.35,"floor":"carpet"}"#,
    r#"{"mode":"outdoor","sunElevationDeg":-12.318756,"sunAzimuthDeg":250.0,"clouds":"fairWeather","floor":"fineSand"}"#,
    r#"{"mode":"outdoor","sunElevationDeg":-23.392975,"sunAzimuthDeg":250.0,"clouds":"wispy","floor":"gravel"}"#,
    r#"{"mode":"outdoor","sunElevationDeg":35.0,"sunAzimuthDeg":200.0}"#,
];

/// What a venue row's `haze` column holds: the migration default and real rows.
const HAZE_RECORDS: &[&str] = &[
    r#"{"enabled":true,"density":0.24,"appearance":{"cloudiness":0.35,"cloudSize":4.0,"turbulence":0.3,"windSpeed":0.15,"windDirection":30.0}}"#,
    r#"{"enabled":true,"density":0.015,"appearance":{"cloudiness":0.35,"cloudSize":4.0,"turbulence":0.3,"windSpeed":0.15,"windDirection":30.0}}"#,
    r#"{"enabled":true,"density":0.019,"appearance":{"cloudiness":0.53,"cloudSize":3.32,"turbulence":1.0,"windSpeed":0.92,"windDirection":298.4}}"#,
    r#"{"enabled":false,"density":0.044,"appearance":{"cloudiness":0.84,"cloudSize":1.13,"turbulence":0.22,"windSpeed":0.33,"windDirection":281.62}}"#,
];

#[test]
fn venue_column_records_round_trip_byte_for_byte() {
    for record in ENVIRONMENT_RECORDS {
        assert_eq!(
            VenueEnvironment::from((*record).to_string()).to_record(),
            *record
        );
    }
    for record in HAZE_RECORDS {
        assert_eq!(VenueHaze::from((*record).to_string()).to_record(), *record);
    }
}
