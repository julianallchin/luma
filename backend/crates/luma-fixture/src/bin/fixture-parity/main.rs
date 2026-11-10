//! `fixture-parity`: hold this crate's builder to the Python reference
//! builder over a fixtures checkout.
//!
//! ```text
//! fixture-parity --fixtures ~/github/fixtures [--reference ref.jsonl] [--jobs 4]
//!                [--only <substring>,...] [--shippable] [--report report.txt]
//! ```
//!
//! The reference builder's answers (`reference_dump.py`, run with the
//! checkout's `.venv`) are written to `--reference` the first time and read
//! back after that, so iterating on the Rust side does not rerun Python;
//! delete the file to refresh it. Python's answers are in the spec's space and
//! are carried into housing space by the build's own `housing_from_spec`
//! (the joint pivots then check that transform).
//!
//! Per fixture it compares, posed at rest: the box around each node's meshes
//! and around all of them (1 mm or 0.5% of the fixture's size), the cell
//! count, each cell's centre (1 mm) and emitter count, and each joint's pivot
//! and axis. A fixture passes when all agree. Run it in release: it builds
//! every spec and reports the build times.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use glam::{DMat4, DVec3, Mat4, Vec3};
use luma_fixture::{bounds_at, Built, Fixture};
use serde_json::Value;

const DUMP: &str = include_str!("reference_dump.py");

struct Args {
    fixtures: PathBuf,
    reference: PathBuf,
    jobs: usize,
    only: Option<String>,
    shippable: bool,
    report: Option<PathBuf>,
}

fn args() -> Args {
    let mut a = std::env::args().skip(1);
    let mut fixtures = None;
    let mut reference = None;
    let mut jobs = 4;
    let mut only = None;
    let mut shippable = false;
    let mut report = None;
    while let Some(k) = a.next() {
        match k.as_str() {
            "--fixtures" => fixtures = a.next().map(PathBuf::from),
            "--reference" => reference = a.next().map(PathBuf::from),
            "--jobs" => jobs = a.next().and_then(|v| v.parse().ok()).unwrap_or(4),
            "--only" => only = a.next(),
            "--shippable" => shippable = true,
            "--report" => report = a.next().map(PathBuf::from),
            other => {
                eprintln!("unknown argument {other}\n{}", usage());
                std::process::exit(2);
            }
        }
    }
    let Some(fixtures) = fixtures else {
        eprintln!("{}", usage());
        std::process::exit(2);
    };
    let reference =
        reference.unwrap_or_else(|| std::env::temp_dir().join("fixture-parity-reference.jsonl"));
    Args {
        fixtures,
        reference,
        jobs: jobs.max(1),
        only,
        shippable,
        report,
    }
}

fn usage() -> &'static str {
    "usage: fixture-parity --fixtures <checkout> [--reference <jsonl>] [--jobs N] [--only <substring>] [--shippable] [--report <file>]"
}

/// `<mfr>/<model>` of every spec, sorted.
fn specs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let dir = root.join("fixtures");
    let mut mfrs: Vec<_> = std::fs::read_dir(&dir)
        .expect("a fixtures/ directory")
        .flatten()
        .collect();
    mfrs.sort_by_key(std::fs::DirEntry::file_name);
    for m in mfrs {
        let Ok(models) = std::fs::read_dir(m.path()) else {
            continue;
        };
        for f in models.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if let Some(model) = name.strip_suffix(".luma.json") {
                out.push(format!("{}/{model}", m.file_name().to_string_lossy()));
            }
        }
    }
    out.sort();
    out
}

