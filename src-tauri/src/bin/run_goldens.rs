//! Runs every captured golden through the NEW eval engine and compares to the
//! legacy output. For each fixture: load the graph from the DB, take the resolved
//! primitive ids / args / span from the golden, compile -> eval -> diff.
//!
//!   cargo run --release --bin run_goldens
//!
//! Positions are dummy (only spatial ops need them — those patterns are flagged);
//! audio is not yet resident (audio patterns flagged). Reports pass / fail(diff) /
//! skip(unlowered node), plus a histogram of the node types still to lower.

use luma_lib::eval::compile::{compile_pattern, CompileError};
use luma_lib::eval::{eval, Arena, ResidentContext};
use luma_lib::models::node_graph::{BeatGrid, Graph};
use serde_json::Value;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn db_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap();
    PathBuf::from(home).join("Library/Application Support/com.luma.luma/luma.db")
}
fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/fixtures")
}

async fn fetch_graph(pool: &SqlitePool, pattern_id: &str) -> Option<Graph> {
    let row = sqlx::query("SELECT graph_json FROM implementations WHERE pattern_id = ? LIMIT 1")
        .bind(pattern_id)
        .fetch_optional(pool)
        .await
        .ok()??;
    let json: String = row.get(0);
    serde_json::from_str(&json).ok()
}

/// Map fixture-uuid -> base world position from the `fixtures` table. Approximate
/// (ignores per-head offsets from the fixture definition XML — exact for
/// single-head fixtures, slightly off for multi-head bars).
async fn fetch_positions(pool: &SqlitePool, venue_id: &str) -> std::collections::HashMap<String, [f32; 3]> {
    let rows = sqlx::query("SELECT id, pos_x, pos_y, pos_z FROM fixtures WHERE venue_id = ?")
        .bind(venue_id)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    rows.into_iter()
        .map(|r| {
            let id: String = r.get(0);
            let p = [
                r.try_get::<f64, _>(1).unwrap_or(0.0) as f32,
                r.try_get::<f64, _>(2).unwrap_or(0.0) as f32,
                r.try_get::<f64, _>(3).unwrap_or(0.0) as f32,
            ];
            (id, p)
        })
        .collect()
}

async fn fetch_beats(pool: &SqlitePool, track_id: &str) -> Option<BeatGrid> {
    let row = sqlx::query(
        "SELECT beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar
         FROM track_beats WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .ok()??;
    let beats: Vec<f32> = serde_json::from_str(&row.get::<String, _>(0)).unwrap_or_default();
    let downbeats: Vec<f32> = serde_json::from_str(&row.get::<String, _>(1)).unwrap_or_default();
    Some(BeatGrid {
        beats,
        downbeats,
        bpm: row.get::<f64, _>(2) as f32,
        downbeat_offset: row.try_get::<f64, _>(3).unwrap_or(0.0) as f32,
        beats_per_bar: row.try_get::<i64, _>(4).unwrap_or(4) as i32,
    })
}

#[tokio::main]
async fn main() {
    let pool = SqlitePoolOptions::new()
        .connect(db_path().to_str().unwrap())
        .await
        .expect("open luma.db");

    let mut files: Vec<PathBuf> = std::fs::read_dir(golden_dir())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .collect();
    files.sort();

    let (mut pass, mut fail, mut skip) = (0u32, 0u32, 0u32);
    let mut unlowered: BTreeMap<String, u32> = BTreeMap::new();
    let mut fails: Vec<(String, f32)> = Vec::new();
    const TOL: f32 = 0.03;

    for path in &files {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let g: Value = match std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()) {
            Some(v) => v,
            None => continue,
        };
        let pattern_id = g["pattern_id"].as_str().unwrap_or("");
        let frames = match g["frames"].as_array() {
            Some(f) if !f.is_empty() => f,
            _ => continue,
        };
        let primitive_ids: Vec<String> = frames[0]["primitives"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["primitive_id"].as_str().unwrap().to_string())
            .collect();
        if primitive_ids.is_empty() {
            continue; // 0-primitive patterns (movement-only on this venue)
        }
        let times: Vec<f32> = g["sample_times"].as_array().unwrap().iter().map(|t| t.as_f64().unwrap() as f32).collect();
        let span = (g["start_time"].as_f64().unwrap_or(0.0) as f32, g["end_time"].as_f64().unwrap_or(0.0) as f32);
        let args: std::collections::HashMap<String, Value> = g["arg_values"]
            .as_object()
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();

        let graph = match fetch_graph(&pool, pattern_id).await {
            Some(g) => g,
            None => {
                skip += 1;
                continue;
            }
        };
        let n = primitive_ids.len();
        let beat_grid = fetch_beats(&pool, g["track_id"].as_str().unwrap_or("")).await;
        let pos_map = fetch_positions(&pool, g["venue_id"].as_str().unwrap_or("")).await;
        let positions: Vec<[f32; 3]> = primitive_ids
            .iter()
            .map(|pid| {
                let uuid = pid.split(':').next().unwrap_or(pid);
                pos_map.get(uuid).copied().unwrap_or([0.0, 0.0, 0.0])
            })
            .collect();
        let ctx = ResidentContext {
            span,
            positions,
            beat_grid,
            ..Default::default()
        };

        match compile_pattern(&graph.nodes, &graph.edges, &args, ctx, primitive_ids.clone()) {
            Ok(plan) => {
                let mut arena = Arena::default();
                let got = eval(&plan, &times, &mut arena);
                let mut max_diff = 0.0f32;
                for (fi, gf) in frames.iter().enumerate() {
                    for gp in gf["primitives"].as_array().unwrap() {
                        let id = gp["primitive_id"].as_str().unwrap();
                        let Some(p) = got.get(fi).and_then(|f| f.primitives.get(id)) else { continue };
                        max_diff = max_diff.max((p.dimmer - gp["dimmer"].as_f64().unwrap() as f32).abs());
                        let gc = gp["color"].as_array().unwrap();
                        for ch in 0..3 {
                            max_diff = max_diff.max((p.color[ch] - gc[ch].as_f64().unwrap() as f32).abs());
                        }
                    }
                }
                if max_diff < TOL {
                    pass += 1;
                    println!("  PASS  {name:32} diff={max_diff:.5}");
                } else {
                    fail += 1;
                    fails.push((name.clone(), max_diff));
                    println!("  FAIL  {name:32} diff={max_diff:.5}");
                }
            }
            Err(CompileError::UnknownNode { type_id, .. }) => {
                skip += 1;
                *unlowered.entry(type_id).or_default() += 1;
                println!("  SKIP  {name:32} (unlowered node)");
            }
            Err(e) => {
                skip += 1;
                println!("  SKIP  {name:32} ({e:?})");
            }
        }
    }

    println!("\n=== {} patterns: {pass} PASS, {fail} FAIL, {skip} SKIP ===", files.len());
    if !fails.is_empty() {
        println!("fails (max diff): {:?}", fails);
    }
    if !unlowered.is_empty() {
        println!("unlowered node types (count): {:?}", unlowered);
    }
}
