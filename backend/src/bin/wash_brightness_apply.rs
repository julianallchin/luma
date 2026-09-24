//! One-time conversion for the Wash brightness change (docs/specs/clip-forms.md,
//! "Wash brightness"). Short-lived: delete after it has run.
//!
//! - Every `color.constant@1` clip gets `brightness = 1` and `every = 0`.
//! - Every `color.sparkle@1` clip with a fixed coverage of 1 and paced events
//!   becomes a `color.constant@1` clip with the same color, brightness, every
//!   and alpha. A shorter `duration` squeezes the hit curve into the start of
//!   each hit. Old and new are rendered on the clip's venue and compared.
//!
//! Opens the database with `open_app_db_at`, so the change log and the upload
//! queue record every write. Writes only with `--apply`, in one transaction.
//!
//! ```text
//! cargo +1.97.1 run --release --bin wash_brightness_apply -- --app-dir DIR [--apply]
//! ```
use luma_lib::{
    database::local::{database::open_app_db_at, scores::rows::load_score},
    eval::{Arena, Scene, Scope},
    models::universe::{PrimitiveState, UniverseState},
    storage::StorageRoot,
};
use luma_patterns::{Keyframes, Score, Segment, Value};
use sqlx::Row;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Below this per-channel difference a conversion is exact.
const EXACT: f64 = 2e-6;
/// Evenly spaced samples per clip, besides the hit boundaries.
const SAMPLES: usize = 48;
/// Beats on either side of a boundary to sample.
const AROUND: f64 = 0.01;

type Inputs = BTreeMap<String, Value>;

/// The Wash inputs for a sparkle at coverage 1, or why there are none.
fn to_wash(sparkle: &Inputs) -> Result<Inputs, String> {
    if sparkle.get("coverage") != Some(&Value::Proportion(1.0)) {
        return Err("coverage is not a fixed 1".into());
    }
    let every = match &sparkle["every"] {
        Value::Beats(every) => *every,
        Value::Events(_) => return Err("stamped events: the Wash has no stamped hits".into()),
        other => {
            return Err(format!(
                "every is {:?}, not plain beats",
                other.source_kind()
            ))
        }
    };
    let Value::Beats(duration) = sparkle["duration"] else {
        return Err("duration is not plain beats".into());
    };
    if every <= 0.0 || duration <= 0.0 {
        return Err("every or duration is not positive".into());
    }
    let share = duration / every;
    let brightness = match &sparkle["brightness"] {
        // Events abut or overlap: some event is always lit.
        plain @ Value::Proportion(_) if share >= 1.0 => plain.clone(),
        // Lit for the first `share` of each hit, dark after it.
        Value::Proportion(level) => Value::Hit(Keyframes::numbers(
            &[[0.0, *level], [share, *level], [1.0, 0.0]],
            &[Segment::Hold, Segment::Step],
        )),
        Value::Hit(curve) if share == 1.0 => Value::Hit(curve.clone()),
        Value::Hit(curve) if share < 1.0 => Value::Hit(squeezed(curve, share)),
        Value::Hit(_) => {
            return Err("duration > every with a hit curve: overlaps keep the maximum".into())
        }
        other if share >= 1.0 => other.clone(),
        other => {
            return Err(format!(
                "duration < every with a {:?} brightness: the sparkle is dark between events",
                other.source_kind()
            ))
        }
    };
    Ok(BTreeMap::from([
        ("color".into(), sparkle["color"].clone()),
        ("brightness".into(), brightness),
        ("every".into(), Value::Beats(every)),
        ("alpha".into(), sparkle["alpha"].clone()),
    ]))
}

/// `curve` played over the first `share` of each hit, then 0.
fn squeezed(curve: &Keyframes, share: f64) -> Keyframes {
    let count = curve.points.len();
    let mut segments: Vec<Segment> = if curve.segments.is_empty() {
        vec![Segment::Linear; count.saturating_sub(1)]
    } else {
        curve.segments.clone()
    };
    for segment in &mut segments {
        if let Segment::Bezier { control1, control2 } = segment {
            control1[0] *= share;
            control2[0] *= share;
        }
    }
    let mut points: Vec<_> = curve
        .points
        .iter()
        .map(|(x, key)| (x * share, *key))
        .collect();
    let (last_x, last) = curve.points[count - 1];
    let last_value = match last {
        luma_patterns::Key::Number(v) => v,
        luma_patterns::Key::Color(_) => unreachable!("a brightness curve is a number curve"),
    };
    if last_value != 0.0 {
        if last_x < 1.0 {
            points.push((share, last));
            segments.push(Segment::Hold);
        }
        points.push((1.0, luma_patterns::Key::Number(0.0)));
        segments.push(Segment::Step);
    }
    Keyframes { points, segments }
}