fn shippable(root: &Path, id: &str) -> bool {
    std::fs::read_to_string(root.join("reviews").join(format!("{id}.json")))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.pointer("/gate/shippable").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn spec_path(root: &Path, id: &str) -> PathBuf {
    root.join("fixtures").join(format!("{id}.luma.json"))
}

/// Run the reference builder over `ids` in `jobs` processes.
fn run_reference(root: &Path, ids: &[String], jobs: usize) -> Vec<String> {
    let python = root.join(".venv/bin/python");
    let builder = root.join("tools/reference_builder");
    let chunks: Vec<Vec<String>> = (0..jobs)
        .map(|j| ids.iter().skip(j).step_by(jobs).cloned().collect())
        .collect();
    let handles: Vec<_> = chunks
        .into_iter()
        .map(|chunk| {
            let (python, builder, root) = (python.clone(), builder.clone(), root.to_path_buf());
            std::thread::spawn(move || {
                let mut child = Command::new(&python)
                    .arg("-c")
                    .arg(DUMP)
                    .env("PYTHONPATH", &builder)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                    .unwrap_or_else(|e| panic!("cannot run {}: {e}", python.display()));
                let mut stdin = child.stdin.take().expect("piped");
                let paths: String = chunk
                    .iter()
                    .map(|id| format!("{}\n", spec_path(&root, id).display()))
                    .collect();
                let writer = std::thread::spawn(move || stdin.write_all(paths.as_bytes()));
                let lines: Vec<String> = BufReader::new(child.stdout.take().expect("piped"))
                    .lines()
                    .map_while(Result::ok)
                    .collect();
                let _ = writer.join();
                let _ = child.wait();
                lines
            })
        })
        .collect();
    handles
        .into_iter()
        .flat_map(|h| h.join().expect("a reference worker"))
        .collect()
}

fn load_reference(a: &Args, ids: &[String]) -> HashMap<String, Value> {
    let mut map = HashMap::new();
    let read = |map: &mut HashMap<String, Value>, text: &str| {
        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if let Some(p) = v.get("path").and_then(Value::as_str) {
                    map.insert(id_of(p), v.clone());
                }
            }
        }
    };
    if let Ok(text) = std::fs::read_to_string(&a.reference) {
        read(&mut map, &text);
    }
    let missing: Vec<String> = ids
        .iter()
        .filter(|id| !map.contains_key(*id))
        .cloned()
        .collect();
    if !missing.is_empty() {
        eprintln!(
            "running the reference builder over {} specs ({} jobs)...",
            missing.len(),
            a.jobs
        );
        let t = Instant::now();
        let lines = run_reference(&a.fixtures, &missing, a.jobs);
        eprintln!("  done in {:.0} s", t.elapsed().as_secs_f64());
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&a.reference)
            .expect("a writable --reference");
        for l in &lines {
            let _ = writeln!(f, "{l}");
        }
        read(&mut map, &lines.join("\n"));
    }
    map
}

fn id_of(path: &str) -> String {
    let p = path.trim_end_matches(".luma.json");
    let mut parts = p.rsplit('/');
    let model = parts.next().unwrap_or_default();
    let mfr = parts.next().unwrap_or_default();
    format!("{mfr}/{model}")
}

fn mat_of(v: &Value) -> DMat4 {
    let a: Vec<f64> = v
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    // Row-major from numpy.
    DMat4::from_cols_array(&[
        a[0], a[4], a[8], a[12], a[1], a[5], a[9], a[13], a[2], a[6], a[10], a[14], a[3], a[7],
        a[11], a[15],
    ])
}

fn vec_of(v: &Value) -> DVec3 {
    let a: Vec<f64> = v
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    DVec3::new(a[0], a[1], a[2])
}

type Aabb = (DVec3, DVec3);

fn aabb_under(m: DMat4, (lo, hi): Aabb) -> Aabb {
    let mut out = (DVec3::splat(f64::MAX), DVec3::splat(f64::MIN));
    for i in 0..8 {
        let c = DVec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        );
        let p = m.transform_point3(c);
        out = (out.0.min(p), out.1.max(p));
    }
    out
}

fn union(a: Option<Aabb>, b: Aabb) -> Aabb {
    a.map_or(b, |a| (a.0.min(b.0), a.1.max(b.1)))
}

fn gap(a: Aabb, b: Aabb) -> f64 {
    (a.0 - b.0).abs().max((a.1 - b.1).abs()).max_element()
}

/// The largest disagreement seen of each kind, metres: node or total box,
/// cell centre, joint pivot.
type Worst = [f64; 3];

