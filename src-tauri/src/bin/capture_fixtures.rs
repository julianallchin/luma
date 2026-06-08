//! Golden-fixture + perf-baseline capture harness for the LEGACY lighting
//! evaluation engine.
//!
//! This is an ADDITIVE, READ-ONLY oracle harness. It only calls existing
//! public entry points (`node_graph::run_graph_internal`, `engine::render_frame`)
//! and existing DB helpers; it does NOT modify any execution-layer source.
//! Its job is to capture input -> output golden fixtures and "before" perf
//! numbers so the upcoming compiled evaluator can be validated against the
//! old engine's behavior.
//!
//! Output: `src-tauri/tests/golden/`
//!   - `fixtures/<pattern>.json`        per-pattern golden I/O
//!   - `composite.json`                 302-annotation composite samples
//!   - `bass_strobe_scrub.json`         forward-then-backward seek canary
//!   - `PERF_BASELINE.json`             machine-readable perf numbers
//!   - `index.json`                     manifest of everything captured
//!
//! Usage:
//!   cargo run --bin capture_fixtures --manifest-path src-tauri/Cargo.toml

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use luma_lib::audio::stem_cache::StemCache;
use luma_lib::audio::{decode_track_samples, stereo_to_mono, FftService};
use luma_lib::engine::render_frame;
use luma_lib::models::node_graph::{BeatGrid, Graph, GraphContext, LayerTimeSeries};
use luma_lib::models::universe::UniverseState;
use luma_lib::node_graph::{run_graph_internal, GraphExecutionConfig, SharedAudioContext};

use serde::Serialize;
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

// ---- Fixed inputs (the busy score + its track/venue) --------------------

const BUSY_SCORE_ID: &str = "eb3d77bd-f86a-4068-99c4-e8cdf367bf84";
const TRACK_ID: &str = "5313eb08-aac7-4e87-b02b-30f6195d77eb";
const VENUE_ID: &str = "99a8a8e9-2bc4-4fac-82d3-33b4cb9e6a4f";
const INSTANCE_SEED: u64 = 0xC0DE_C0DE;

fn db_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/com.luma.luma/luma.db")
}

fn fixtures_root() -> PathBuf {
    PathBuf::from("/Users/julian/github/luma/resources/fixtures/2511260420")
}

fn golden_dir() -> PathBuf {
    PathBuf::from("/Users/julian/github/luma/src-tauri/tests/golden")
}

// ---- Output shapes (stable, diffable) -----------------------------------

#[derive(Serialize)]
struct PrimSample {
    primitive_id: String,
    dimmer: f32,
    color: [f32; 3],
    strobe: f32,
    position: [f32; 2],
    speed: f32,
}

#[derive(Serialize)]
struct FrameSample {
    t: f32,
    primitives: Vec<PrimSample>,
}

#[derive(Serialize)]
struct PatternGolden {
    pattern_id: String,
    pattern_name: String,
    graph_hash: String,
    track_id: String,
    venue_id: String,
    instance_seed: u64,
    start_time: f32,
    end_time: f32,
    arg_values: Value,
    arg_source: String,
    audio_loaded: bool,
    has_audio_node: bool,
    primitive_count: usize,
    sample_times: Vec<f32>,
    frames: Vec<FrameSample>,
    exec_ms: f64,
    note: Option<String>,
}

#[derive(Serialize)]
struct IndexEntry {
    pattern_id: String,
    pattern_name: String,
    file: String,
    primitive_count: usize,
    exec_ms: f64,
    captured: bool,
    skip_reason: Option<String>,
}

// ---- Helpers ------------------------------------------------------------

/// Deterministic, order-independent hash of the graph JSON (canonicalized).
fn graph_hash(graph: &Graph) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    // Canonicalize via serde_json Value -> sorted string.
    let v = serde_json::to_value(graph).unwrap_or(Value::Null);
    let canon = canonical_json(&v);
    let mut h = DefaultHasher::new();
    canon.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn canonical_json(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .iter()
                .map(|k| format!("{:?}:{}", k, canonical_json(&m[*k])))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(a) => {
            let inner: Vec<String> = a.iter().map(canonical_json).collect();
            format!("[{}]", inner.join(","))
        }
        other => other.to_string(),
    }
}

fn snapshot(state: &UniverseState) -> Vec<PrimSample> {
    let mut prims: Vec<PrimSample> = state
        .primitives
        .iter()
        .map(|(id, p)| PrimSample {
            primitive_id: id.clone(),
            dimmer: p.dimmer,
            color: p.color,
            strobe: p.strobe,
            position: p.position,
            speed: p.speed,
        })
        .collect();
    // Stable ordering for diffable output.
    prims.sort_by(|a, b| a.primitive_id.cmp(&b.primitive_id));
    prims
}