fn with_hits(wash: &mut Inputs) {
    wash.insert("brightness".into(), Value::Proportion(1.0));
    wash.insert("every".into(), Value::Beats(0.0));
}

struct Converted {
    id: String,
    score_id: String,
    key: String,
    inputs: Inputs,
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

    // Wash rows: add the two inputs.
    let mut washes = Vec::new();
    for row in sqlx::query("SELECT id, inputs_json FROM clips WHERE graph = 'color.constant@1'")
        .fetch_all(&pool)
        .await
        .map_err(err)?
    {
        let id: String = row.get("id");
        let mut inputs: Inputs =
            serde_json::from_str(row.get("inputs_json")).map_err(|e| format!("{id}: {e}"))?;
        let had: BTreeSet<_> = inputs.keys().cloned().collect();
        if had.contains("brightness") || had.contains("every") {
            return Err(format!("{id} already has brightness or every"));
        }
        with_hits(&mut inputs);
        washes.push((id, inputs));
    }

    // Sparkle rows at coverage 1.
    let mut converted = Vec::new();
    let mut kept = Vec::new();
    for row in sqlx::query(
        "SELECT id, score_id, inputs_json FROM clips WHERE graph = 'color.sparkle@1' ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .map_err(err)?
    {
        let id: String = row.get("id");
        let score_id: String = row.get("score_id");
        let inputs: Inputs =
            serde_json::from_str(row.get("inputs_json")).map_err(|e| format!("{id}: {e}"))?;
        if inputs.get("coverage") != Some(&Value::Proportion(1.0)) {
            continue;
        }
        let key = id
            .strip_prefix(&score_id)
            .and_then(|rest| rest.strip_prefix(':'))
            .ok_or_else(|| format!("{id} is not in {score_id}"))?
            .to_owned();
        match to_wash(&inputs) {
            Ok(wash) => {
                let (Value::Beats(e), Value::Beats(d)) = (&inputs["every"], &inputs["duration"])
                else {
                    unreachable!()
                };
                let note = if d == e {
                    "duration = every".to_string()
                } else {
                    format!("duration {d} of every {e}")
                };
                converted.push(Converted {
                    id,
                    score_id,
                    key,
                    inputs: wash,
                    note,
                });
            }
            Err(reason) => kept.push((id, reason)),
        }
    }

    // Render check: each converted clip alone, old and new, on its venue.
    let mut worst = 0.0_f64;
    let mut different = Vec::new();
    let mut by_score: BTreeMap<&str, Vec<&Converted>> = BTreeMap::new();
    for clip in &converted {
        by_score.entry(&clip.score_id).or_default().push(clip);
    }
    for (score_id, clips) in &by_score {
        let mut connection = pool.acquire().await.map_err(err)?;
        let mut score = load_score(&mut connection, score_id).await?;
        drop(connection);
        for clip in score.clips.values_mut() {
            if clip.graph == "color.constant@1" {
                with_hits(&mut clip.inputs);
            }
        }
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
        for converted in clips {
            let old = score.clips[&converted.key].clone();
            let mut new = old.clone();
            new.graph = "color.constant@1".into();
            new.inputs = converted.inputs.clone();
            let Value::Beats(every) = converted.inputs["every"] else {
                unreachable!()
            };
            let Value::Beats(duration) = score.clips[&converted.key].inputs["duration"] else {
                unreachable!()
            };
            let (start, end) = (old.start, old.start + old.duration);
            let mut beats: Vec<f64> = (0..SAMPLES)
                .map(|i| start + old.duration * (i as f64 + 0.5) / SAMPLES as f64)
                .collect();
            let mut k = 0.0;
            while start + k * every < end && k < 256.0 {
                for edge in [start + k * every, start + k * every + duration] {
                    beats.extend([edge - AROUND, edge + AROUND]);
                }
                k += 1.0;
            }
            beats.retain(|b| *b > start && *b < end);
            beats.sort_by(f64::total_cmp);
            beats.dedup();
            let seconds: Vec<f32> = beats
                .iter()
                .map(|b| clock.seconds_at(*b).map(|s| s as f32))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            let render = |clip: luma_patterns::Clip| {
                let mut single = Score::default();
                single.clips.insert(converted.key.clone(), clip);
                render(&pool, &storage, &fixtures, score_id, single, &seconds)
            };
            let before = render(old).await?;
            let after = render(new).await?;
            let mut max = 0.0_f64;
            let mut lit = 0.0_f64;
            for (a, b) in before.iter().zip(&after) {
                max = max.max(compare(a, b));
                lit = lit.max(brightest(a));
            }
            worst = worst.max(max);
            if max >= EXACT {
                different.push((converted.id.clone(), max, converted.note.clone()));
            }
            if lit <= 0.0 {
                different.push((converted.id.clone(), 0.0, "never lit on its venue".into()));
            }
        }
    }

    // New scores validate.
    let library = luma_patterns::standard_library();
    for converted in &converted {
        let mut score = Score::default();
        let mut connection = pool.acquire().await.map_err(err)?;
        let stored = load_score(&mut connection, &converted.score_id).await?;
        let mut clip = stored.clips[&converted.key].clone();
        clip.graph = "color.constant@1".into();
        clip.inputs = converted.inputs.clone();
        score.clips.insert(converted.key.clone(), clip);
        score
            .validate(&library)
            .map_err(|e| format!("{}: {e}", converted.id))?;
    }

    println!("washes given brightness and every: {}", washes.len());
    println!("sparkles at coverage 1 converted: {}", converted.len());
    let mut notes: BTreeMap<&str, usize> = BTreeMap::new();
    for c in &converted {
        let kind = if c.note == "duration = every" {
            "duration = every"
        } else {
            "duration < every"
        };
        *notes.entry(kind).or_default() += 1;
    }
    println!("  by timing: {notes:?}");
    println!("  exact (< {EXACT}): {}", converted.len() - different.len());
    println!("  largest difference: {worst:e}");
    for (id, max, note) in &different {
        println!("  DIFFERENT {id}: {max:e} ({note})");
    }
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, reason) in &kept {
        *reasons.entry(reason).or_default() += 1;
    }
    println!("sparkles at coverage 1 kept: {} {reasons:?}", kept.len());