/// Every way `built` disagrees with the reference `r`; empty when it agrees.
fn compare(built: &Built, r: &Value, worst_seen: &mut Worst) -> Vec<String> {
    let mut out = Vec::new();
    let c = built.housing_from_spec.as_dmat4();
    let worlds = built.world_matrices(&built.rest_angles());
    let rworlds: Vec<DMat4> = worlds.iter().map(Mat4::as_dmat4).collect();
    let nodes = r["nodes"].as_array().cloned().unwrap_or_default();
    let mut ref_boxes: BTreeMap<String, Aabb> = BTreeMap::new();
    let mut ref_total: Option<Aabb> = None;
    for n in &nodes {
        if let Some(b) = n["aabb"].as_array() {
            let bx = aabb_under(c, (vec_of(&b[0]), vec_of(&b[1])));
            ref_total = Some(union(ref_total, bx));
            ref_boxes.insert(n["name"].as_str().unwrap_or_default().to_string(), bx);
        }
    }
    let total = bounds_at(built, &worlds).map(|(lo, hi)| (lo.as_dvec3(), hi.as_dvec3()));
    let (Some(rt), Some(t)) = (ref_total, total) else {
        out.push("no geometry on one side".into());
        return out;
    };
    let tol = 0.001f64.max(0.005 * (rt.1 - rt.0).max_element());
    let d = gap(rt, t);
    worst_seen[0] = worst_seen[0].max(d);
    if d > tol {
        out.push(format!("total aabb off by {:.1} mm", d * 1000.0));
    }
    let mut mine: BTreeMap<String, Aabb> = BTreeMap::new();
    for (n, w) in built.nodes.iter().zip(&worlds) {
        for m in &n.meshes {
            for p in &m.positions {
                let q = w.transform_point3(Vec3::from(*p)).as_dvec3();
                let e = mine.entry(n.name.clone()).or_insert((q, q));
                *e = (e.0.min(q), e.1.max(q));
            }
        }
    }
    for (name, rb) in &ref_boxes {
        match mine.get(name) {
            None => out.push(format!("node {name}: no geometry here")),
            Some(&b) => {
                let d = gap(*rb, b);
                worst_seen[0] = worst_seen[0].max(d);
                if d > tol {
                    out.push(format!("node {name}: aabb off by {:.1} mm", d * 1000.0));
                }
            }
        }
    }
    for name in mine.keys().filter(|n| !ref_boxes.contains_key(*n)) {
        out.push(format!("node {name}: geometry only here"));
    }
    // Cells.
    let rcells = r["cells"].as_u64().unwrap_or(0) as usize;
    if rcells != built.cells.len() {
        out.push(format!(
            "cells: {} here, {rcells} in the reference",
            built.cells.len()
        ));
    }
    let mut bad_centre = 0;
    let mut bad_count = 0;
    let mut worst = 0.0f64;
    for cell in &built.cells {
        let k = cell.index.to_string();
        let rc = r["cell_centres"]
            .get(&k)
            .map(vec_of)
            .map(|p| c.transform_point3(p));
        let rn = r["cell_emitters"]
            .get(&k)
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        if rn != cell.emitters.len() {
            bad_count += 1;
        }
        let mc = cell
            .node
            .map(|n| rworlds[n].transform_point3(cell.centre.as_dvec3()));
        match (rc, mc) {
            (Some(a), Some(b)) => {
                let d = (a - b).length();
                worst_seen[1] = worst_seen[1].max(d);
                if d > 0.001 {
                    bad_centre += 1;
                    worst = worst.max(d);
                }
            }
            (None, None) => {}
            _ => bad_centre += 1,
        }
    }
    if bad_count > 0 {
        out.push(format!("emitter count differs on {bad_count} cells"));
    }
    if bad_centre > 0 {
        out.push(format!(
            "{bad_centre} cell centres off (worst {:.1} mm)",
            worst * 1000.0
        ));
    }
    // Joints.
    let rjoints: Vec<&Value> = nodes.iter().filter(|n| n["joint"].is_object()).collect();
    if rjoints.len() != built.joints.len() {
        out.push(format!(
            "joints: {} here, {} in the reference",
            built.joints.len(),
            rjoints.len()
        ));
    }
    for rj in rjoints {
        let name = rj["name"].as_str().unwrap_or_default();
        let Some(j) = built
            .joints
            .iter()
            .find(|j| built.nodes[j.node].name == name)
        else {
            out.push(format!("joint {name}: missing here"));
            continue;
        };
        let w = c * mat_of(&rj["world"]);
        let pivot = w.transform_point3(DVec3::ZERO);
        let axis = w
            .transform_vector3(vec_of(&rj["joint"]["axis"]))
            .normalize();
        let mw = rworlds[j.node];
        let mpivot = mw.transform_point3(DVec3::ZERO);
        let maxis = mw.transform_vector3(j.axis.as_dvec3()).normalize();
        worst_seen[2] = worst_seen[2].max((pivot - mpivot).length());
        if (pivot - mpivot).length() > 0.001 {
            out.push(format!(
                "joint {name}: pivot off by {:.1} mm",
                (pivot - mpivot).length() * 1000.0
            ));
        }
        if axis.dot(maxis) < 0.9999 {
            out.push(format!(
                "joint {name}: axis {maxis:.3} here, {axis:.3} in the reference"
            ));
        }
    }
    out
}

#[derive(Default)]
struct Tally {
    pass: usize,
    fail: usize,
    error: usize,
    both_refuse: usize,
    reasons: BTreeMap<String, usize>,
}

