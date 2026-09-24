//! One-time cleanup: remove the dark padding that the stamped events split
//! (e637b88d) added. Short-lived: delete after it has run.
//!
//! No clip is black only, and a clip does not run dark before or after the
//! part where it lights. For the clips that the split made (created in one
//! write, keys `<old key>-NN`):
//!
//! - A Wash whose brightness is 0 for its whole span is deleted.
//! - A Wash is trimmed to where its brightness curve lights: leading and
//!   trailing segments that hold or step at 0 are cut off, and the curve is
//!   mapped onto the shorter span.
//! - A chase ends when its stroke has fully left: its duration is at most its
//!   travel.
//! - Two chases of one old clip that start less than `DOUBLE` beats apart are
//!   one double-detected hit: the later one is deleted.
//!
//! It also renders every clip in the database alone and lists the clips that
//! emit no light and no strobe (black only), and renders each changed score
//! before and after.
//!
//! Opens the database with `open_app_db_at`, so the change log and the upload
//! queue record every write. Writes only with `--apply`, in one transaction.
//!
//! ```text
//! cargo +1.97.1 run --release --bin trim_dark_padding -- --app-dir DIR \
//!     [--storage-root DIR] [--skip-scan] [--apply --backup PATH]
//! ```
use luma_lib::{
    database::local::{
        database::open_app_db_at,
        scores::rows::{load_score, save_score, Changed},
    },
    eval::{Arena, Scene, Scope},
    models::universe::{PrimitiveState, UniverseState},
    storage::StorageRoot,
};
use luma_patterns::{Clip, Key, Keyframes, Score, Segment, Value};
use sqlx::Row;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// The one write that made the split clips.
const SPLIT_CREATED: &str = "2026-09-24T04:18%";
const SPLIT_CLIPS: usize = 1012;
/// Hits of one old chase closer than this are one hit detected twice.
const DOUBLE: f64 = 0.05;
/// Render sample step over changed spans, in beats.
const STEP: f64 = 1.0 / 64.0;
/// Samples per clip for the black-only scan.
const SCAN: usize = 256;
/// A difference below this is no change.
const SAME: f64 = 2e-6;

fn number(key: &Key) -> f64 {
    match key {
        Key::Number(v) => *v,
        Key::Color(_) => unreachable!("brightness is a number curve"),
    }
}

/// Whether segment `i` of a number curve is 0 over its whole length.
fn dark_segment(curve: &Keyframes, i: usize) -> bool {
    let a = number(&curve.points[i].1);
    let b = number(&curve.points[i + 1].1);
    match curve.segments.get(i).copied().unwrap_or_default() {
        Segment::Hold => a == 0.0,
        Segment::Step => b == 0.0,
        Segment::Linear => a == 0.0 && b == 0.0,
        Segment::Bezier { control1, control2 } => {
            a == 0.0 && b == 0.0 && control1[1] == 0.0 && control2[1] == 0.0
        }
    }
}

enum Trim {
    Keep,
    Dark,
    /// The lit part `[from, to]` in clip progress, and its curve.
    Cut(f64, f64, Keyframes),
}