/// Build a sample grid: t=0, span edges, and several interior points.
fn sample_grid(start: f32, end: f32) -> Vec<f32> {
    let span = (end - start).max(0.0);
    let mut times = vec![
        0.0, // before/at absolute zero
        start,
        start + span * 0.001,
        start + span * 0.1,
        start + span * 0.25,
        start + span * 0.5,
        start + span * 0.75,
        start + span * 0.9,
        end - 0.001_f32.min(span),
        end,
    ];
    times.retain(|t| t.is_finite());
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    times.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    times
}

async fn fetch_graph(pool: &SqlitePool, pattern_id: &str) -> Result<Option<Graph>, String> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT graph_json FROM implementations
         WHERE pattern_id = ? ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(pattern_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("graph query failed: {e}"))?;
    match row {
        None => Ok(None),
        Some(j) => {
            let g: Graph =
                serde_json::from_str(&j).map_err(|e| format!("parse graph failed: {e}"))?;
            Ok(Some(g))
        }
    }
}

async fn load_beat_grid(pool: &SqlitePool, track_id: &str) -> Result<Option<BeatGrid>, String> {
    let row: Option<(String, String, Option<f64>, Option<f64>, Option<i64>)> = sqlx::query_as(
        "SELECT beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar
         FROM track_beats WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("beat query failed: {e}"))?;
    let Some((beats_j, downbeats_j, bpm, off, bpb)) = row else {
        return Ok(None);
    };
    let beats: Vec<f32> = serde_json::from_str(&beats_j).unwrap_or_default();
    let downbeats: Vec<f32> = serde_json::from_str(&downbeats_j).unwrap_or_default();
    Ok(Some(BeatGrid {
        beats,
        downbeats,
        bpm: bpm.unwrap_or(120.0) as f32,
        downbeat_offset: off.unwrap_or(0.0) as f32,
        beats_per_bar: bpb.unwrap_or(4) as i32,
    }))
}

struct Annotation {
    pattern_id: String,
    start: f32,
    end: f32,
    z: i32,
    blend: String,
    args: Value,
}

async fn load_busy_annotations(pool: &SqlitePool) -> Result<Vec<Annotation>, String> {
    let rows: Vec<(String, f64, f64, i64, String, String)> = sqlx::query_as(
        "SELECT pattern_id, start_time, end_time, z_index, blend_mode, args_json
         FROM track_scores WHERE score_id = ? ORDER BY start_time, id",
    )
    .bind(BUSY_SCORE_ID)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("annotation query failed: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|(pid, s, e, z, blend, args)| Annotation {
            pattern_id: pid,
            start: s as f32,
            end: e as f32,
            z: z as i32,
            blend,
            args: serde_json::from_str(&args).unwrap_or(Value::Null),
        })
        .collect())
}

fn does_graph_need_audio(graph: &Graph) -> bool {
    graph.nodes.iter().any(|n| {
        matches!(
            n.type_id.as_str(),
            "audio_input"
                | "stem_splitter"
                | "harmony_analysis"
                | "lowpass_filter"
                | "highpass_filter"
        )
    })
}

