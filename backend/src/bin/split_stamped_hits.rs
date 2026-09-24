//! One-time conversion: stamped events leave `every`
//! (docs/specs/clip-forms.md). Short-lived: delete after it has run.
//!
//! A clip with `every = {"type": "events"}` becomes one clip per hit:
//!
//! - A sparkle (all are at coverage 1) becomes Washes. At each moment the
//!   old clip shows the brightest live hit, so the span is split where the
//!   brightest hit changes, and each part is a Wash with `every = 0` whose
//!   brightness is that hit's curve over the part.
//! - A chase becomes one chase per hit with `every = 0` and the same travel.
//!
//! Layouts are tried per clip, and the first whose full-score render
//! matches the old one is kept. `life`: each clip covers its hit's life
//! only. `tiled`: the clips also cover the dark time between hits (a dark
//! tail, and a dark lead-in for a Wash), as the old clip did. `lead-in`
//! (chase only): tiled, and a Wash at brightness 0 covers the dark time
//! before the first hit. A chase whose hits overlap cannot match: one clip
//! took the brightest stroke per head, and separate clips do not.
//!
//! Opens the database with `open_app_db_at`, so the change log and the upload
//! queue record every write. Writes only with `--apply`, in one transaction.
//!
//! ```text
//! cargo +1.97.1 run --release --bin split_stamped_hits -- --app-dir DIR \
//!     [--storage-root DIR] [--apply]
//! ```
use luma_lib::{
    database::local::{
        database::open_app_db_at,
        scores::rows::{load_score, save_score},
    },
    eval::{Arena, Scene, Scope},
    models::universe::{PrimitiveState, UniverseState},
    storage::StorageRoot,
};
use luma_patterns::{Clip, Events, Key, Keyframes, Score, Segment, Value};
use sqlx::Row;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Below this per-channel difference a conversion is exact.
const EXACT: f64 = 2e-6;
/// Evenly spaced samples per clip, besides the edges.
const SAMPLES: usize = 64;
/// Beats on either side of an edge to sample.
const AROUND: f64 = 0.01;
/// Samples per interval when looking for the brightest hit.
const SCAN: usize = 2000;

type Inputs = BTreeMap<String, Value>;

// ---------------------------------------------------------------------------
// Curves in beats

/// A piecewise curve over absolute beats, in `Keyframes` terms.
#[derive(Clone, Debug)]
struct Piece {
    points: Vec<(f64, f64)>,
    segments: Vec<Segment>,
}

fn cubic(c: [[f64; 2]; 4], t: f64) -> [f64; 2] {
    let s = 1.0 - t;
    std::array::from_fn(|k| {
        s * s * s * c[0][k]
            + 3.0 * s * s * t * c[1][k]
            + 3.0 * s * t * t * c[2][k]
            + t * t * t * c[3][k]
    })
}
/// The Bézier parameter at `x`, as the engine solves it.
fn parameter(c: [[f64; 2]; 4], x: f64) -> f64 {
    if x <= c[0][0] {
        return 0.0;
    }
    if x >= c[3][0] {
        return 1.0;
    }
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..80 {
        let mid = (low + high) * 0.5;
        if cubic(c, mid)[0] < x {
            low = mid;
        } else {
            high = mid;
        }
    }
    (low + high) * 0.5
}
fn lerp(a: [f64; 2], b: [f64; 2], t: f64) -> [f64; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}
/// Split a cubic at `t`: the left and right control polygons.
fn split(c: [[f64; 2]; 4], t: f64) -> ([[f64; 2]; 4], [[f64; 2]; 4]) {
    let ab = lerp(c[0], c[1], t);
    let bc = lerp(c[1], c[2], t);
    let cd = lerp(c[2], c[3], t);
    let abc = lerp(ab, bc, t);
    let bcd = lerp(bc, cd, t);
    let mid = lerp(abc, bcd, t);
    ([c[0], ab, abc, mid], [mid, bcd, cd, c[3]])
}

