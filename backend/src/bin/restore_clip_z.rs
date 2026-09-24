//! One-time repair: put clips back on their layers after the clip forms apply
//! (2026-09-23) gave every clip its own z-index. Short-lived: delete after it
//! has run.
//!
//! `--dump FILE` writes, per score and clip, the clip's active span in seconds
//! and the heads it writes. The plan is made from that dump offline.
//!
//! `--plan FILE` reads `{score_id: {key: [current z, new z]}}`. Each planned
//! score must hold exactly the planned clips at their current z. The tool
//! renders every planned score with the current and the new z and requires an
//! exact match. With `--apply --backup PATH` it writes the new z in one
//! transaction.
//!
//! Opens the database with `open_app_db_at`, so the change log and the upload
//! queue record every write. Close the app first.
//!
//! ```text
//! cargo +1.97.1 run --release --bin restore_clip_z -- --app-dir DIR \
//!     [--storage-root DIR] (--dump FILE | --plan FILE [--apply --backup PATH])
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
use luma_patterns::Score;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A difference below this is no change.
const SAME: f64 = 2e-6;
/// Uniform render step, in seconds.
const STEP: f32 = 0.005;
/// Distance from a span boundary to sample on each side, in seconds.
const EDGE: f32 = 0.0005;
/// Frames rendered per batch.
const BATCH: usize = 2048;

type Plan = BTreeMap<String, BTreeMap<String, (i64, i64)>>;

struct Ctx {
    pool: sqlx::SqlitePool,
    storage: StorageRoot,
    fixtures: PathBuf,
}

impl Ctx {
    async fn load(&self, score_id: &str) -> Result<Score, String> {
        let mut connection = self.pool.acquire().await.map_err(|e| e.to_string())?;
        load_score(&mut connection, score_id).await
    }

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
}