fn main() {
    let a = args();
    let mut ids = specs(&a.fixtures);
    if let Some(s) = &a.only {
        ids.retain(|id| s.split(',').any(|s| id.contains(s)));
    }
    let ship: HashMap<String, bool> = ids
        .iter()
        .map(|id| (id.clone(), shippable(&a.fixtures, id)))
        .collect();
    if a.shippable {
        ids.retain(|id| ship[id]);
    }
    let reference = load_reference(&a, &ids);
    let mut all = Tally::default();
    let mut shipped = Tally::default();
    let mut times = Vec::new();
    let mut worst: [(f64, String); 3] = Default::default();
    let mut slowest = (0.0, String::new());
    let mut report = String::new();
    for id in &ids {
        let text = std::fs::read_to_string(spec_path(&a.fixtures, id)).unwrap_or_default();
        let t = Instant::now();
        let built = Fixture::from_json(&text).and_then(|f| luma_fixture::build(&f));
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if ms > slowest.0 {
            slowest = (ms, id.clone());
        }
        times.push(ms);
        let r = reference.get(id);
        let ref_built = r.is_some_and(|r| r["fallback"].is_null() && r.get("crash").is_none());
        let (verdict, reasons): (&str, Vec<String>) = match (&built, r) {
            (_, None) => ("error", vec!["no reference answer".into()]),
            (Err(e), Some(r)) if !ref_built => {
                let why = r["fallback"]
                    .as_str()
                    .or(r["crash"].as_str())
                    .unwrap_or("?")
                    .to_string();
                (
                    "refuse",
                    vec![format!("both refuse: here {e}; reference {why}")],
                )
            }
            (Err(e), Some(_)) => ("error", vec![format!("{e}")]),
            (Ok(_), Some(r)) if !ref_built => {
                let why = r["fallback"]
                    .as_str()
                    .or(r["crash"].as_str())
                    .unwrap_or("?")
                    .to_string();
                (
                    "fail",
                    vec![format!("reference fell back ({why}), built here")],
                )
            }
            (Ok(b), Some(r)) => {
                let mut w = [0.0; 3];
                let d = compare(b, r, &mut w);
                if d.is_empty() {
                    for (k, v) in w.into_iter().enumerate() {
                        if v > worst[k].0 {
                            worst[k] = (v, id.clone());
                        }
                    }
                }
                (if d.is_empty() { "pass" } else { "fail" }, d)
            }
        };
        for tally in [Some(&mut all), ship[id].then_some(&mut shipped)]
            .into_iter()
            .flatten()
        {
            match verdict {
                "pass" => tally.pass += 1,
                "fail" => tally.fail += 1,
                "refuse" => tally.both_refuse += 1,
                _ => tally.error += 1,
            }
            if let Some(first) = reasons.first() {
                let key: String = first
                    .split(" off by")
                    .next()
                    .unwrap_or(first)
                    .chars()
                    .filter(|c| !c.is_ascii_digit())
                    .collect();
                *tally
                    .reasons
                    .entry(format!("{verdict}: {key}"))
                    .or_default() += 1;
            }
        }
        if verdict != "pass" {
            report.push_str(&format!(
                "{verdict} {id}{}\n",
                if ship[id] { " [shippable]" } else { "" }
            ));
            for r in &reasons {
                report.push_str(&format!("    {r}\n"));
            }
        }
    }
    times.sort_by(f64::total_cmp);
    let pct = |q: f64| {
        times
            .get(((times.len() as f64 - 1.0) * q) as usize)
            .copied()
            .unwrap_or(0.0)
    };
    let mut summary = String::new();
    for (name, t) in [("all", &all), ("shippable", &shipped)] {
        let n = t.pass + t.fail + t.error + t.both_refuse;
        summary.push_str(&format!(
            "{name}: {n} specs, {} pass ({:.1}%), {} fail, {} error, {} refused by both\n",
            t.pass,
            100.0 * t.pass as f64 / n.max(1) as f64,
            t.fail,
            t.error,
            t.both_refuse
        ));
        let mut reasons: Vec<_> = t.reasons.iter().collect();
        reasons.sort_by(|a, b| b.1.cmp(a.1));
        for (r, k) in reasons.iter().take(15) {
            summary.push_str(&format!("    {k:5} {r}\n"));
        }
    }
    summary.push_str(&format!(
        "build time per spec (parse + build): median {:.2} ms, p95 {:.2} ms, max {:.2} ms ({})\n",
        pct(0.5),
        pct(0.95),
        pct(1.0),
        slowest.1
    ));
    for (what, (d, id)) in ["node or total box", "cell centre", "joint pivot"]
        .iter()
        .zip(&worst)
    {
        summary.push_str(&format!(
            "worst {what} among passes: {:.3} mm ({id})\n",
            d * 1000.0
        ));
    }
    print!("{summary}");
    if let Some(p) = &a.report {
        std::fs::write(p, format!("{summary}\n{report}")).expect("a writable --report");
    } else {
        print!("{report}");
    }
}
