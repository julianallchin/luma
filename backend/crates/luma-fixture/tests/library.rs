//! The library Luma ships (`resources/fixture-library/<version>/`): every
//! fixture in it parses, builds, and resolves every mode it ships to that
//! mode's footprint, with every cell it addresses inside the build.

use std::path::PathBuf;

use luma_fixture::control::Control;

fn library() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/fixture-library");
    std::fs::read_dir(&root)
        .expect("the repository ships a fixture library")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .max()
        .expect("a version directory")
}

#[test]
fn every_shipped_mode_resolves_onto_the_built_cells() {
    let root = library();
    let index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("index.json")).unwrap()).unwrap();
    let entries = index["fixtures"].as_array().unwrap();
    assert!(!entries.is_empty());
    let mut failures = Vec::new();
    for entry in entries {
        let id = entry["id"].as_str().unwrap();
        let text =
            std::fs::read_to_string(root.join("fixtures").join(format!("{id}.json"))).unwrap();
        let fixture = match luma_fixture::Fixture::from_json(&text) {
            Ok(f) => f,
            Err(e) => {
                failures.push(format!("{id}: {e}"));
                continue;
            }
        };
        let built = match luma_fixture::build(&fixture) {
            Ok(b) => b,
            Err(e) => {
                failures.push(format!("{id}: {e}"));
                continue;
            }
        };
        let control = Control::from_value(fixture.control.as_ref().unwrap()).unwrap();
        for mode in entry["modes"].as_array().unwrap() {
            let name = mode["name"].as_str().unwrap();
            match control.resolve(name) {
                Ok(m) => {
                    assert_eq!(m.slots.len() as u64, mode["footprint"].as_u64().unwrap());
                    for p in &m.params {
                        if let Some(cell) = p.cell {
                            if cell.span()[1] as usize >= built.cells.len().max(1) {
                                failures.push(format!("{id} {name}: cell {cell:?} past the build"));
                            }
                        }
                    }
                }
                Err(e) => failures.push(format!("{id} {name}: {e}")),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every shipped mode drives only emitters its cells' geometry draws: no
/// channel lights a colour the cell has no die for, and no colour name falls
/// outside the vocabulary. The engine and the preview both take a cell's
/// light from where the two agree, so a disagreement is a cell that is dark
/// on the rig.
#[test]
fn every_shipped_mode_drives_only_emitters_its_cells_draw() {
    let root = library();
    let index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("index.json")).unwrap()).unwrap();
    let mut failures = Vec::new();
    for entry in index["fixtures"].as_array().unwrap() {
        let id = entry["id"].as_str().unwrap();
        let text =
            std::fs::read_to_string(root.join("fixtures").join(format!("{id}.json"))).unwrap();
        let fixture = luma_fixture::Fixture::from_json(&text).unwrap();
        let built = luma_fixture::build(&fixture).unwrap();
        let control = Control::from_value(fixture.control.as_ref().unwrap()).unwrap();
        for mode in entry["modes"].as_array().unwrap() {
            let name = mode["name"].as_str().unwrap();
            let resolved = control.resolve(name).unwrap();
            for m in resolved.mismatches(&built) {
                failures.push(format!("{id} {name}: {m:?}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