impl Piece {
    /// Hit `k` of a sparkle: `curve` over `[t, t + life)`, 0 outside it.
    fn hit(curve: &Keyframes, t: f64, life: f64, from: f64, to: f64) -> Self {
        let number = |key: &Key| match key {
            Key::Number(v) => *v,
            Key::Color(_) => unreachable!("a brightness curve is a number curve"),
        };
        let map = |x: f64| t + x * life;
        let mut points: Vec<(f64, f64)> = curve
            .points
            .iter()
            .map(|(x, key)| (map(*x), number(key)))
            .collect();
        let mut segments: Vec<Segment> = if curve.segments.is_empty() {
            vec![Segment::Linear; points.len() - 1]
        } else {
            curve
                .segments
                .iter()
                .map(|segment| match segment {
                    Segment::Bezier { control1, control2 } => Segment::Bezier {
                        control1: [map(control1[0]), control1[1]],
                        control2: [map(control2[0]), control2[1]],
                    },
                    other => *other,
                })
                .collect()
        };
        if curve.points[0].0 > 0.0 {
            points.insert(0, (t, points[0].1));
            segments.insert(0, Segment::Hold);
        }
        let last = points[points.len() - 1];
        if last.0 < t + life {
            points.push((t + life, last.1));
            segments.push(Segment::Hold);
        }
        // Dark before the hit and after its life.
        if from < t {
            points.insert(0, (from, 0.0));
            segments.insert(0, Segment::Hold);
        }
        points.push((to.max(t + life) + 1.0, 0.0));
        segments.push(Segment::Step);
        Self { points, segments }
    }

    fn controls(&self, i: usize) -> Option<[[f64; 2]; 4]> {
        match self.segments[i] {
            Segment::Bezier { control1, control2 } => Some([
                [self.points[i].0, self.points[i].1],
                control1,
                control2,
                [self.points[i + 1].0, self.points[i + 1].1],
            ]),
            _ => None,
        }
    }

    /// The segment holding `x`: the last point at or before it.
    fn segment_at(&self, x: f64) -> usize {
        self.points
            .partition_point(|(px, _)| *px <= x)
            .saturating_sub(1)
            .min(self.points.len() - 2)
    }

    /// Cut to `[from, to]` and map it onto progress 0–1.
    fn crop(&self, from: f64, to: f64) -> Result<Keyframes, String> {
        let first = self.points[0].0;
        let last = self.points[self.points.len() - 1].0;
        if from < first || to > last || to <= from {
            return Err(format!("crop {from}..{to} outside {first}..{last}"));
        }
        let mut points: Vec<(f64, f64)> = Vec::new();
        let mut segments: Vec<Segment> = Vec::new();
        // The start: a new point inside segment `i`, and its right part.
        let i = self.segment_at(from);
        let (x0, y0) = self.points[i];
        let (x1, y1) = self.points[i + 1];
        if from == x0 {
            points.push((x0, y0));
            segments.push(self.segments[i]);
        } else {
            match self.segments[i] {
                Segment::Hold => {
                    points.push((from, y0));
                    segments.push(Segment::Hold);
                }
                Segment::Step => {
                    points.push((from, y1));
                    segments.push(Segment::Step);
                }
                Segment::Linear => {
                    points.push((from, y0 + (y1 - y0) * (from - x0) / (x1 - x0)));
                    segments.push(Segment::Linear);
                }
                Segment::Bezier { .. } => {
                    let c = self.controls(i).unwrap();
                    let (_, right) = split(c, parameter(c, from));
                    points.push((from, right[0][1]));
                    segments.push(Segment::Bezier {
                        control1: right[1],
                        control2: right[2],
                    });
                }
            }
        }
        // Whole points inside.
        for j in i + 1..self.points.len() {
            let (x, y) = self.points[j];
            if x >= to {
                break;
            }
            points.push((x, y));
            segments.push(self.segments[j]);
        }
        // The end: cut the last segment, which runs to point `e`, at `to`.
        let j = points.len() - 1;
        let e = self.points.partition_point(|(x, _)| *x < to);
        let (ex, ey) = self.points[e];
        let end_value = if ex == to {
            ey
        } else {
            let (sx, sy) = points[j];
            match segments[j] {
                Segment::Hold => sy,
                Segment::Step => ey,
                Segment::Linear => sy + (ey - sy) * (to - sx) / (ex - sx),
                Segment::Bezier { control1, control2 } => {
                    let c = [[sx, sy], control1, control2, [ex, ey]];
                    let (left, _) = split(c, parameter(c, to));
                    segments[j] = Segment::Bezier {
                        control1: left[1],
                        control2: left[2],
                    };
                    left[3][1]
                }
            }
        };
        points.push((to, end_value));
        let span = to - from;
        let u = |x: f64| ((x - from) / span).clamp(0.0, 1.0);
        let n = points.len();
        let keyframes = Keyframes {
            points: points
                .iter()
                .enumerate()
                .map(|(i, (x, y))| {
                    let x = if i == 0 {
                        0.0
                    } else if i == n - 1 {
                        1.0
                    } else {
                        u(*x)
                    };
                    (x, Key::Number(*y))
                })
                .collect(),
            segments: segments
                .into_iter()
                .map(|segment| match segment {
                    Segment::Bezier { control1, control2 } => Segment::Bezier {
                        control1: [u(control1[0]), control1[1]],
                        control2: [u(control2[0]), control2[1]],
                    },
                    other => other,
                })
                .collect(),
        };
        keyframes.validate().map_err(|e| e.to_string())?;
        Ok(keyframes)
    }
}

