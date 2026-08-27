//! The pinned vectors, checked on the `src-tauri` side.
//!
//! `gpui/crates/render/tests/fixture_kinematics_contract.rs` includes the same
//! file and runs the same assertion. Two workspaces, one set of numbers.

#[path = "../contract_vectors.rs"]
mod contract_vectors;

#[test]
fn contract_vectors_hold() {
    contract_vectors::assert_all();
}

#[test]
fn every_vector_is_distinct() {
    // A pinned set is only worth its coverage; a copy-paste that duplicated a
    // case would silently narrow it.
    let mut seen: Vec<[f32; 3]> = Vec::new();
    for v in contract_vectors::CONTRACT_VECTORS {
        assert!(
            !seen.contains(&v.beam_origin),
            "{} pins an origin another case already pins",
            v.name
        );
        seen.push(v.beam_origin);
    }
}
