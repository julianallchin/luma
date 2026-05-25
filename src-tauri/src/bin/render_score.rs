//! Headless score renderer.
//!
//! Takes a render plan JSON (list of pattern annotations with absolute
//! seconds, z-index, blend mode, and arg values) and a venue id; produces
//! per-tick `UniverseState` frames as JSONL on stdout (or to --output).
//!
//! Mirrors `compositor::composite_track` but without any Tauri AppHandle
//! / live render-engine plumbing. Each annotation's pattern graph is run
//! once via `run_graph_internal`; the resulting LayerTimeSeries are
//! composited in z order via `composite_layer_frame` at each frame.
//!
//! Audio-driven nodes (`stem_splitter`, `harmony_analysis`, etc.) will
//! receive silence because we pass `shared_audio: None`. Patterns that
//! depend on the audio waveform itself (`kick_intensity`, `bass_strobe`,
//! etc.) will collapse to their default/silent state. This is a v0
//! limitation — wire in `SharedAudioContext` from a track's mp3/ogg when
//! these patterns become important to render.
//!
//! Usage:
//!     render_score --plan PATH --output PATH [--fps 30] [--db PATH]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use luma_lib::audio::stem_cache::StemCache;
use luma_lib::audio::{decode_track_samples, stereo_to_mono, FftService};
use luma_lib::engine::{composite_layer_frame, render_frame};
use luma_lib::models::node_graph::{BeatGrid, BlendMode, Graph, GraphContext, LayerTimeSeries};
use luma_lib::models::universe::UniverseState;
use luma_lib::node_graph::{run_graph_internal, GraphExecutionConfig, SharedAudioContext};

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[derive(Deserialize)]
struct RenderPlan {
    venue_id: String,
    track_id: String,
    duration_sec: f32,
    beat_grid: Option<BeatGrid>,
    annotations: Vec<PlanAnnotation>,
    /// Absolute path to the original audio file. When present we decode
    /// and inject as `SharedAudioContext` so audio-driven nodes (kick
    /// intensity, bass strobe, stems, harmony) actually react. Without
    /// this, those nodes return silence — multiply-blended reactive
    /// layers will collapse the foundation to black.
    audio_path: Option<String>,
    /// Hash used by audio-aware nodes to key their per-track caches
    /// (stems, etc.). Pass the same hash we use in luma-lighting-model;
    /// audio-cache hits won't happen in headless mode but the field is
    /// required by `SharedAudioContext`.
    #[serde(default)]
    track_hash: Option<String>,
}

#[derive(Deserialize)]
struct PlanAnnotation {
    start_sec: f32,
    end_sec: f32,
    z: i32,
    blend_mode: BlendMode,
    pattern_id: String,
    #[serde(default)]
    args: HashMap<String, serde_json::Value>,
}

struct ResolvedLayer {
    z: i32,
    blend_mode: BlendMode,
    start_sec: f32,
    end_sec: f32,
    pattern_id: String,
    layer: LayerTimeSeries,
}

#[derive(Serialize)]
struct FrameOut<'a> {
    t: f32,
    primitives: &'a HashMap<String, luma_lib::models::universe::PrimitiveState>,
}

struct Args {
    plan: PathBuf,
    output: Option<PathBuf>,
    db_path: PathBuf,
    fixtures_root: PathBuf,
    fps: f32,
}

fn parse_args() -> Result<Args, String> {
    let mut plan: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut db_path: Option<PathBuf> = None;
    let mut fixtures_root: Option<PathBuf> = None;
    let mut fps: f32 = 30.0;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--plan" => plan = it.next().map(PathBuf::from),
            "--output" | "-o" => output = it.next().map(PathBuf::from),
            "--db" => db_path = it.next().map(PathBuf::from),
            "--fixtures-root" => fixtures_root = it.next().map(PathBuf::from),
            "--fps" => {
                fps = it
                    .next()
                    .ok_or("--fps needs a value")?
                    .parse::<f32>()
                    .map_err(|e| format!("bad --fps: {e}"))?;
            }
            "--help" | "-h" => {
                eprintln!("usage: render_score --plan PATH [--output PATH] [--fps 30] [--db PATH] [--fixtures-root PATH]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    Ok(Args {
        plan: plan.ok_or("--plan is required")?,
        output,
        db_path: db_path.unwrap_or_else(default_luma_db),
        fixtures_root: fixtures_root.unwrap_or_else(default_fixtures_root),
        fps,
    })
}