// ---------------------------------------------------------------------------
// Splitting

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    Life,
    Tiled,
    /// Tiled, and a chase's dark time before its first hit is a dark Wash.
    LeadIn,
}

/// One new clip: its span in beats and its inputs.
struct Part {
    start: f64,
    end: f64,
    /// The hit it plays; `None` for a dark lead-in.
    hit: Option<usize>,
    graph: &'static str,
    inputs: Inputs,
}

fn beats(value: &Value, what: &str) -> Result<f64, String> {
    match value {
        Value::Beats(v) => Ok(*v),
        other => Err(format!("{what} is {other:?}, not plain beats")),
    }
}

fn stamps(inputs: &Inputs) -> Result<Vec<f64>, String> {
    match &inputs["every"] {
        Value::Events(Events::Beats { times }) => Ok(times.as_slice().to_vec()),
        other => Err(format!("every is {other:?}, not stamped beats")),
    }
}

/// Who owns each moment of a sparkle at coverage 1: the brightest live hit.
/// Ties keep the current owner. `None` is a moment with no live hit.
fn owners(
    curve: &Keyframes,
    times: &[f64],
    life: f64,
    span: f64,
) -> Vec<(Option<usize>, f64, f64)> {
    let value = |k: usize, t: f64| {
        let age = (t - times[k]) / life;
        (0.0..1.0)
            .contains(&age)
            .then(|| curve.sample(age.clamp(0.0, 1.0))[0])
    };
    let pick = |t: f64, current: Option<usize>| -> Option<usize> {
        let live: Vec<(usize, f64)> = (0..times.len())
            .filter_map(|k| value(k, t).map(|v| (k, v)))
            .collect();
        let best = live
            .iter()
            .map(|(_, v)| *v)
            .fold(f64::NEG_INFINITY, f64::max);
        if live.is_empty() {
            return None;
        }
        if let Some(c) = current {
            if live.iter().any(|(k, v)| *k == c && *v == best) {
                return Some(c);
            }
        }
        live.iter().rev().find(|(_, v)| *v == best).map(|(k, _)| *k)
    };
    let mut edges: Vec<f64> = vec![0.0, span];
    for t in times {
        edges.extend([*t, t + life]);
    }
    edges.retain(|e| (0.0..=span).contains(e));
    edges.sort_by(f64::total_cmp);
    edges.dedup();
    let mut runs: Vec<(Option<usize>, f64, f64)> = Vec::new();
    let mut push = |owner: Option<usize>, a: f64, b: f64| {
        if b <= a {
            return;
        }
        match runs.last_mut() {
            Some(last) if last.0 == owner && last.2 == a => last.2 = b,
            _ => runs.push((owner, a, b)),
        }
    };
    let mut current = None;
    for pair in edges.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let mut at = a;
        current = pick(a, current);
        let mut from = a;
        for step in 1..=SCAN {
            let t = a + (b - a) * step as f64 / SCAN as f64;
            if t >= b {
                break;
            }
            let next = pick(t, current);
            if next != current {
                // Bisect the switch between `at` and `t`.
                let (mut low, mut high) = (at, t);
                for _ in 0..80 {
                    let mid = (low + high) * 0.5;
                    if pick(mid, current) == current {
                        low = mid;
                    } else {
                        high = mid;
                    }
                }
                push(current, from, high);
                from = high;
                current = next;
            }
            at = t;
        }
        push(current, from, b);
    }
    runs
}