/// The part of a brightness curve over a clip where it lights.
fn trim_curve(curve: &Keyframes) -> Trim {
    let n = curve.points.len();
    let first_x = curve.points[0].0;
    let last_x = curve.points[n - 1].0;
    let lit_before = first_x > 0.0 && number(&curve.points[0].1) != 0.0;
    let lit_after = last_x < 1.0 && number(&curve.points[n - 1].1) != 0.0;
    let lit: Vec<usize> = (0..n - 1).filter(|&i| !dark_segment(curve, i)).collect();
    if lit.is_empty() && !lit_before && !lit_after {
        return Trim::Dark;
    }
    // Lit segments, or the lit hold before the first / after the last point.
    let from = if lit_before {
        0.0
    } else {
        curve.points[lit[0]].0
    };
    let to = if lit_after {
        1.0
    } else {
        curve.points[lit[lit.len() - 1] + 1].0
    };
    if from <= 0.0 && to >= 1.0 {
        return Trim::Keep;
    }
    let (a, b) = if lit.is_empty() {
        (0, n - 1)
    } else {
        (
            if lit_before { 0 } else { lit[0] },
            if lit_after {
                n - 1
            } else {
                lit[lit.len() - 1] + 1
            },
        )
    };
    let span = to - from;
    let u = |x: f64| ((x - from) / span).clamp(0.0, 1.0);
    let mut points: Vec<(f64, Key)> = curve.points[a..=b]
        .iter()
        .map(|(x, key)| (u(*x), key.clone()))
        .collect();
    points[0].0 = 0.0;
    let last = points.len() - 1;
    points[last].0 = 1.0;
    let segments = if curve.segments.is_empty() {
        Vec::new()
    } else {
        curve.segments[a..b]
            .iter()
            .map(|segment| match segment {
                Segment::Bezier { control1, control2 } => Segment::Bezier {
                    control1: [u(control1[0]), control1[1]],
                    control2: [u(control2[0]), control2[1]],
                },
                other => *other,
            })
            .collect()
    };
    let trimmed = Keyframes { points, segments };
    trimmed.validate().expect("a trimmed curve is valid");
    Trim::Cut(from, to, trimmed)
}

#[derive(Default)]
struct Tally {
    deleted_dark: Vec<String>,
    deleted_double: Vec<String>,
    trimmed_wash: usize,
    trimmed_wash_start: usize,
    trimmed_wash_end: usize,
    trimmed_chase: BTreeSet<String>,
}

/// `(old key, NN)` for a split key.
fn split_key(key: &str) -> (&str, u32) {
    let (base, nn) = key.rsplit_once('-').expect("a split key");
    (base, nn.parse().expect("a split number"))
}

/// Apply the cleanup to one score's split clips.
fn clean(score: &Score, keys: &BTreeSet<String>, tally: &mut Tally) -> Result<Score, String> {
    let mut out = score.clone();
    for key in keys {
        let clip = out.clips.get_mut(key).ok_or(format!("{key} missing"))?;
        match clip.graph.as_str() {
            "color.constant@1" => {
                let black =
                    matches!(clip.inputs.get("color"), Some(Value::Color(c)) if *c == [0.0; 3]);
                match &clip.inputs["brightness"] {
                    Value::Proportion(v) if *v == 0.0 || black => {
                        out.clips.remove(key);
                        tally.deleted_dark.push(key.clone());
                    }
                    Value::Proportion(_) => {}
                    Value::Hit(curve) if !black => match trim_curve(curve) {
                        Trim::Keep => {}
                        Trim::Dark => {
                            out.clips.remove(key);
                            tally.deleted_dark.push(key.clone());
                        }
                        Trim::Cut(from, to, curve) => {
                            let d = clip.duration;
                            clip.start += from * d;
                            clip.duration = (to - from) * d;
                            clip.inputs.insert("brightness".into(), Value::Hit(curve));
                            tally.trimmed_wash += 1;
                            tally.trimmed_wash_start += usize::from(from > 0.0);
                            tally.trimmed_wash_end += usize::from(to < 1.0);
                        }
                    },
                    Value::Hit(_) => {
                        out.clips.remove(key);
                        tally.deleted_dark.push(key.clone());
                    }
                    other => return Err(format!("{key}: brightness {other:?}")),
                }
            }
            "color.chase@1" => {
                let Value::Beats(travel) = clip.inputs["travel"] else {
                    return Err(format!("{key}: travel is not plain beats"));
                };
                if travel > 0.0 && clip.duration > travel {
                    clip.duration = travel;
                    tally.trimmed_chase.insert(key.clone());
                }
            }
            other => return Err(format!("{key}: unexpected form {other}")),
        }
    }
    // Double hits: chases of one old clip that start less than DOUBLE apart.
    let mut by_old: BTreeMap<&str, Vec<(u32, &String)>> = BTreeMap::new();
    for key in keys {
        if out
            .clips
            .get(key)
            .is_some_and(|c| c.graph == "color.chase@1")
        {
            let (base, nn) = split_key(key);
            by_old.entry(base).or_default().push((nn, key));
        }
    }
    let mut drop = Vec::new();
    for parts in by_old.values_mut() {
        parts.sort();
        let mut kept: Option<&Clip> = None;
        for (_, key) in parts.iter() {
            let clip = &out.clips[*key];
            match kept {
                Some(first)
                    if clip.start - first.start < DOUBLE
                        && first.z_index == clip.z_index
                        && first.blend_mode == clip.blend_mode =>
                {
                    drop.push((*key).clone());
                }
                _ => kept = Some(clip),
            }
        }
    }
    for key in drop {
        out.clips.remove(&key);
        tally.trimmed_chase.remove(&key);
        tally.deleted_double.push(key);
    }
    Ok(out)
}