fn default_luma_db() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/com.luma.luma/luma.db")
}

fn default_fixtures_root() -> PathBuf {
    // Matches the QLC+ fixture library Luma bundles. Override with
    // --fixtures-root if the dump moves or you have a different version.
    PathBuf::from("/Users/julian/github/luma/resources/fixtures/2511260420")
}

async fn fetch_pattern_graph(pool: &sqlx::SqlitePool, pattern_id: &str) -> Result<String, String> {
    // Mirrors `crate::database::local::patterns::get_pattern_graph_pool` —
    // the user-edited implementation lives in a private module so we run
    // the query inline.
    let row: Option<String> = sqlx::query_scalar(
        "SELECT graph_json FROM implementations
         WHERE pattern_id = ? ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(pattern_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("pattern graph query failed: {e}"))?;
    row.ok_or_else(|| format!("no implementation for pattern {pattern_id}"))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), String> {
    let args = parse_args()?;

    let plan_text =
        std::fs::read_to_string(&args.plan).map_err(|e| format!("read plan failed: {e}"))?;
    let plan: RenderPlan =
        serde_json::from_str(&plan_text).map_err(|e| format!("parse plan failed: {e}"))?;

    if !args.db_path.exists() {
        return Err(format!("luma DB not found at {}", args.db_path.display()));
    }
    let connect = SqliteConnectOptions::new()
        .filename(&args.db_path)
        .read_only(true)
        .immutable(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(connect)
        .await
        .map_err(|e| format!("connect failed: {e}"))?;

    let stem_cache = StemCache::new();
    let fft_service = FftService::new();

    // Find Luma's track_id + track_hash so stem-based nodes
    // (`kick_intensity`, `bass_strobe`, `stem_splitter`, `harmony_analysis`)
    // can locate precomputed stems via the DB. We can't match by hash
    // because luma-lighting-model uses BLAKE2b-128 while Luma stores
    // SHA256 — fall back to file_path which we already know.
    let (effective_track_id, effective_track_hash): (String, String) = {
        let mut tid: Option<String> = None;
        let mut th: Option<String> = None;
        if let Some(audio_path) = plan.audio_path.as_deref() {
            let row: Option<(String, String)> =
                sqlx::query_as("SELECT id, track_hash FROM tracks WHERE file_path = ? LIMIT 1")
                    .bind(audio_path)
                    .fetch_optional(&pool)
                    .await
                    .map_err(|e| format!("track lookup failed: {e}"))?;
            if let Some((id, hash)) = row {
                tid = Some(id);
                th = Some(hash);
            }
        }
        (
            tid.unwrap_or_else(|| plan.track_id.clone()),
            th.unwrap_or_else(|| plan.track_hash.clone().unwrap_or_default()),
        )
    };
    eprintln!(
        "track_id={} track_hash={}",
        effective_track_id, effective_track_hash,
    );

    // Decode the audio file once and reuse across every annotation so
    // audio-driven nodes (kick_intensity, bass_strobe, stem_splitter,
    // harmony_analysis) have something to react to.
    let shared_audio: Option<SharedAudioContext> = match plan.audio_path.as_deref() {
        Some(p) => {
            eprintln!("decoding audio: {p}");
            let decoded = decode_track_samples(std::path::Path::new(p), None)
                .map_err(|e| format!("audio decode failed: {e}"))?;
            // Decoder returns stereo interleaved at 48kHz; mono helps the
            // bandpass / stem-splitter nodes which assume single-channel.
            let mono = stereo_to_mono(&decoded.samples);
            eprintln!(
                "audio: {:.1}s mono @ {}Hz",
                mono.len() as f32 / decoded.sample_rate as f32,
                decoded.sample_rate,
            );
            Some(SharedAudioContext {
                track_id: effective_track_id.clone(),
                track_hash: effective_track_hash.clone(),
                samples: Arc::new(mono),
                sample_rate: decoded.sample_rate,
            })
        }
        None => {
            eprintln!("no audio_path provided — audio-driven nodes will see silence");
            None
        }
    };

    eprintln!(
        "rendering {} annotation(s), duration={:.1}s fps={:.0}",
        plan.annotations.len(),
        plan.duration_sec,
        args.fps,
    );

    // 1. Resolve each annotation → LayerTimeSeries via run_graph_internal.
    let mut layers: Vec<ResolvedLayer> = Vec::with_capacity(plan.annotations.len());
    for (i, ann) in plan.annotations.iter().enumerate() {
        let graph_json = fetch_pattern_graph(&pool, &ann.pattern_id).await?;
        let graph: Graph = serde_json::from_str(&graph_json)
            .map_err(|e| format!("parse graph for {} failed: {e}", ann.pattern_id))?;

        // Use the agent's args verbatim — they were validated against the
        // pattern's arg schema at place-time.
        let ctx = GraphContext {
            track_id: effective_track_id.clone(),
            venue_id: plan.venue_id.clone(),
            start_time: ann.start_sec,
            end_time: ann.end_sec,
            beat_grid: plan.beat_grid.clone(),
            arg_values: Some(ann.args.clone()),
            instance_seed: Some(0xC0DE_C0DE), // deterministic for re-renders
        };
        let config = GraphExecutionConfig {
            compute_visualizations: false,
            log_summary: false,
            log_primitives: false,
            shared_audio: shared_audio.clone(),
        };
        let (_run, layer_opt) = run_graph_internal(
            &pool,
            Some(&pool), // project_pool — same DB
            &stem_cache,
            &fft_service,
            Some(args.fixtures_root.clone()), // for GDTF lookups
            graph,
            ctx,
            config,
        )
        .await
        .map_err(|e| format!("graph exec for ann[{i}] ({}) failed: {e}", ann.pattern_id))?;

        if let Some(layer) = layer_opt {
            layers.push(ResolvedLayer {
                z: ann.z,
                blend_mode: ann.blend_mode,
                start_sec: ann.start_sec,
                end_sec: ann.end_sec,
                pattern_id: ann.pattern_id.clone(),
                layer,
            });
        } else {
            eprintln!(
                "  ann[{i}] {} produced no layer (no primitive outputs)",
                ann.pattern_id
            );
        }
        eprintln!("  resolved [{}/{}]", i + 1, plan.annotations.len());
    }

    // 2. Sort layers by z (low first so we composite bottom→top).
    layers.sort_by_key(|l| l.z);

    // 3. Tick the timeline and emit one JSONL frame per tick.
    let dt = 1.0_f32 / args.fps;
    let total_frames = (plan.duration_sec / dt).ceil() as usize + 1;

    let mut out: Box<dyn std::io::Write> = match &args.output {
        Some(p) => Box::new(std::io::BufWriter::new(
            std::fs::File::create(p).map_err(|e| format!("open output failed: {e}"))?,
        )),
        None => Box::new(std::io::BufWriter::new(std::io::stdout())),
    };

    for f in 0..total_frames {
        let t = f as f32 * dt;
        let mut state = UniverseState {
            primitives: HashMap::new(),
        };
        for layer in &layers {
            // Skip layers whose time range doesn't cover this tick — the
            // graph executor already emitted samples only inside the
            // annotation's window, but layers placed at unrelated times
            // shouldn't bleed across.
            if t < layer.start_sec || t > layer.end_sec {
                continue;
            }
            composite_layer_frame(&mut state, &layer.layer, t, layer.blend_mode, 1.0, None);
        }
        let frame = FrameOut {
            t,
            primitives: &state.primitives,
        };
        serde_json::to_writer(&mut out, &frame).map_err(|e| format!("write frame failed: {e}"))?;
        writeln!(&mut out).map_err(|e| format!("write newline failed: {e}"))?;
    }
    out.flush().map_err(|e| format!("flush failed: {e}"))?;

    eprintln!(
        "wrote {} frame(s) ({:.1}s @ {:.0} fps) from {} layer(s)",
        total_frames,
        plan.duration_sec,
        args.fps,
        layers.len()
    );
    let _ = render_frame; // silence unused-import if not directly needed
    Ok(())
}