/// A sparkle at coverage 1 as Washes.
fn washes(sparkle: &Inputs, span: f64, layout: Layout) -> Result<Vec<Part>, String> {
    if sparkle.get("coverage") != Some(&Value::Proportion(1.0)) {
        return Err("coverage is not a fixed 1".into());
    }
    let times = stamps(sparkle)?;
    let life = beats(&sparkle["duration"], "duration")?;
    if life <= 0.0 {
        return Err("duration is not positive".into());
    }
    let Value::Hit(curve) = &sparkle["brightness"] else {
        return Err("brightness is not a hit curve".into());
    };
    if matches!(sparkle["alpha"], Value::Time(_)) {
        return Err("alpha is a time curve".into());
    }
    let mut runs = owners(curve, &times, life, span);
    if layout != Layout::Life {
        // Dark time goes to the hit before it, or the first hit.
        let first = runs.iter().find_map(|r| r.0);
        let mut previous = first;
        for run in &mut runs {
            match run.0 {
                Some(owner) => previous = Some(owner),
                None => run.0 = previous,
            }
        }
        let mut merged: Vec<(Option<usize>, f64, f64)> = Vec::new();
        for run in runs {
            match merged.last_mut() {
                Some(last) if last.0 == run.0 && last.2 == run.1 => last.2 = run.2,
                _ => merged.push(run),
            }
        }
        runs = merged;
    }
    let mut parts = Vec::new();
    for (owner, a, b) in runs {
        let Some(k) = owner else { continue };
        let piece = Piece::hit(curve, times[k], life, a.min(times[k]), b);
        let brightness = piece.crop(a, b)?;
        parts.push(Part {
            start: a,
            end: b,
            hit: Some(k),
            graph: "color.constant@1",
            inputs: BTreeMap::from([
                ("color".into(), sparkle["color"].clone()),
                ("brightness".into(), Value::Hit(brightness)),
                ("every".into(), Value::Beats(0.0)),
                ("alpha".into(), sparkle["alpha"].clone()),
            ]),
        });
    }
    Ok(parts)
}

/// A chase as one chase per hit.
fn chases(chase: &Inputs, span: f64, layout: Layout) -> Result<Vec<Part>, String> {
    let times = stamps(chase)?;
    let travel = beats(&chase["travel"], "travel")?;
    if travel <= 0.0 {
        return Err("travel is not positive".into());
    }
    if matches!(chase["alpha"], Value::Time(_)) {
        return Err("alpha is a time curve".into());
    }
    let mut parts = Vec::new();
    // The old clip was dark before its first hit. In the tiled layout a
    // Wash at brightness 0 keeps that darkness, which matters under
    // `multiply` and `replace`.
    if layout == Layout::LeadIn && times[0] > 0.0 {
        parts.push(Part {
            start: 0.0,
            end: times[0].min(span),
            hit: None,
            graph: "color.constant@1",
            inputs: BTreeMap::from([
                ("color".into(), chase["color"].clone()),
                ("brightness".into(), Value::Proportion(0.0)),
                ("every".into(), Value::Beats(0.0)),
                ("alpha".into(), Value::Proportion(1.0)),
            ]),
        });
    }
    for (k, t) in times.iter().enumerate() {
        if *t >= span {
            continue;
        }
        let end = match layout {
            Layout::Life => (t + travel).min(span),
            Layout::Tiled | Layout::LeadIn => times.get(k + 1).copied().unwrap_or(span).min(span),
        };
        if end <= *t {
            continue;
        }
        let mut inputs = chase.clone();
        inputs.insert("every".into(), Value::Beats(0.0));
        parts.push(Part {
            start: *t,
            end,
            hit: Some(k),
            graph: "color.chase@1",
            inputs,
        });
    }
    Ok(parts)
}