struct Ctx {
    pool: sqlx::SqlitePool,
    storage: StorageRoot,
    fixtures: PathBuf,
}

impl Ctx {
    async fn scene(&self, score_id: &str, score: Score) -> Result<Scene, String> {
        let scene = luma_lib::build_score_scene(
            &self.pool,
            &self.storage,
            &self.fixtures,
            score_id,
            Some(score),
        )
        .await?;
        Ok(Scene::new(scene.annotations))
    }

    async fn clock(&self, score_id: &str) -> Result<luma_patterns::BeatTimeline, String> {
        let track_id: String = sqlx::query_scalar("SELECT track_id FROM scores WHERE id = ?")
            .bind(score_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| e.to_string())?;
        luma_lib::services::tracks::get_track_beats(&self.pool, &track_id)
            .await?
            .ok_or("the track has no beat grid")?
            .timeline()
            .map_err(|e| e.to_string())
    }

    async fn track(&self, score_id: &str) -> Result<String, String> {
        let row = sqlx::query(
            "SELECT t.id, coalesce(t.title, '') AS title FROM scores s \
             JOIN tracks t ON t.id = s.track_id WHERE s.id = ?",
        )
        .bind(score_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        let id: String = row.get("id");
        let title: String = row.get("title");
        Ok(format!("{title} ({})", &id[..8]))
    }
}

fn seconds(clock: &luma_patterns::BeatTimeline, beats: &[f64]) -> Result<Vec<f32>, String> {
    beats
        .iter()
        .map(|b| {
            clock
                .seconds_at(*b)
                .map(|s| s as f32)
                .map_err(|e| e.to_string())
        })
        .collect()
}

/// Emitted light per channel and strobe.
fn channels(state: Option<&PrimitiveState>) -> [f64; 5] {
    let Some(state) = state else {
        return [0.0; 5];
    };
    let light = |c: usize| f64::from(state.color[c]) * f64::from(state.dimmer);
    [
        light(0),
        light(1),
        light(2),
        f64::from(state.strobe),
        f64::from(state.position[0]) + f64::from(state.position[1]),
    ]
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

/// Largest emitted light or strobe on any head.
fn output(frame: &UniverseState) -> f64 {
    frame
        .primitives
        .values()
        .map(|p| channels(Some(p))[..4].iter().copied().fold(0.0, f64::max))
        .fold(0.0, f64::max)
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut app_dir = None;
    let mut apply = false;
    let mut storage = None;
    let mut skip_scan = false;
    let mut backup = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--app-dir" => app_dir = args.next().map(PathBuf::from),
            "--storage-root" => storage = args.next().map(PathBuf::from),
            "--apply" => apply = true,
            "--skip-scan" => skip_scan = true,
            "--backup" => backup = args.next().map(PathBuf::from),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let app_dir = app_dir.ok_or("--app-dir is required")?;
    let storage = StorageRoot::from_path(storage.unwrap_or_else(|| app_dir.clone()));
    let fixtures = luma_lib::headless_host::HostConfig::default().fixtures_root()?;
    let (db, _connections) = open_app_db_at(&app_dir).await?;
    let pool = db.0.clone();
    let err = |e: sqlx::Error| e.to_string();
    let ctx = Ctx {
        pool: pool.clone(),
        storage,
        fixtures,
    };

    // The split clips.
    let mut split: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut count = 0;
    for row in sqlx::query("SELECT id, score_id FROM clips WHERE created_at LIKE ? ORDER BY id")
        .bind(SPLIT_CREATED)
        .fetch_all(&pool)
        .await
        .map_err(err)?
    {
        let id: String = row.get("id");
        let score_id: String = row.get("score_id");
        let key = id
            .strip_prefix(&score_id)
            .and_then(|rest| rest.strip_prefix(':'))
            .ok_or_else(|| format!("{id} is not in {score_id}"))?
            .to_owned();
        let (_, nn) = key
            .rsplit_once('-')
            .ok_or(format!("{key} is not a split key"))?;
        if nn.len() != 2 || !nn.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("{key} is not a split key"));
        }
        split.entry(score_id).or_default().insert(key);
        count += 1;
    }
    if count != SPLIT_CLIPS {
        return Err(format!("found {count} split clips, expected {SPLIT_CLIPS}"));
    }
    println!("split clips: {count} in {} scores", split.len());

