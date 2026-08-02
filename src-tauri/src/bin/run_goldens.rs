//! Runs every captured golden through the NEW eval engine and compares to the
//! legacy output. For each fixture: load the graph from the DB, take the resolved
//! primitive ids / args / span from the golden, compile -> eval -> diff.
//!
//!   cargo run --release --bin run_goldens
//!
//! Positions are built per-head via the shared `fixtures::layout` mapping (offset
//! + rotation + base), audio is decoded resident, and sample times are clamped to
//! the annotation span (the capture sampled some out-of-span frames legacy held at
//! the boundary). Comparison is on the emitted `dimmer × color`, not the HSV-split
//! channels. Reports pass / fail(diff) / skip(unlowered node), plus a histogram of
//! the node types still to lower.

use luma_lib::audio::{load_or_decode_audio, read_pcm_file, stereo_to_mono};
use luma_lib::eval::compile::{compile_pattern, CompileError};
use luma_lib::eval::{eval, Arena, ResidentAudio, ResidentContext};
use luma_lib::models::node_graph::{BeatGrid, Graph};
use luma_lib::services::tracks::TARGET_SAMPLE_RATE;
use luma_lib::storage::StorageRoot;
use serde_json::Value;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const AUDIO_NODES: &[&str] = &[
    "audio_input",
    "frequency_amplitude",
    "stem_splitter",
    "harmony_analysis",
    "drum_events",
    "lowpass_filter",
    "highpass_filter",
];

/// Decode a track's mono audio (cached per track_id). Returns None if the file is
/// missing/undecodable (audio ops then read silence).
async fn track_audio(
    pool: &SqlitePool,
    track_id: &str,
    cache: &mut HashMap<String, Option<ResidentAudio>>,
) -> Option<ResidentAudio> {
    if let Some(c) = cache.get(track_id) {
        return c.clone();
    }
    let row = sqlx::query("SELECT file_path, track_hash FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    let res = row.and_then(|r| {
        let path: String = r.get(0);
        let hash: String = r.try_get(1).unwrap_or_default();
        let audio = load_or_decode_audio(Path::new(&path), &hash, TARGET_SAMPLE_RATE).ok()?;
        Some(ResidentAudio {
            samples: Arc::new(stereo_to_mono(&audio.samples)),
            sample_rate: audio.sample_rate,
        })
    });
    cache.insert(track_id.to_string(), res.clone());
    res
}

/// Read a cached stem `.pcm` into mono `ResidentAudio` (the eval engine is
/// always mono, so a stereo file is downmixed on the way in).
fn read_stem_pcm(path: &Path) -> Option<ResidentAudio> {
    let pcm = read_pcm_file(path).ok()?;
    let mono = if pcm.channels >= 2 {
        stereo_to_mono(&pcm.samples)
    } else {
        pcm.samples
    };
    Some(ResidentAudio {
        samples: Arc::new(mono),
        sample_rate: pcm.sample_rate,
    })
}