fn split_clip(clip: &Clip, layout: Layout) -> Result<Vec<Part>, String> {
    match clip.graph.as_str() {
        "color.sparkle@1" => washes(&clip.inputs, clip.duration, layout),
        "color.chase@1" => chases(&clip.inputs, clip.duration, layout),
        other => Err(format!("unexpected form {other}")),
    }
}

/// The score with `key` replaced by its parts.
fn replaced(score: &Score, key: &str, parts: &[Part]) -> Score {
    let mut out = score.clone();
    let old = out.clips.remove(key).expect("clip in score");
    for (n, part) in parts.iter().enumerate() {
        let mut clip = old.clone();
        clip.graph = part.graph.into();
        clip.start = old.start + part.start;
        clip.duration = part.end - part.start;
        clip.inputs = part.inputs.clone();
        out.clips.insert(format!("{key}-{:02}", n + 1), clip);
    }
    out
}

// ---------------------------------------------------------------------------
// Render check

struct Found {
    id: String,
    score_id: String,
    key: String,
    graph: String,
    hits: usize,
}

struct Chosen {
    layout: Layout,
    parts: Vec<Part>,
    inputs_of_old: Inputs,
    span: f64,
    max: f64,
    note: String,
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut app_dir = None;
    let mut apply = false;
    let mut storage = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--app-dir" => app_dir = args.next().map(PathBuf::from),
            "--storage-root" => storage = args.next().map(PathBuf::from),
            "--apply" => apply = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let app_dir = app_dir.ok_or("--app-dir is required")?;
    let storage = StorageRoot::from_path(storage.unwrap_or_else(|| app_dir.clone()));
    let fixtures = luma_lib::headless_host::HostConfig::default().fixtures_root()?;
    let (db, _connections) = open_app_db_at(&app_dir).await?;
    let pool = db.0.clone();
    let err = |e: sqlx::Error| e.to_string();