    let library = luma_patterns::standard_library();
    let mut tally = Tally::default();
    let mut finals: BTreeMap<String, Score> = BTreeMap::new();
    let mut looks: Vec<(String, f64, f64, usize, usize, String)> = Vec::new();
    for (score_id, keys) in &split {
        let mut connection = pool.acquire().await.map_err(err)?;
        let score = load_score(&mut connection, score_id).await?;
        drop(connection);
        let cleaned = clean(&score, keys, &mut tally)?;
        cleaned
            .validate(&library)
            .map_err(|e| format!("{score_id}: {e}"))?;
        // Render the old spans of every split clip, before and after.
        let mut beats: Vec<f64> = Vec::new();
        for key in keys {
            let clip = &score.clips[key];
            let steps = (clip.duration / STEP).ceil() as usize;
            for i in 0..=steps {
                beats.push(clip.start + (i as f64 * STEP).min(clip.duration));
            }
            if let Some(new) = cleaned.clips.get(key) {
                for edge in [new.start, new.start + new.duration] {
                    beats.extend([edge - 0.005, edge + 0.005]);
                }
            }
        }
        beats.sort_by(f64::total_cmp);
        beats.dedup();
        let clock = ctx.clock(score_id).await?;
        let times = seconds(&clock, &beats)?;
        let before = ctx.scene(score_id, score.clone()).await?.try_render(
            &times,
            Scope::Composite,
            &mut Arena::default(),
        )?;
        let after = ctx.scene(score_id, cleaned.clone()).await?.try_render(
            &times,
            Scope::Composite,
            &mut Arena::default(),
        )?;
        let (mut max, mut at, mut changed) = (0.0_f64, 0.0, 0);
        for ((x, y), b) in before.iter().zip(&after).zip(&beats) {
            let d = compare(x, y);
            if d >= SAME {
                changed += 1;
            }
            if d > max {
                max = d;
                at = *b;
            }
        }
        if std::env::var_os("TRIM_DEBUG").is_some() && max >= SAME {
            let k = beats.iter().position(|b| *b == at).unwrap();
            let (x, y) = (&before[k], &after[k]);
            let mut worst = (0.0, String::new());
            for (head, p) in &x.primitives {
                let d = compare(
                    &UniverseState {
                        primitives: [(head.clone(), p.clone())].into(),
                    },
                    &UniverseState {
                        primitives: y
                            .primitives
                            .get(head)
                            .map(|q| (head.clone(), q.clone()))
                            .into_iter()
                            .collect(),
                    },
                );
                if d > worst.0 {
                    worst = (
                        d,
                        format!(
                            "{head}: {:?} -> {:?}",
                            channels(Some(p)),
                            channels(y.primitives.get(head))
                        ),
                    );
                }
            }
            println!("    {score_id} beat {at:.3} {}", worst.1);
        }
        let blends: BTreeSet<String> = keys
            .iter()
            .map(|k| score.clips[k].blend_mode.name().to_owned())
            .collect();
        looks.push((
            ctx.track(score_id).await?,
            max,
            at,
            changed,
            beats.len(),
            blends.into_iter().collect::<Vec<_>>().join("/"),
        ));
        finals.insert(score_id.clone(), cleaned);
    }