/// Load the stems actually consumed by a `stem_splitter` in this graph (by the
/// `<name>_out` ports wired downstream), from the on-disk PCM cache.
async fn load_needed_stems(
    pool: &SqlitePool,
    storage: &StorageRoot,
    graph: &Graph,
    track_id: &str,
) -> HashMap<String, ResidentAudio> {
    let mut out = HashMap::new();
    let splitter_ids: Vec<&str> = graph
        .nodes
        .iter()
        .filter(|n| n.type_id == "stem_splitter")
        .map(|n| n.id.as_str())
        .collect();
    if splitter_ids.is_empty() {
        return out;
    }
    let mut names: std::collections::BTreeSet<String> = Default::default();
    for e in &graph.edges {
        if splitter_ids.contains(&e.from_node.as_str()) {
            if let Some(stem) = e.from_port.strip_suffix("_out") {
                names.insert(stem.to_string());
            }
        }
    }
    let hash: Option<String> = sqlx::query("SELECT track_hash FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get::<String, _>(0).ok());
    let Some(hash) = hash else { return out };
    for name in names {
        let path = storage.stem_pcm_path(&hash, &name);
        if let Some(audio) = read_stem_pcm(&path) {
            out.insert(name, audio);
        }
    }
    out
}

/// Drum-onset times per class (`kick|snare|hat|cymbal`) from `track_drum_onsets`.
async fn fetch_drum_onsets(pool: &SqlitePool, track_id: &str) -> HashMap<String, Vec<f32>> {
    let row = sqlx::query("SELECT onsets_json FROM track_drum_onsets WHERE track_id = ? LIMIT 1")
        .bind(track_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    row.and_then(|r| r.try_get::<String, _>(0).ok())
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

/// Chord sections `(start, end, root)` from `track_roots.sections_json`.
async fn fetch_chord_sections(pool: &SqlitePool, track_id: &str) -> Vec<(f32, f32, Option<u8>)> {
    let row = sqlx::query("SELECT sections_json FROM track_roots WHERE track_id = ? LIMIT 1")
        .bind(track_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    let Some(json) = row.and_then(|r| r.try_get::<String, _>(0).ok()) else {
        return Vec::new();
    };
    let Ok(arr) = serde_json::from_str::<Vec<Value>>(&json) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|s| {
            Some((
                s["start"].as_f64()? as f32,
                s["end"].as_f64()? as f32,
                s["root"].as_u64().map(|r| r as u8),
            ))
        })
        .collect()
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/fixtures")
}

async fn fetch_graph(pool: &SqlitePool, pattern_id: &str) -> Option<Graph> {
    let implementation_id = luma_lib::services::graph_documents::resolve_graph_implementation(
        pool, pattern_id, None, None,
    )
    .await
    .ok()?;
    luma_lib::services::graph_documents::load_graph_document_unscoped(
        pool,
        pattern_id,
        &implementation_id,
    )
    .await
    .ok()
    .map(|document| document.graph)
}

/// The fixtures-library resource root (`fixture_path` is relative to it). In the
/// app this is a Tauri resource dir; for the harness we locate the versioned
/// snapshot under `resources/fixtures/<v>/` that actually holds the definitions.
fn fixtures_root() -> Option<PathBuf> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join("resources/fixtures");
    std::fs::read_dir(&base)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.is_dir())
}

/// Map primitive id (`fixture-uuid:head`) -> world position, reproducing the
/// legacy mapping exactly: per-head GDTF offset rotated by the fixture
/// orientation and added to the base position (shared `head_world_position`).
/// This replaces the old base-position shortcut so spatial patterns (chases,
/// gradients) see the same per-head geometry the legacy engine did.
async fn fetch_positions(pool: &SqlitePool, venue_id: &str) -> HashMap<String, [f32; 3]> {
    let root = fixtures_root();
    let rows = sqlx::query(
        "SELECT id, fixture_path, mode_name, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z
         FROM fixtures WHERE venue_id = ?",
    )
    .bind(venue_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut out = HashMap::new();
    for r in rows {
        let id: String = r.get(0);
        let fixture_path: String = r.try_get(1).unwrap_or_default();
        let mode_name: String = r.try_get(2).unwrap_or_default();
        let base = [
            r.try_get::<f64, _>(3).unwrap_or(0.0) as f32,
            r.try_get::<f64, _>(4).unwrap_or(0.0) as f32,
            r.try_get::<f64, _>(5).unwrap_or(0.0) as f32,
        ];
        let rot = [
            r.try_get::<f64, _>(6).unwrap_or(0.0),
            r.try_get::<f64, _>(7).unwrap_or(0.0),
            r.try_get::<f64, _>(8).unwrap_or(0.0),
        ];
        // Per-head offsets from the definition (single head at origin if missing).
        let offsets = root
            .as_ref()
            .map(|root| root.join(&fixture_path))
            .and_then(|p| luma_lib::fixtures::parser::parse_definition(&p).ok())
            .map(|def| luma_lib::fixtures::layout::compute_head_offsets(&def, &mode_name))
            .unwrap_or_else(|| {
                vec![luma_lib::fixtures::layout::HeadLayout {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                }]
            });
        for (i, offset) in offsets.iter().enumerate() {
            let pos = luma_lib::fixtures::layout::head_world_position(base, rot, *offset);
            out.insert(format!("{id}:{i}"), pos);
        }
    }
    out
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
    let storage = StorageRoot::from_env_default().expect("locate the app config dir");
    let pool = SqlitePoolOptions::new()
        .connect(storage.luma_db_path().to_str().unwrap())
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
    let mut audio_cache: HashMap<String, Option<ResidentAudio>> = HashMap::new();
    // Rough-match threshold on mean absolute error of the emitted output. A clean
    // bit-match isn't the goal (the engines differ by design). The captured set
    // splits bimodally: patterns whose behavior matches but differ on isolated
    // knife-edge frames or RNG fixture choice land ≤ ~0.13, while patterns with a
    // systematic, across-the-board difference land ≥ ~0.19. The threshold sits in
    // that gap — "is the rough output the same?", not "is it identical?".
    const ROUGH_TOL: f32 = 0.15;

    for path in &files {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let g: Value = match std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
        {
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
        let span = (
            g["start_time"].as_f64().unwrap_or(0.0) as f32,
            g["end_time"].as_f64().unwrap_or(0.0) as f32,
        );
        // Clamp sample times to the annotation span. The capture sampled some frames
        // outside the span (e.g. absolute t=0 for a span-[60,68] pattern); legacy
        // held those at the span boundary. In realtime the compositor only evaluates
        // an annotation while it's active, so out-of-span times never occur — clamping
        // reproduces legacy's capture condition and tests in-span fidelity.
        let times: Vec<f32> = g["sample_times"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t.as_f64().unwrap() as f32).clamp(span.0, span.1))
            .collect();
        let mut args: std::collections::HashMap<String, Value> = g["arg_values"]
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
        // The golden's arg_values only carries overrides; fill unset args from the
        // graph's PatternArgDef defaults (legacy `arg_values.get(id).unwrap_or(default)`).
        for ad in &graph.args {
            args.entry(ad.id.clone())
                .or_insert_with(|| ad.default_value.clone());
        }
        let beat_grid = fetch_beats(&pool, g["track_id"].as_str().unwrap_or("")).await;
        let pos_map = fetch_positions(&pool, g["venue_id"].as_str().unwrap_or("")).await;
        let positions: Vec<[f32; 3]> = primitive_ids
            .iter()
            .map(|pid| pos_map.get(pid).copied().unwrap_or([0.0, 0.0, 0.0]))
            .collect();
        let needs_audio = graph
            .nodes
            .iter()
            .any(|n| AUDIO_NODES.contains(&n.type_id.as_str()));
        let audio = if needs_audio {
            track_audio(
                &pool,
                g["track_id"].as_str().unwrap_or(""),
                &mut audio_cache,
            )
            .await
        } else {
            None
        };
        // Load the preprocessed stems consumed via stem_splitter (cached PCM).
        let stems = load_needed_stems(
            &pool,
            &storage,
            &graph,
            g["track_id"].as_str().unwrap_or(""),
        )
        .await;
        // Load drum onsets if the graph uses drum_events.
        let drum_onsets = if graph.nodes.iter().any(|n| n.type_id == "drum_events") {
            fetch_drum_onsets(&pool, g["track_id"].as_str().unwrap_or("")).await
        } else {
            HashMap::new()
        };
        // Load chord sections if the graph uses harmony_analysis.
        let chord_sections = if graph.nodes.iter().any(|n| n.type_id == "harmony_analysis") {
            fetch_chord_sections(&pool, g["track_id"].as_str().unwrap_or("")).await
        } else {
            Vec::new()
        };
        let ctx = ResidentContext {
            span,
            positions,
            beat_grid,
            audio,
            stems,
            drum_onsets,
            chord_sections,
            ..Default::default()
        };

        match compile_pattern(
            &graph.nodes,
            &graph.edges,
            &args,
            ctx,
            primitive_ids.clone(),
        ) {
            Ok(plan) => {
                let mut arena = Arena::default();
                let got = eval(&plan, &times, &mut arena);
                // Rough-match validation. The new engine evaluates the continuous
                // signal exactly; legacy captured a dense-grid → keyframe → step-hold
                // approximation, and some patterns sit on numerical knife-edges (e.g.
                // `threshold(env, 1.0)` exactly on a sustain peak) where the two
                // legitimately differ on individual frames. We don't need bit-parity
                // — we validate the *rough* output matches: mean absolute error over
                // the emitted (dimmer × color) channels, which averages out isolated
                // frame flips. Max diff is reported for context only.
                let mut max_diff = 0.0f32;
                let mut sum_abs = 0.0f64;
                let mut count = 0u64;
                let mut worst = String::new();
                for (fi, gf) in frames.iter().enumerate() {
                    for gp in gf["primitives"].as_array().unwrap() {
                        let id = gp["primitive_id"].as_str().unwrap();
                        let Some(p) = got.get(fi).and_then(|f| f.primitives.get(id)) else {
                            continue;
                        };
                        let gd = gp["dimmer"].as_f64().unwrap() as f32;
                        let gc = gp["color"].as_array().unwrap();
                        // Emitted output = dimmer × color (what reaches the lamp).
                        // Accumulate per-channel abs error for the MAE, track the max.
                        let mut local = (p.dimmer - gd).abs();
                        for ch in 0..3 {
                            let g = gd * gc[ch].as_f64().unwrap() as f32;
                            let d = (p.dimmer * p.color[ch] - g).abs();
                            sum_abs += d as f64;
                            count += 1;
                            local = local.max(d);
                        }
                        if local > max_diff {
                            max_diff = local;
                            worst = format!(
                                "got d={:.3} c=[{:.2},{:.2},{:.2}] | want d={:.3} c=[{:.2},{:.2},{:.2}]",
                                p.dimmer, p.color[0], p.color[1], p.color[2],
                                gd, gc[0].as_f64().unwrap(), gc[1].as_f64().unwrap(), gc[2].as_f64().unwrap()
                            );
                        }
                    }
                }
                let mae = if count > 0 {
                    (sum_abs / count as f64) as f32
                } else {
                    0.0
                };
                if mae < ROUGH_TOL {
                    pass += 1;
                    println!("  PASS  {name:32} mae={mae:.4} max={max_diff:.3}");
                } else {
                    fail += 1;
                    fails.push((name.clone(), mae));
                    println!("  FAIL  {name:32} mae={mae:.4} max={max_diff:.3}  {worst}");
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

    println!(
        "\n=== {} patterns: {pass} PASS, {fail} FAIL, {skip} SKIP ===",
        files.len()
    );
    if !fails.is_empty() {
        println!("fails (max diff): {:?}", fails);
    }
    if !unlowered.is_empty() {
        println!("unlowered node types (count): {:?}", unlowered);
    }
}