    if !apply {
        return Ok(());
    }
    if !different.is_empty() {
        return Err("not applying: some conversions differ".into());
    }
    let crud = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM ps_crud")
            .fetch_one(&pool)
            .await
            .map_err(|e| e.to_string())
    };
    println!("before: ps_crud {}", crud().await?);
    let mut tx = pool.begin().await.map_err(err)?;
    for (id, inputs) in &washes {
        let json = serde_json::to_string(inputs).map_err(|e| e.to_string())?;
        sqlx::query("UPDATE clips SET inputs_json = ? WHERE id = ?")
            .bind(json)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
    }
    for converted in &converted {
        let json = serde_json::to_string(&converted.inputs).map_err(|e| e.to_string())?;
        let done = sqlx::query(
            "UPDATE clips SET graph = 'color.constant@1', inputs_json = ? \
             WHERE id = ? AND graph = 'color.sparkle@1'",
        )
        .bind(json)
        .bind(&converted.id)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        if done.rows_affected() != 1 {
            return Err(format!("{} changed under the tool", converted.id));
        }
    }
    tx.commit().await.map_err(err)?;
    println!("after: ps_crud {}", crud().await?);
    for row in sqlx::query("SELECT graph, count(*) AS n FROM clips GROUP BY graph")
        .fetch_all(&pool)
        .await
        .map_err(err)?
    {
        println!(
            "after: {} {}",
            row.get::<String, _>("graph"),
            row.get::<i64, _>("n")
        );
    }
    Ok(())
}

/// One score rendered at `seconds`.
async fn render(
    pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    fixtures: &std::path::Path,
    score_id: &str,
    score: Score,
    seconds: &[f32],
) -> Result<Vec<UniverseState>, String> {
    let scene = luma_lib::build_score_scene(pool, storage, fixtures, score_id, Some(score)).await?;
    Scene::new(scene.annotations).try_render(seconds, Scope::Composite, &mut Arena::default())
}

/// The largest channel difference between two frames.
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

fn brightest(frame: &UniverseState) -> f64 {
    frame
        .primitives
        .values()
        .flat_map(|state| channels(Some(state)))
        .fold(0.0, f64::max)
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