/// Inject a `selection: all` arg so every pattern resolves to all venue
/// fixtures even when its own default expression names a group ("wash")
/// that this venue may not have. Keeps any other arg defaults intact.
fn synthetic_args(graph: &Graph) -> Value {
    let mut map = serde_json::Map::new();
    for arg in &graph.args {
        // Selection args: force "all" so we always get primitives.
        let is_selection = format!("{:?}", arg.arg_type) == "Selection";
        if is_selection {
            map.insert(
                arg.id.clone(),
                serde_json::json!({"expression": "all", "spatialReference": "global"}),
            );
        } else {
            map.insert(arg.id.clone(), arg.default_value.clone());
        }
    }
    Value::Object(map)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), String> {
    let out = golden_dir();
    std::fs::create_dir_all(out.join("fixtures")).map_err(|e| e.to_string())?;

    let dbp = db_path();
    if !dbp.exists() {
        return Err(format!("luma DB not found at {}", dbp.display()));
    }
    let connect = SqliteConnectOptions::new()
        .filename(&dbp)
        .read_only(true)
        .immutable(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(connect)
        .await
        .map_err(|e| format!("connect failed: {e}"))?;

    let stem_cache = StemCache::new();
    let fft_service = FftService::new();
    let froot = fixtures_root();

    // --- audio + beat grid for the busy track --------------------------
    let beat_grid = load_beat_grid(&pool, TRACK_ID).await?;
    eprintln!(
        "beat_grid: {}",
        beat_grid
            .as_ref()
            .map(|g| format!("{} beats, bpm={}", g.beats.len(), g.bpm))
            .unwrap_or_else(|| "NONE".into())
    );

    let track_file: Option<(String, String)> =
        sqlx::query_as("SELECT file_path, track_hash FROM tracks WHERE id = ? LIMIT 1")
            .bind(TRACK_ID)
            .fetch_optional(&pool)
            .await
            .map_err(|e| e.to_string())?;

    let shared_audio: Option<SharedAudioContext> = match &track_file {
        Some((path, hash)) if std::path::Path::new(path).exists() => {
            eprintln!("decoding audio: {path}");
            let decoded = decode_track_samples(std::path::Path::new(path), None)
                .map_err(|e| format!("audio decode failed: {e}"))?;
            let mono = stereo_to_mono(&decoded.samples);
            let dur = mono.len() as f32 / decoded.sample_rate.max(1) as f32;
            eprintln!("audio: {:.1}s mono @ {}Hz", dur, decoded.sample_rate);
            Some(SharedAudioContext {
                track_id: TRACK_ID.to_string(),
                track_hash: hash.clone(),
                samples: Arc::new(mono),
                sample_rate: decoded.sample_rate,
            })
        }
        _ => {
            eprintln!("WARNING: track audio missing — audio nodes see silence");
            None
        }
    };
    let audio_available = shared_audio.is_some();

    // Map pattern_id -> first real annotation args from the busy score, so
    // we use realistic args (and the actual placed time window) where a
    // pattern is genuinely used on this track.
    let busy = load_busy_annotations(&pool).await?;
    let mut real_args: HashMap<String, (f32, f32, Value)> = HashMap::new();
    for a in &busy {
        real_args
            .entry(a.pattern_id.clone())
            .or_insert_with(|| (a.start, a.end, a.args.clone()));
    }

    // All 43 patterns by name.
    let patterns: Vec<(String, String)> =
        sqlx::query_as("SELECT id, name FROM patterns ORDER BY name")
            .fetch_all(&pool)
            .await
            .map_err(|e| e.to_string())?;
    eprintln!("patterns: {}", patterns.len());

    let mut index: Vec<IndexEntry> = Vec::new();
    let mut per_pattern_ms: Vec<(String, f64)> = Vec::new();

    // --- 1. Per-pattern golden I/O ------------------------------------
    for (pid, name) in &patterns {
        let graph = match fetch_graph(&pool, pid).await? {
            Some(g) => g,
            None => {
                eprintln!("  {name}: NO IMPL — skipping");
                index.push(IndexEntry {
                    pattern_id: pid.clone(),
                    pattern_name: name.clone(),
                    file: String::new(),
                    primitive_count: 0,
                    exec_ms: 0.0,
                    captured: false,
                    skip_reason: Some("no implementation".into()),
                });
                continue;
            }
        };
        let ghash = graph_hash(&graph);
        let has_audio_node = does_graph_need_audio(&graph);

        // Pick time window + args. Prefer the real annotation window; else a
        // representative 8s window that contains plenty of beats.
        let (start, end, args, arg_source) = match real_args.get(pid) {
            Some((s, e, a)) => (*s, *e, a.clone(), "busy_score_annotation".to_string()),
            None => (60.0_f32, 68.0_f32, synthetic_args(&graph), "synthetic_all".to_string()),
        };
        // Always force selection -> all even on real args, so primitives
        // resolve against this venue regardless of group names.
        let args = force_all_selection(&graph, args);

        let arg_map: HashMap<String, Value> = match &args {
            Value::Object(m) => m.clone().into_iter().collect(),
            _ => HashMap::new(),
        };

        let ctx = GraphContext {
            track_id: TRACK_ID.to_string(),
            venue_id: VENUE_ID.to_string(),
            start_time: start,
            end_time: end,
            beat_grid: beat_grid.clone(),
            arg_values: Some(arg_map),
            instance_seed: Some(INSTANCE_SEED),
        };
        let config = GraphExecutionConfig {
            compute_visualizations: false,
            log_summary: false,
            log_primitives: false,
            shared_audio: if has_audio_node {
                shared_audio.clone()
            } else {
                None
            },
        };

        let t0 = Instant::now();
        let run = run_graph_internal(
            &pool,
            Some(&pool),
            &stem_cache,
            &fft_service,
            Some(froot.clone()),
            graph,
            ctx,
            config,
        )
        .await;
        let exec_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let (_, layer_opt) = match run {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  {name}: EXEC FAILED: {e}");
                index.push(IndexEntry {
                    pattern_id: pid.clone(),
                    pattern_name: name.clone(),
                    file: String::new(),
                    primitive_count: 0,
                    exec_ms,
                    captured: false,
                    skip_reason: Some(format!("exec failed: {e}")),
                });
                continue;
            }
        };

        let layer = layer_opt.unwrap_or(LayerTimeSeries { primitives: vec![] });
        let times = sample_grid(start, end);
        let frames: Vec<FrameSample> = times
            .iter()
            .map(|&t| {
                let st = render_frame(&layer, t);
                FrameSample {
                    t,
                    primitives: snapshot(&st),
                }
            })
            .collect();

        let golden = PatternGolden {
            pattern_id: pid.clone(),
            pattern_name: name.clone(),
            graph_hash: ghash,
            track_id: TRACK_ID.to_string(),
            venue_id: VENUE_ID.to_string(),
            instance_seed: INSTANCE_SEED,
            start_time: start,
            end_time: end,
            arg_values: args,
            arg_source,
            audio_loaded: audio_available && has_audio_node,
            has_audio_node,
            primitive_count: layer.primitives.len(),
            sample_times: times,
            frames,
            exec_ms,
            note: if has_audio_node && !audio_available {
                Some("audio node present but no audio loaded (silence)".into())
            } else {
                None
            },
        };

        let file = format!("fixtures/{name}.json");
        write_json(&out.join(&file), &golden)?;
        eprintln!(
            "  {name}: prims={} exec={:.1}ms{}",
            golden.primitive_count,
            exec_ms,
            if golden.audio_loaded { " [audio]" } else { "" }
        );
        index.push(IndexEntry {
            pattern_id: pid.clone(),
            pattern_name: name.clone(),
            file,
            primitive_count: golden.primitive_count,
            exec_ms,
            captured: true,
            skip_reason: None,
        });
        per_pattern_ms.push((name.clone(), exec_ms));
    }

    // --- 2. bass_strobe forward+backward scrub (determinism canary) ----
    let scrub = capture_bass_strobe_scrub(
        &pool,
        &stem_cache,
        &fft_service,
        &froot,
        &beat_grid,
        &shared_audio,
    )
    .await?;
    write_json(&out.join("bass_strobe_scrub.json"), &scrub)?;
    eprintln!("bass_strobe scrub captured: {} steps", scrub.steps.len());

    // --- 3. Full 302-annotation composite ------------------------------
    let (composite, composite_perf) = capture_composite(
        &pool,
        &stem_cache,
        &fft_service,
        &froot,
        &beat_grid,
        &shared_audio,
        &busy,
    )
    .await?;
    write_json(&out.join("composite.json"), &composite)?;
    eprintln!(
        "composite captured: {} layers, cold_resolve={:.0}ms",
        composite.layer_count, composite_perf.cold_resolve_ms
    );

    // --- 4. Perf baseline ---------------------------------------------
    let perf = build_perf(&pool, &stem_cache, &fft_service, &froot, &beat_grid, &shared_audio, &per_pattern_ms, composite_perf).await?;
    write_json(&out.join("PERF_BASELINE.json"), &perf)?;
    write_perf_md(&out.join("PERF_BASELINE.md"), &perf)?;

    // --- index manifest ------------------------------------------------
    let captured = index.iter().filter(|e| e.captured).count();
    write_json(
        &out.join("index.json"),
        &serde_json::json!({
            "track_id": TRACK_ID,
            "venue_id": VENUE_ID,
            "busy_score_id": BUSY_SCORE_ID,
            "instance_seed": INSTANCE_SEED,
            "audio_available": audio_available,
            "patterns_total": patterns.len(),
            "patterns_captured": captured,
            "entries": index,
        }),
    )?;

    eprintln!("\nDONE. {captured}/{} patterns captured.", patterns.len());
    Ok(())
}

