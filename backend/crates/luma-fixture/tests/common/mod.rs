//! The sample specs in `harness/fixture-specs`, copied from the fixtures
//! library (see the harness README).

use std::path::PathBuf;

use luma_fixture::Fixture;

pub fn specs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../harness/fixture-specs")
}

/// `(id, spec text)` of every sample spec, sorted by id.
pub fn sample() -> Vec<(String, String)> {
    let root = specs_dir();
    let mut out = Vec::new();
    for mfr in std::fs::read_dir(&root).expect("harness/fixture-specs") {
        let mfr = mfr.unwrap().path();
        for f in std::fs::read_dir(&mfr).unwrap() {
            let path = f.unwrap().path();
            let rel = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let id = rel.trim_end_matches(".luma.json").to_string();
            out.push((id, std::fs::read_to_string(&path).unwrap()));
        }
    }
    out.sort();
    out
}

#[allow(dead_code)] // not every test file looks one up
pub fn fixture(id: &str) -> Fixture {
    let text = std::fs::read_to_string(specs_dir().join(format!("{id}.luma.json"))).unwrap();
    Fixture::from_json(&text).unwrap()
}