    println!(
        "deleted dark: {} {:?}",
        tally.deleted_dark.len(),
        tally.deleted_dark
    );
    println!(
        "deleted double hits: {} {:?}",
        tally.deleted_double.len(),
        tally.deleted_double
    );
    println!(
        "trimmed washes: {} ({} at the start, {} at the end)",
        tally.trimmed_wash, tally.trimmed_wash_start, tally.trimmed_wash_end
    );
    println!("trimmed chases: {}", tally.trimmed_chase.len());
    println!("look by track (max difference, at beat, samples changed / sampled, split blends):");
    for (track, max, at, changed, total, blends) in &looks {
        println!("  {track}: {max:.4} at {at:.3}, {changed}/{total}, {blends}");
    }

    if !skip_scan {
        // Black-only clips in the whole database, each rendered alone.
        println!("black-only clips (after the cleanup):");
        let scores: Vec<String> = sqlx::query_scalar("SELECT DISTINCT score_id FROM clips")
            .fetch_all(&pool)
            .await
            .map_err(err)?;
        for score_id in scores {
            let score = match finals.get(&score_id) {
                Some(score) => score.clone(),
                None => {
                    let mut connection = pool.acquire().await.map_err(err)?;
                    load_score(&mut connection, &score_id).await?
                }
            };
            let Ok(clock) = ctx.clock(&score_id).await else {
                println!("  {score_id}: no beat grid; not scanned");
                continue;
            };
            for (key, clip) in &score.clips {
                let beats: Vec<f64> = (0..SCAN)
                    .map(|i| clip.start + clip.duration * (i as f64 + 0.5) / SCAN as f64)
                    .collect();
                let times = seconds(&clock, &beats)?;
                let alone = Score {
                    clips: BTreeMap::from([(key.clone(), clip.clone())]),
                };
                let frames = ctx.scene(&score_id, alone).await?.try_render(
                    &times,
                    Scope::Composite,
                    &mut Arena::default(),
                )?;
                let peak = frames.iter().map(output).fold(0.0, f64::max);
                if peak < SAME {
                    let why = if frames.iter().all(|f| f.primitives.is_empty()) {
                        "selects no heads"
                    } else {
                        "dark"
                    };
                    println!(
                        "  {why} {score_id}:{key} {} {} {:.3}+{:.3} ({})",
                        clip.graph,
                        clip.blend_mode.name(),
                        clip.start,
                        clip.duration,
                        ctx.track(&score_id).await?
                    );
                }
            }
        }
    }

    if !apply {
        return Ok(());
    }
    let backup = backup.ok_or("--apply needs --backup PATH")?;
    if backup.exists() {
        return Err(format!("{} exists", backup.display()));
    }
    sqlx::query("VACUUM INTO ?")
        .bind(backup.to_string_lossy().into_owned())
        .execute(&pool)
        .await
        .map_err(err)?;
    println!("backup: {}", backup.display());
    let queue = |pool: sqlx::SqlitePool| async move {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM ps_crud")
            .fetch_one(&pool)
            .await
            .map_err(|e| e.to_string())
    };
    println!("before: ps_crud {}", queue(pool.clone()).await?);
    let mut tx = pool.begin().await.map_err(err)?;
    let mut total = Changed::default();
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
    let deleted = tally.deleted_dark.len() + tally.deleted_double.len();
    let updated = tally.trimmed_wash + tally.trimmed_chase.len();
    if total.inserted != 0 || total.deleted != deleted || total.updated != updated {
        return Err(format!(
            "unexpected writes {total:?} (expected {deleted} deleted, {updated} updated); rolled back"
        ));
    }
    tx.commit().await.map_err(err)?;
    println!("wrote {total:?}");
    println!("after: ps_crud {}", queue(pool.clone()).await?);
    Ok(())
}