    let mut found = Vec::new();
    for row in sqlx::query(
        "SELECT id, score_id, graph, inputs_json FROM clips \
         WHERE json_extract(inputs_json, '$.every.type') = 'events' ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .map_err(err)?
    {
        let id: String = row.get("id");
        let score_id: String = row.get("score_id");
        let inputs: Inputs =
            serde_json::from_str(row.get("inputs_json")).map_err(|e| format!("{id}: {e}"))?;
        let key = id
            .strip_prefix(&score_id)
            .and_then(|rest| rest.strip_prefix(':'))
            .ok_or_else(|| format!("{id} is not in {score_id}"))?
            .to_owned();
        found.push(Found {
            hits: stamps(&inputs)?.len(),
            id,
            score_id,
            key,
            graph: row.get("graph"),
        });
    }
    let mut by_score: BTreeMap<&str, Vec<&Found>> = BTreeMap::new();
    for clip in &found {
        by_score.entry(&clip.score_id).or_default().push(clip);
    }
    let library = luma_patterns::standard_library();

    let mut chosen: BTreeMap<String, Chosen> = BTreeMap::new();
    let mut finals: BTreeMap<String, Score> = BTreeMap::new();
    let mut final_diff: Vec<(String, f64)> = Vec::new();
    for (score_id, clips) in &by_score {
        let mut connection = pool.acquire().await.map_err(err)?;
        let score = load_score(&mut connection, score_id).await?;
        drop(connection);
        let track_id: String = sqlx::query_scalar("SELECT track_id FROM scores WHERE id = ?")
            .bind(score_id)
            .fetch_one(&pool)
            .await
            .map_err(err)?;
        let clock = luma_lib::services::tracks::get_track_beats(&pool, &track_id)
            .await?
            .ok_or("the track has no beat grid")?
            .timeline()
            .map_err(|e| e.to_string())?;
        let old_scene = scene(&pool, &storage, &fixtures, score_id, score.clone()).await?;
        let mut all_new = score.clone();
        let mut spans = Vec::new();
        for found in clips {
            let clip = &score.clips[&found.key];
            let mut best: Option<Chosen> = None;
            let layouts: &[Layout] = if clip.graph == "color.chase@1" {
                &[Layout::Life, Layout::Tiled, Layout::LeadIn]
            } else {
                &[Layout::Life, Layout::Tiled]
            };
            for &layout in layouts {
                let parts = split_clip(clip, layout)?;
                let candidate = replaced(&score, &found.key, &parts);
                candidate
                    .validate(&library)
                    .map_err(|e| format!("{}: {e}", found.id))?;
                let seconds = samples(clip, &parts, &clock)?;
                let before =
                    old_scene.try_render(&seconds, Scope::Composite, &mut Arena::default())?;
                let new_scene = scene(&pool, &storage, &fixtures, score_id, candidate).await?;
                let after =
                    new_scene.try_render(&seconds, Scope::Composite, &mut Arena::default())?;
                let (max, at) = worst(&before, &after, &seconds);
                if max >= EXACT && std::env::var_os("SPLIT_DEBUG").is_some() {
                    let beats = sample_beats(clip, &parts)?;
                    for ((x, y), b) in before.iter().zip(&after).zip(&beats) {
                        let d = compare(x, y);
                        if d >= EXACT {
                            println!(
                                "    {} {layout:?} beat {:.4}: {d:e}",
                                found.key,
                                b - clip.start
                            );
                        }
                    }
                }
                let note = if max < EXACT {
                    String::new()
                } else {
                    format!("largest at {at:.4}s")
                };
                let better = best.as_ref().is_none_or(|b| b.max >= EXACT && max < b.max);
                if better {
                    best = Some(Chosen {
                        layout,
                        parts,
                        inputs_of_old: clip.inputs.clone(),
                        span: clip.duration,
                        max,
                        note,
                    });
                }
                if max < EXACT {
                    break;
                }
            }
            let best = best.expect("one layout");
            all_new = replaced(&all_new, &found.key, &best.parts);
            spans.push((found.id.clone(), clip.clone(), best.parts.len()));
            chosen.insert(found.id.clone(), best);
        }
        // All conversions of the score at once.
        all_new
            .validate(&library)
            .map_err(|e| format!("{score_id}: {e}"))?;
        let new_scene = scene(&pool, &storage, &fixtures, score_id, all_new.clone()).await?;
        for (id, clip, _) in &spans {
            let parts = &chosen[id].parts;
            let seconds = samples(clip, parts, &clock)?;
            let before = old_scene.try_render(&seconds, Scope::Composite, &mut Arena::default())?;
            let after = new_scene.try_render(&seconds, Scope::Composite, &mut Arena::default())?;
            let (max, _) = worst(&before, &after, &seconds);
            final_diff.push((id.clone(), max));
        }
        finals.insert(score_id.to_string(), all_new);
    }

    // Report.
    let removed = found.len();
    let created: usize = chosen.values().map(|c| c.parts.len()).sum();
    let hits: usize = found.iter().map(|f| f.hits).sum();
    println!("clips with stamped events: {removed} ({hits} hits)");
    for graph in ["color.sparkle@1", "color.chase@1"] {
        let of: Vec<_> = found.iter().filter(|f| f.graph == graph).collect();
        let parts: usize = of.iter().map(|f| chosen[&f.id].parts.len()).sum();
        let hits: usize = of.iter().map(|f| f.hits).sum();
        println!(
            "  {graph}: {} clips, {hits} hits -> {parts} new clips",
            of.len()
        );
    }
    println!("new clips: {created}");
    let mut layouts: BTreeMap<String, usize> = BTreeMap::new();
    for c in chosen.values() {
        *layouts.entry(format!("{:?}", c.layout)).or_default() += 1;
    }
    println!("layouts: {layouts:?}");
    let mut owners_split = 0;
    for (id, c) in &chosen {
        let hits: BTreeSet<usize> = c.parts.iter().filter_map(|p| p.hit).collect();
        let dark = c.parts.iter().filter(|p| p.hit.is_none()).count();
        if dark > 0 {
            println!("  NOTE {id}: {dark} dark lead-in Wash");
        }
        let found = found.iter().find(|f| &f.id == id).unwrap();
        if hits.len() + dark != c.parts.len() {
            owners_split += 1;
            println!(
                "  NOTE {id}: {} parts for {} hits",
                c.parts.len(),
                hits.len()
            );
        }
        if hits.len() != found.hits {
            println!(
                "  NOTE {id}: {} of {} hits own some time",
                hits.len(),
                found.hits
            );
        }
    }
    println!("clips where a hit has two parts: {owners_split}");
    let (mut whole, mut cut, mut longer, mut lead) = (0, 0, 0, 0);
    for (id, c) in &chosen {
        let clip = &found.iter().find(|f| &f.id == id).unwrap();
        let _ = clip;
        for part in &c.parts {
            let Some(k) = part.hit else { continue };
            let inputs = &c.inputs_of_old;
            let times = stamps(inputs)?;
            let life = match inputs.get("duration") {
                Some(Value::Beats(d)) => *d,
                _ => beats_of(&inputs["travel"]),
            };
            let hit_end = (times[k] + life).min(c.span);
            if part.start < times[k] {
                lead += 1;
            }
            let (a, b) = (part.start.max(times[k]), part.end);
            if (a - times[k]).abs() < 1e-12 && (b - hit_end).abs() < 1e-12 {
                whole += 1;
            } else if b > hit_end {
                longer += 1;
            } else {
                cut += 1;
            }
        }
    }
    println!("hit clips: {whole} span the hit life (to the old clip end), {longer} run on dark after it, {cut} are cut by a brighter hit; {lead} start dark before their hit");
    let exact = chosen.values().filter(|c| c.max < EXACT).count();
    println!("exact per clip (< {EXACT}): {exact} of {removed}");
    for (id, c) in &chosen {
        if c.max >= EXACT {
            println!("  DIFFERENT {id}: {:e} ({:?}, {})", c.max, c.layout, c.note);
        }
    }
    let final_exact = final_diff.iter().filter(|(_, m)| *m < EXACT).count();
    let final_worst = final_diff.iter().map(|(_, m)| *m).fold(0.0, f64::max);
    println!(
        "exact with all conversions at once: {final_exact} of {removed}, largest {final_worst:e}"
    );
    let shortest = chosen
        .values()
        .flat_map(|c| c.parts.iter().map(|p| p.end - p.start))
        .fold(f64::INFINITY, f64::min);
    println!("shortest new clip: {shortest} beats");

    if !apply {
        return Ok(());
    }
    if final_exact != removed {
        println!("applying although some clips differ (listed above)");
    }
    let count = |sql: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&pool)
                .await
                .map_err(|e| e.to_string())
        }
    };
    println!(
        "before: ps_crud {}",
        count("SELECT count(*) FROM ps_crud").await?
    );
    let mut tx = pool.begin().await.map_err(err)?;
    let mut total = luma_lib::database::local::scores::rows::Changed::default();
    for (score_id, score) in &finals {
        let uid: String = sqlx::query_scalar("SELECT uid FROM scores WHERE id = ?")
            .bind(score_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(err)?;
        let changed = save_score(&mut tx, score_id, &uid, score).await?;
        total.inserted += changed.inserted;
        total.updated += changed.updated;
        total.deleted += changed.deleted;
    }
    if total.deleted != removed || total.inserted != created || total.updated != 0 {
        return Err(format!("unexpected writes {total:?}; rolled back"));
    }
    tx.commit().await.map_err(err)?;
    println!("wrote {total:?}");
    println!(
        "after: ps_crud {}",
        count("SELECT count(*) FROM ps_crud").await?
    );
    println!(
        "after: stamped clips {}",
        count(
            "SELECT count(*) FROM clips WHERE json_extract(inputs_json, '$.every.type') = 'events'"
        )
        .await?
    );
    Ok(())
}