/// Every field of a head's state.
fn channels(state: Option<&PrimitiveState>) -> [f64; 9] {
    let Some(s) = state else {
        return [0.0; 9];
    };
    [
        f64::from(s.dimmer),
        f64::from(s.color[0]),
        f64::from(s.color[1]),
        f64::from(s.color[2]),
        f64::from(s.strobe),
        f64::from(s.position[0]),
        f64::from(s.position[1]),
        f64::from(s.speed),
        1.0,
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

/// Sample times: a uniform grid, and both sides and the middle of every
/// stretch between span boundaries.
fn sample_times(scene: &Scene) -> Vec<f32> {
    let mut edges: Vec<f32> = scene
        .annotations
        .iter()
        .flat_map(|a| [a.span.0, a.span.1])
        .collect();
    edges.sort_by(f32::total_cmp);
    edges.dedup();
    let mut times = Vec::new();
    if let (Some(first), Some(last)) = (edges.first(), edges.last()) {
        let mut t = first.max(0.0);
        while t <= *last {
            times.push(t);
            t += STEP;
        }
    }
    for pair in edges.windows(2) {
        times.push((pair[0] + pair[1]) / 2.0);
    }
    for edge in &edges {
        times.extend([edge - EDGE, edge + EDGE]);
    }
    times.retain(|t| *t >= 0.0);
    times.sort_by(f32::total_cmp);
    times.dedup();
    times
}

/// Largest difference between the two scenes, and where.
fn difference(before: &Scene, after: &Scene) -> Result<(f64, f32, usize), String> {
    let mut times = sample_times(before);
    times.extend(sample_times(after));
    times.sort_by(f32::total_cmp);
    times.dedup();
    let (mut max, mut at) = (0.0_f64, 0.0_f32);
    for chunk in times.chunks(BATCH) {
        let x = before.try_render(chunk, Scope::Composite, &mut Arena::default())?;
        let y = after.try_render(chunk, Scope::Composite, &mut Arena::default())?;
        for ((a, b), t) in x.iter().zip(&y).zip(chunk) {
            let d = compare(a, b);
            if d > max {
                max = d;
                at = *t;
            }
        }
    }
    Ok((max, at, times.len()))
}

async fn dump(ctx: &Ctx, out: &Path) -> Result<(), String> {
    let scores: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT score_id FROM clips ORDER BY score_id")
            .fetch_all(&ctx.pool)
            .await
            .map_err(|e| e.to_string())?;
    let mut all = serde_json::Map::new();
    for score_id in scores {
        let score = ctx.load(&score_id).await?;
        // Each clip on its own z, so an annotation names its clip.
        let keys: Vec<String> = score.clips.keys().cloned().collect();
        let mut tagged = score.clone();
        for (index, key) in keys.iter().enumerate() {
            tagged.clips.get_mut(key).unwrap().z_index = index as i64;
        }
        let scene = match ctx.scene(&score_id, tagged).await {
            Ok(scene) => scene,
            Err(error) => {
                println!("{score_id}: no scene: {error}");
                continue;
            }
        };
        let mut clips = serde_json::Map::new();
        for annotation in &scene.annotations {
            let key = &keys[annotation.z_index as usize];
            let clip = &score.clips[key];
            clips.insert(
                key.clone(),
                json!({
                    "z": clip.z_index,
                    "start": clip.start,
                    "duration": clip.duration,
                    "span": [annotation.span.0, annotation.span.1],
                    "heads": annotation.plan.primitive_ids,
                }),
            );
        }
        println!("{score_id}: {} clips, {} compiled", keys.len(), clips.len());
        all.insert(score_id, clips.into());
    }
    std::fs::write(out, serde_json::to_string(&all).unwrap()).map_err(|e| e.to_string())
}

async fn distinct_layers(pool: &sqlx::SqlitePool) -> Result<i64, String> {
    sqlx::query_scalar("SELECT count(DISTINCT score_id || ':' || z_index) FROM clips")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())
}

async fn queue(pool: &sqlx::SqlitePool) -> Result<i64, String> {
    sqlx::query_scalar("SELECT count(*) FROM ps_crud")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut app_dir = None;
    let mut storage = None;
    let mut dump_to = None;
    let mut plan = None;
    let mut apply = false;
    let mut backup = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--app-dir" => app_dir = args.next().map(PathBuf::from),
            "--storage-root" => storage = args.next().map(PathBuf::from),
            "--dump" => dump_to = args.next().map(PathBuf::from),
            "--plan" => plan = args.next().map(PathBuf::from),
            "--apply" => apply = true,
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

    if let Some(out) = dump_to {
        return dump(&ctx, &out).await;
    }
    let plan_path = plan.ok_or("--dump or --plan is required")?;
    let plan: Plan = serde_json::from_slice(
        &std::fs::read(&plan_path).map_err(|e| format!("{}: {e}", plan_path.display()))?,
    )
    .map_err(|e| e.to_string())?;

    println!("distinct score:z before: {}", distinct_layers(&pool).await?);
    let mut finals: BTreeMap<String, Score> = BTreeMap::new();
    let mut changes = 0;
    let mut inexact = Vec::new();
    for (score_id, clips) in &plan {
        let score = ctx.load(score_id).await?;
        let stored: BTreeSet<&String> = score.clips.keys().collect();
        let planned: BTreeSet<&String> = clips.keys().collect();
        if stored != planned {
            return Err(format!(
                "{score_id}: clips differ from the plan: {} not planned, {} missing",
                stored.difference(&planned).count(),
                planned.difference(&stored).count()
            ));
        }
        let mut next = score.clone();
        for (key, (now, new)) in clips {
            let clip = next.clips.get_mut(key).unwrap();
            if clip.z_index != *now {
                return Err(format!(
                    "{score_id}:{key} is at z {}, the plan expects {now}",
                    clip.z_index
                ));
            }
            if clip.z_index != *new {
                clip.z_index = *new;
                changes += 1;
            }
        }
        let before = ctx.scene(score_id, score).await?;
        let after = ctx.scene(score_id, next.clone()).await?;
        if before.annotations.len() != after.annotations.len() {
            return Err(format!("{score_id}: compiled clip count differs"));
        }
        let (max, at, samples) = difference(&before, &after)?;
        let levels: BTreeSet<i64> = next.clips.values().map(|c| c.z_index).collect();
        println!(
            "{score_id}: {} clips, levels {levels:?}, {samples} samples, max difference {max:.2e} at {at:.3}s",
            clips.len()
        );
        if max >= SAME {
            inexact.push(score_id.clone());
        }
        finals.insert(score_id.clone(), next);
    }
    println!("z changes: {changes}");
    if !inexact.is_empty() {
        return Err(format!("not exact: {inexact:?}"));
    }
    println!("every planned score renders the same");

    if apply {
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
        println!("before: ps_crud {}", queue(&pool).await?);
    }
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
    if total.inserted != 0 || total.deleted != 0 || total.updated != changes {
        return Err(format!(
            "unexpected writes {total:?} (expected {changes} updated); rolled back"
        ));
    }
    let layers: i64 =
        sqlx::query_scalar("SELECT count(DISTINCT score_id || ':' || z_index) FROM clips")
            .fetch_one(&mut *tx)
            .await
            .map_err(err)?;
    println!("distinct score:z after: {layers}");
    if !apply {
        tx.rollback().await.map_err(err)?;
        println!("dry run: rolled back {total:?}");
        return Ok(());
    }
    tx.commit().await.map_err(err)?;
    println!("wrote {total:?}");
    println!("after: ps_crud {}", queue(&pool).await?);
    Ok(())
}
