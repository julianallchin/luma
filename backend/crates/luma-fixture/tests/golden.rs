//! The builder's output for the sample specs, pinned: each fixture's box,
//! cell count, a hash of its cell centres and its joints' pivots, all in
//! housing space with every joint at 0, to 0.1 mm.
//!
//! The sample's parity with the Python reference builder is what
//! `fixture-parity` checks; this golden keeps the port from drifting once it
//! is checked. A stale golden fails; `LUMA_UPDATE_GOLDENS=1` rewrites
//! `harness/goldens/fixture-builds.json` for review.

mod common;

use std::fmt::Write;

use glam::Vec3;
use luma_fixture::{build, Built, Fixture};

/// Tenths of a millimetre, so libm's last bits on another platform do not
/// move a line.
fn q(v: f32) -> i64 {
    (f64::from(v) * 1e4).round() as i64
}

fn mm(v: Vec3) -> String {
    let f = |x: f32| {
        let t = q(x);
        format!(
            "{}{}.{}",
            if t < 0 { "-" } else { "" },
            t.abs() / 10,
            t.abs() % 10
        )
    };
    format!("[{}, {}, {}]", f(v.x), f(v.y), f(v.z))
}

/// FNV-1a over the quantized centres, in canonical order.
fn centre_hash(b: &Built) -> String {
    let worlds = b.world_matrices(&[]);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for c in &b.cells {
        let p = c.node.map(|n| worlds[n].transform_point3(c.centre));
        let words = p.map_or([i64::MIN; 3], |p| [q(p.x), q(p.y), q(p.z)]);
        for w in words {
            for byte in w.to_le_bytes() {
                h ^= u64::from(byte);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    format!("{h:016x}")
}

fn line(id: &str, b: &Built) -> String {
    let (lo, hi) = b.bounds().expect("every fixture has geometry");
    let worlds = b.world_matrices(&[]);
    let joints: Vec<String> = b
        .joints
        .iter()
        .map(|j| {
            format!(
                "\"{:?} {} at {}\"",
                j.kind,
                b.nodes[j.node].name,
                mm(worlds[j.node].transform_point3(Vec3::ZERO))
            )
        })
        .collect();
    let mut s = String::new();
    write!(
        s,
        "  {{\"id\": \"{id}\", \"bounds_mm\": [{}, {}], \"cells\": {}, \"centres\": \"{}\", \"joints\": [{}]}}",
        mm(lo),
        mm(hi),
        b.cells.len(),
        centre_hash(b),
        joints.join(", ")
    )
    .unwrap();
    s
}

#[test]
fn sample_builds_match_the_golden() {
    let lines: Vec<String> = common::sample()
        .iter()
        .map(|(id, text)| {
            let f = Fixture::from_json(text).unwrap_or_else(|e| panic!("{id}: {e}"));
            line(id, &build(&f).unwrap_or_else(|e| panic!("{id}: {e}")))
        })
        .collect();
    let actual = format!("[\n{}\n]\n", lines.join(",\n"));
    let path = common::specs_dir().join("../goldens/fixture-builds.json");
    if std::env::var_os("LUMA_UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let golden = std::fs::read_to_string(&path).unwrap_or_default();
    if golden != actual {
        let at = golden
            .lines()
            .zip(actual.lines())
            .position(|(a, b)| a != b)
            .unwrap_or(0);
        panic!(
            "{} is stale at line {}:\n  golden: {}\n  actual: {}\nRegenerate with LUMA_UPDATE_GOLDENS=1, then review and commit the diff.",
            path.display(),
            at + 1,
            golden.lines().nth(at).unwrap_or("<end of file>"),
            actual.lines().nth(at).unwrap_or("<end of file>"),
        );
    }
}