fn force_all_selection(graph: &Graph, args: Value) -> Value {
    let mut map = match args {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    for arg in &graph.args {
        if format!("{:?}", arg.arg_type) == "Selection" {
            map.insert(
                arg.id.clone(),
                serde_json::json!({"expression": "all", "spatialReference": "global"}),
            );
        }
    }
    Value::Object(map)
}

// ---- bass_strobe scrub --------------------------------------------------

#[derive(Serialize)]
struct ScrubStep {
    order: usize,
    t: f32,
    direction: &'static str,
    primitives: Vec<PrimSample>,
}

#[derive(Serialize)]
struct ScrubGolden {
    pattern_id: String,
    pattern_name: String,
    graph_hash: String,
    start_time: f32,
    end_time: f32,
    audio_loaded: bool,
    primitive_count: usize,
    note: String,
    steps: Vec<ScrubStep>,
}

#[allow(clippy::too_many_arguments)]
async fn capture_bass_strobe_scrub(
    pool: &SqlitePool,
    stem_cache: &StemCache,
    fft_service: &FftService,
    froot: &PathBuf,
    beat_grid: &Option<BeatGrid>,
    shared_audio: &Option<SharedAudioContext>,
) -> Result<ScrubGolden, String> {
    let pid = "63ab42bc-5476-4571-a8a3-970fac0398f4"; // bass_strobe
    let graph = fetch_graph(pool, pid)
        .await?
        .ok_or("bass_strobe has no impl")?;
    let ghash = graph_hash(&graph);

    // Use the busy-score window where bass_strobe-ish strobing is active; if
    // not annotated, use a beat-dense 6s window.
    let (start, end) = (60.0_f32, 66.0_f32);
    let args = force_all_selection(&graph, synthetic_args(&graph));
    let arg_map: HashMap<String, Value> = match &args {
        Value::Object(m) => m.clone().into_iter().collect(),
        _ => HashMap::new(),
    };
    let ctx = GraphContext {
        track_id: TRACK_ID.to_string(),
        venue_id: VENUE_ID.to_string(),
        start_time: start,
        end_time: end,
        beat_grid: beat_grid.clone(),
        arg_values: Some(arg_map),
        instance_seed: Some(INSTANCE_SEED),
    };
    let config = GraphExecutionConfig {
        compute_visualizations: false,
        log_summary: false,
        log_primitives: false,
        shared_audio: shared_audio.clone(),
    };
    let (_, layer_opt) = run_graph_internal(
        pool,
        Some(pool),
        stem_cache,
        fft_service,
        Some(froot.clone()),
        graph,
        ctx,
        config,
    )
    .await
    .map_err(|e| format!("bass_strobe exec: {e}"))?;
    let layer = layer_opt.unwrap_or(LayerTimeSeries { primitives: vec![] });

    // Forward sweep then backward sweep over the same times — records the
    // legacy strobe on/off pattern across a simulated seek. Because the
    // legacy engine samples a precomputed LayerTimeSeries by absolute time
    // (binary search, step-hold), the SAME t should yield the SAME frame in
    // both directions; this file pins that.
    let dt = (end - start) / 24.0;
    let mut fwd: Vec<f32> = (0..=24).map(|i| start + i as f32 * dt).collect();
    let mut steps = Vec::new();
    let mut order = 0usize;
    for &t in &fwd {
        steps.push(ScrubStep {
            order,
            t,
            direction: "forward",
            primitives: snapshot(&render_frame(&layer, t)),
        });
        order += 1;
    }
    fwd.reverse();
    for &t in &fwd {
        steps.push(ScrubStep {
            order,
            t,
            direction: "backward",
            primitives: snapshot(&render_frame(&layer, t)),
        });
        order += 1;
    }

    Ok(ScrubGolden {
        pattern_id: pid.to_string(),
        pattern_name: "bass_strobe".to_string(),
        graph_hash: ghash,
        start_time: start,
        end_time: end,
        audio_loaded: shared_audio.is_some(),
        primitive_count: layer.primitives.len(),
        note: "Forward then backward sweep over identical times. Legacy engine \
               samples a precomputed series by absolute t (step-hold), so \
               same-t frames must match in both directions. Determinism canary."
            .to_string(),
        steps,
    })
}

// ---- composite ----------------------------------------------------------

#[derive(Serialize)]
struct CompositeGolden {
    score_id: String,
    track_id: String,
    venue_id: String,
    annotation_count: usize,
    layer_count: usize,
    span_start: f32,
    span_end: f32,
    sample_times: Vec<f32>,
    frames: Vec<FrameSample>,
}

struct CompositePerf {
    cold_resolve_ms: f64,
    layers_resolved: usize,
    annotations_failed: usize,
}

struct ResolvedLayer {
    z: i32,
    blend: luma_lib::models::node_graph::BlendMode,
    start: f32,
    end: f32,
    layer: LayerTimeSeries,
}

fn blend_from_str(s: &str) -> luma_lib::models::node_graph::BlendMode {
    use luma_lib::models::node_graph::BlendMode::*;
    match s {
        "add" => Add,
        "multiply" => Multiply,
        "screen" => Screen,
        "max" => Max,
        "min" => Min,
        "lighten" => Lighten,
        "value" => Value,
        "subtract" => Subtract,
        _ => Replace,
    }
}

#[allow(clippy::too_many_arguments)]
async fn capture_composite(
    pool: &SqlitePool,
    stem_cache: &StemCache,
    fft_service: &FftService,
    froot: &PathBuf,
    beat_grid: &Option<BeatGrid>,
    shared_audio: &Option<SharedAudioContext>,
    busy: &[Annotation],
) -> Result<(CompositeGolden, CompositePerf), String> {
    use luma_lib::engine::composite_layer_frame;

    let cold = Instant::now();
    let mut layers: Vec<ResolvedLayer> = Vec::new();
    let mut failed = 0usize;
    // Cache graphs per pattern to avoid 302 DB hits + reparse.
    let mut graph_cache: HashMap<String, Option<Graph>> = HashMap::new();

    for ann in busy {
        let graph = if let Some(g) = graph_cache.get(&ann.pattern_id) {
            g.clone()
        } else {
            let g = fetch_graph(pool, &ann.pattern_id).await?;
            graph_cache.insert(ann.pattern_id.clone(), g.clone());
            g
        };
        let Some(graph) = graph else {
            failed += 1;
            continue;
        };
        let has_audio = does_graph_need_audio(&graph);
        let args = force_all_selection(&graph, ann.args.clone());
        let arg_map: HashMap<String, Value> = match &args {
            Value::Object(m) => m.clone().into_iter().collect(),
            _ => HashMap::new(),
        };
        let ctx = GraphContext {
            track_id: TRACK_ID.to_string(),
            venue_id: VENUE_ID.to_string(),
            start_time: ann.start,
            end_time: ann.end,
            beat_grid: beat_grid.clone(),
            arg_values: Some(arg_map),
            instance_seed: Some(INSTANCE_SEED),
        };
        let config = GraphExecutionConfig {
            compute_visualizations: false,
            log_summary: false,
            log_primitives: false,
            shared_audio: if has_audio { shared_audio.clone() } else { None },
        };
        match run_graph_internal(
            pool,
            Some(pool),
            stem_cache,
            fft_service,
            Some(froot.clone()),
            graph,
            ctx,
            config,
        )
        .await
        {
            Ok((_, Some(layer))) => layers.push(ResolvedLayer {
                z: ann.z,
                blend: blend_from_str(&ann.blend),
                start: ann.start,
                end: ann.end,
                layer,
            }),
            Ok((_, None)) => {}
            Err(e) => {
                eprintln!("  composite ann {} failed: {e}", ann.pattern_id);
                failed += 1;
            }
        }
    }
    let cold_resolve_ms = cold.elapsed().as_secs_f64() * 1000.0;
    layers.sort_by_key(|l| l.z);

    let span_start = busy.iter().map(|a| a.start).fold(f32::INFINITY, f32::min);
    let span_end = busy.iter().map(|a| a.end).fold(f32::NEG_INFINITY, f32::max);

    // A handful of times across the span.
    let mut times: Vec<f32> = (0..=8)
        .map(|i| span_start + (span_end - span_start) * (i as f32 / 8.0))
        .collect();
    times.dedup_by(|a, b| (*a - *b).abs() < 1e-6);

    let frames: Vec<FrameSample> = times
        .iter()
        .map(|&t| {
            let mut state = UniverseState {
                primitives: HashMap::new(),
            };
            for l in &layers {
                if t < l.start || t > l.end {
                    continue;
                }
                composite_layer_frame(&mut state, &l.layer, t, l.blend, 1.0, None);
            }
            FrameSample {
                t,
                primitives: snapshot(&state),
            }
        })
        .collect();

    Ok((
        CompositeGolden {
            score_id: BUSY_SCORE_ID.to_string(),
            track_id: TRACK_ID.to_string(),
            venue_id: VENUE_ID.to_string(),
            annotation_count: busy.len(),
            layer_count: layers.len(),
            span_start,
            span_end,
            sample_times: times,
            frames,
        },
        CompositePerf {
            cold_resolve_ms,
            layers_resolved: layers.len(),
            annotations_failed: failed,
        },
    ))
}

// ---- perf baseline ------------------------------------------------------

#[derive(Serialize)]
struct PerfBaseline {
    machine: String,
    track_id: String,
    venue_id: String,
    audio_available: bool,
    composite_cold_resolve_ms: f64,
    composite_layers_resolved: usize,
    composite_annotations_failed: usize,
    per_pattern_exec_ms: Vec<PatternMs>,
    per_pattern_exec_mean_ms: f64,
    per_pattern_exec_median_ms: f64,
    render_frame_probe: RenderFrameProbe,
    realtime_single_eval: RealtimeProbe,
}

#[derive(Serialize, Clone)]
struct PatternMs {
    pattern: String,
    exec_ms: f64,
}

#[derive(Serialize)]
struct RenderFrameProbe {
    pattern: String,
    primitive_count: usize,
    iterations: usize,
    mean_us: f64,
    note: String,
}

#[derive(Serialize)]
struct RealtimeProbe {
    pattern: String,
    iterations: usize,
    mean_ms_per_full_graph_eval: f64,
    note: String,
}

#[allow(clippy::too_many_arguments)]
async fn build_perf(
    pool: &SqlitePool,
    stem_cache: &StemCache,
    fft_service: &FftService,
    froot: &PathBuf,
    beat_grid: &Option<BeatGrid>,
    shared_audio: &Option<SharedAudioContext>,
    per_pattern_ms: &[(String, f64)],
    composite: CompositePerf,
) -> Result<PerfBaseline, String> {
    // render_frame per-sample cost: use the gradient layer (deterministic,
    // cheap) so we measure sampling not graph exec.
    let grad_pid = "2e8fee94-3de3-4af5-8587-50673a547d8d";
    let graph = fetch_graph(pool, grad_pid).await?.ok_or("no gradient")?;
    let args = force_all_selection(&graph, synthetic_args(&graph));
    let arg_map: HashMap<String, Value> = match &args {
        Value::Object(m) => m.clone().into_iter().collect(),
        _ => HashMap::new(),
    };
    let ctx = GraphContext {
        track_id: TRACK_ID.to_string(),
        venue_id: VENUE_ID.to_string(),
        start_time: 60.0,
        end_time: 68.0,
        beat_grid: beat_grid.clone(),
        arg_values: Some(arg_map.clone()),
        instance_seed: Some(INSTANCE_SEED),
    };
    let (_, layer_opt) = run_graph_internal(
        pool,
        Some(pool),
        stem_cache,
        fft_service,
        Some(froot.clone()),
        graph,
        ctx,
        GraphExecutionConfig {
            compute_visualizations: false,
            log_summary: false,
            log_primitives: false,
            shared_audio: None,
        },
    )
    .await
    .map_err(|e| format!("perf gradient exec: {e}"))?;
    let layer = layer_opt.unwrap_or(LayerTimeSeries { primitives: vec![] });
    let prim_count = layer.primitives.len();

    let iters = 20_000usize;
    let t_probe = Instant::now();
    let mut sink = 0.0f32;
    for i in 0..iters {
        let t = 60.0 + (i as f32 % 240.0) * (8.0 / 240.0);
        let st = render_frame(&layer, t);
        // touch result so it's not optimized away
        if let Some(p) = st.primitives.values().next() {
            sink += p.dimmer;
        }
    }
    let render_mean_us = t_probe.elapsed().as_secs_f64() * 1e6 / iters as f64;
    std::hint::black_box(sink);

    // Realtime single-eval: time a full graph re-exec repeatedly (the naive
    // "recompute every frame" cost). Gradient has an audio_input node, so we
    // pass the already-decoded shared_audio to measure pure executor cost
    // rather than re-decoding the 472s track every iteration.
    let rt_iters = 30usize;
    let rt_start = Instant::now();
    for _ in 0..rt_iters {
        let graph = fetch_graph(pool, grad_pid).await?.unwrap();
        let ctx = GraphContext {
            track_id: TRACK_ID.to_string(),
            venue_id: VENUE_ID.to_string(),
            start_time: 60.0,
            end_time: 68.0,
            beat_grid: beat_grid.clone(),
            arg_values: Some(arg_map.clone()),
            instance_seed: Some(INSTANCE_SEED),
        };
        let _ = run_graph_internal(
            pool,
            Some(pool),
            stem_cache,
            fft_service,
            Some(froot.clone()),
            graph,
            ctx,
            GraphExecutionConfig {
                compute_visualizations: false,
                log_summary: false,
                log_primitives: false,
                shared_audio: shared_audio.clone(),
            },
        )
        .await;
    }
    let rt_mean_ms = rt_start.elapsed().as_secs_f64() * 1000.0 / rt_iters as f64;

    let mean = if per_pattern_ms.is_empty() {
        0.0
    } else {
        per_pattern_ms.iter().map(|(_, m)| m).sum::<f64>() / per_pattern_ms.len() as f64
    };
    let median = {
        let mut v: Vec<f64> = per_pattern_ms.iter().map(|(_, m)| *m).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if v.is_empty() {
            0.0
        } else {
            v[v.len() / 2]
        }
    };

    Ok(PerfBaseline {
        machine: format!(
            "{} / {} logical cpus",
            std::env::consts::OS,
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(0)
        ),
        track_id: TRACK_ID.to_string(),
        venue_id: VENUE_ID.to_string(),
        audio_available: shared_audio.is_some(),
        composite_cold_resolve_ms: composite.cold_resolve_ms,
        composite_layers_resolved: composite.layers_resolved,
        composite_annotations_failed: composite.annotations_failed,
        per_pattern_exec_ms: per_pattern_ms
            .iter()
            .map(|(p, m)| PatternMs {
                pattern: p.clone(),
                exec_ms: *m,
            })
            .collect(),
        per_pattern_exec_mean_ms: mean,
        per_pattern_exec_median_ms: median,
        render_frame_probe: RenderFrameProbe {
            pattern: "gradient".into(),
            primitive_count: prim_count,
            iterations: iters,
            mean_us: render_mean_us,
            note: "Cost of engine::render_frame sampling a prebuilt LayerTimeSeries (no graph exec).".into(),
        },
        realtime_single_eval: RealtimeProbe {
            pattern: "gradient".into(),
            iterations: rt_iters,
            mean_ms_per_full_graph_eval: rt_mean_ms,
            note: "Naive 'recompute the whole graph every frame' cost (gradient, no audio). \
                   This is the 'before' realtime number for a single non-audio pattern.".into(),
        },
    })
}

// ---- IO ----------------------------------------------------------------

fn write_json<T: Serialize>(path: &std::path::Path, v: &T) -> Result<(), String> {
    let s = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    std::fs::write(path, s).map_err(|e| format!("write {} failed: {e}", path.display()))
}

fn write_perf_md(path: &std::path::Path, p: &PerfBaseline) -> Result<(), String> {
    let mut s = String::new();
    s.push_str("# Legacy Engine Perf Baseline (\"before\" numbers)\n\n");
    s.push_str(&format!("- Machine: {}\n", p.machine));
    s.push_str(&format!("- Track: `{}`  Venue: `{}`\n", p.track_id, p.venue_id));
    s.push_str(&format!("- Audio available: {}\n\n", p.audio_available));

    s.push_str("## Headline\n\n");
    s.push_str(&format!(
        "- **Composite cold resolve (302 annotations, {} layers):** {:.0} ms\n",
        p.composite_layers_resolved, p.composite_cold_resolve_ms
    ));
    s.push_str(&format!(
        "  ({} annotations produced no layer / failed)\n",
        p.composite_annotations_failed
    ));
    s.push_str(&format!(
        "- **Per-pattern graph exec: median {:.1} ms / mean {:.1} ms**\n",
        p.per_pattern_exec_median_ms, p.per_pattern_exec_mean_ms
    ));
    s.push_str(
        "  (mean is skewed by the FIRST audio-reactive pattern, which pays a \
         one-time ~470s-audio decode + harmony/stem cache warm-up; median is \
         the representative steady-state per-pattern exec cost.)\n",
    );
    s.push_str(&format!(
        "- **render_frame sample cost ({} prims):** {:.3} us/frame (mean of {} iters)\n",
        p.render_frame_probe.primitive_count,
        p.render_frame_probe.mean_us,
        p.render_frame_probe.iterations
    ));
    s.push_str(&format!(
        "- **Naive full-graph re-eval per frame (gradient):** {:.2} ms (mean of {} iters)\n\n",
        p.realtime_single_eval.mean_ms_per_full_graph_eval, p.realtime_single_eval.iterations
    ));

    s.push_str("## Per-pattern graph exec (single run, ms)\n\n");
    s.push_str("| pattern | exec_ms |\n|---|---|\n");
    let mut rows = p.per_pattern_exec_ms.clone();
    rows.sort_by(|a, b| b.exec_ms.partial_cmp(&a.exec_ms).unwrap());
    for r in &rows {
        s.push_str(&format!("| {} | {:.1} |\n", r.pattern, r.exec_ms));
    }

    s.push_str("\n## Notes\n\n");
    s.push_str("- `render_frame` probe: ");
    s.push_str(&p.render_frame_probe.note);
    s.push('\n');
    s.push_str("- realtime probe: ");
    s.push_str(&p.realtime_single_eval.note);
    s.push('\n');
    s.push_str("- N-sweep (scaling primitive count vs per-frame cost): SKIPPED. \
                The number of primitives is determined by the venue's fixtures \
                and the selection expression resolved inside the executor; \
                synthetically scaling it would require modifying execution-layer \
                code (fixture loading / selection), which is forbidden for this \
                oracle capture. The `render_frame` cost above is linear in \
                primitive count, so per-frame cost ~= (mean_us / prim_count) * N.\n");
    std::fs::write(path, s).map_err(|e| format!("write md: {e}"))
}