/// Sample seconds over the old clip's span: an even grid, both sides of each
/// hit edge and new clip edge, and points inside each new clip.
fn samples(
    clip: &Clip,
    parts: &[Part],
    clock: &luma_patterns::BeatTimeline,
) -> Result<Vec<f32>, String> {
    sample_beats(clip, parts)?
        .iter()
        .map(|b| {
            clock
                .seconds_at(*b)
                .map(|s| s as f32)
                .map_err(|e| e.to_string())
        })
        .collect()
}

fn sample_beats(clip: &Clip, parts: &[Part]) -> Result<Vec<f64>, String> {
    let (start, end) = (clip.start, clip.start + clip.duration);
    let mut beats: Vec<f64> = (0..SAMPLES)
        .map(|i| start + clip.duration * (i as f64 + 0.5) / SAMPLES as f64)
        .collect();
    let times = stamps(&clip.inputs)?;
    let life = match clip.graph.as_str() {
        "color.sparkle@1" => beats_of(&clip.inputs["duration"]),
        _ => beats_of(&clip.inputs["travel"]),
    };
    for t in &times {
        for edge in [start + t, start + t + life] {
            beats.extend([edge - AROUND, edge + AROUND]);
        }
    }
    for part in parts {
        let (a, b) = (start + part.start, start + part.end);
        beats.extend([a - AROUND, a + AROUND, b - AROUND, b + AROUND]);
        for q in [0.25, 0.5, 0.75] {
            beats.push(a + (b - a) * q);
        }
    }
    beats.retain(|b| *b > start && *b < end);
    beats.sort_by(f64::total_cmp);
    beats.dedup();
    Ok(beats)
}

fn beats_of(value: &Value) -> f64 {
    match value {
        Value::Beats(v) => *v,
        _ => 0.0,
    }
}

async fn scene(
    pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    fixtures: &std::path::Path,
    score_id: &str,
    score: Score,
) -> Result<Scene, String> {
    let scene = luma_lib::build_score_scene(pool, storage, fixtures, score_id, Some(score)).await?;
    Ok(Scene::new(scene.annotations))
}

/// The largest channel difference over all frames, and where it is.
fn worst(a: &[UniverseState], b: &[UniverseState], seconds: &[f32]) -> (f64, f32) {
    let mut max = 0.0_f64;
    let mut at = 0.0;
    for ((x, y), s) in a.iter().zip(b).zip(seconds) {
        let d = compare(x, y);
        if d > max {
            max = d;
            at = *s;
        }
    }
    (max, at)
}

fn compare(a: &UniverseState, b: &UniverseState) -> f64 {
    let heads: BTreeSet<&String> = a.primitives.keys().chain(b.primitives.keys()).collect();
    let mut max = 0.0_f64;
    for head in heads {
        let x = channels(a.primitives.get(head));
        let y = channels(b.primitives.get(head));
        for (p, q) in x.iter().zip(y) {
            max = max.max((p - q).abs());
        }
    }
    max
}

/// Emitted light per channel, strobe and aim.
fn channels(state: Option<&PrimitiveState>) -> [f64; 6] {
    let Some(state) = state else {
        return [0.0; 6];
    };
    let light = |c: usize| f64::from(state.color[c]) * f64::from(state.dimmer);
    [
        light(0),
        light(1),
        light(2),
        f64::from(state.strobe),
        f64::from(state.position[0]),
        f64::from(state.position[1]),
    ]
}
