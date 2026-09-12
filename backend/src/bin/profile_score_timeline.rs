//! Full-timeline, read-only score replay through the production evaluator and renderer.
//!
//! The default workload is the Gasworks Park / Get Lucky reference used by the
//! native renderer harness. It samples the lead-in plus bars 1 through 41 and
//! writes per-frame evidence and per-bar distributions without exporting scene
//! descriptions or images.

use luma_lib::{
    eval::{Arena, Scope},
    stage_render::{primitive_state, VenueGeometry},
    storage::StorageRoot,
};
use luma_render::{
    assets::Library, build_frame_with, coords::three_from_world, scene_desc::RenderSettings,
    Catalogue, Frame, FrameTimings, Renderer,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row,
};
use std::{
    collections::BTreeMap,
    env,
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, UNIX_EPOCH},
};

const REFERENCE_SCORE: &str = "7de2624c-6095-8ae1-a7bf-c9496baae2d8";
const REFERENCE_CAMERA_CASE: &str = "gasworks-get-lucky-3s-close";
const DEFAULT_RATE_HZ: f64 = 60.0;
const DEFAULT_WARMUP_FRAMES: u32 = 60;
const DEFAULT_END_BAR: usize = 41;
const BUDGET_75_HZ_MS: f64 = 1000.0 / 75.0;
const BUDGET_50_HZ_MS: f64 = 20.0;

#[derive(Debug)]
struct Arguments {
    score: String,
    database: PathBuf,
    storage_root: StorageRoot,
    output: PathBuf,
    start: f64,
    end: Option<f64>,
    rate_hz: f64,
    width: Option<u32>,
    height: Option<u32>,
    warmup_frames: u32,
    camera_suite: PathBuf,
    camera_case: String,
    capture_dir: Option<PathBuf>,
    capture_start: Option<f64>,
    capture_end: Option<f64>,
}

#[derive(Debug)]
struct CaptureWindow {
    directory: PathBuf,
    staging_directory: PathBuf,
    start_seconds: f64,
    end_seconds: f64,
    first_index: usize,
    end_index: usize,
    rgba_bytes_per_frame: usize,
}

impl Drop for CaptureWindow {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.staging_directory);
    }
}

impl CaptureWindow {
    fn contains(&self, index: usize) -> bool {
        (self.first_index..self.end_index).contains(&index)
    }

    fn frame_count(&self) -> usize {
        self.end_index - self.first_index
    }
}

#[derive(Debug, Serialize)]
struct CapturedFrame {
    frame_index: usize,
    time_seconds: f64,
    score_time_f32_bits: u32,
    image: String,
    png_bytes: u64,
    png_sha256: String,
}

#[derive(Debug, Deserialize)]
struct CameraSuite {
    catalogue: PathBuf,
    width: u32,
    height: u32,
    cases: Vec<CameraCase>,
}

#[derive(Debug, Deserialize)]
struct CameraCase {
    id: String,
    scene: String,
    eye: [f32; 3],
    target: [f32; 3],
    fov: f32,
}

#[derive(Debug)]
struct ReferenceView {
    case: CameraCase,
    settings: RenderSettings,
    catalogue_path: PathBuf,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Serialize)]
struct Distribution {
    samples: usize,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    max_ms: f64,
    over_13_333_ms: usize,
    over_20_ms: usize,
}

#[derive(Debug, Clone, Serialize)]
struct FrameRow {
    index: usize,
    time_seconds: f64,
    bar: usize,
    score_eval_ms: f64,
    frame_build_ms: f64,
    renderer_cpu_ms: f64,
    cpu_work_ms: f64,
    gpu_ms: f64,
    budget_estimate_ms: f64,
    profiler_wall_ms: f64,
    gpu: Value,
    renderer_cpu: Value,
    fixture_cones: usize,
    opaque_draws: usize,
    transparent_draws: usize,
    shadow: Value,
    interval_cache: Value,
    compact: Value,
}

#[derive(Default)]
struct Bin {
    start_seconds: f64,
    end_seconds: f64,
    frames: Vec<FrameRow>,
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let repository = repository_root()?;
    let args = Arguments::parse(&repository)?;
    let reference = load_reference_view(&args.camera_suite, &args.camera_case)?;
    let width = args.width.unwrap_or(reference.width);
    let height = args.height.unwrap_or(reference.height);
    validate_dimensions(width, height)?;

    let database_sha256 = sha256_file(&args.database)?;
    let database_metadata = fs::metadata(&args.database).map_err(|error| {
        format!(
            "cannot inspect database {}: {error}",
            args.database.display()
        )
    })?;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&args.database)
                .read_only(true),
        )
        .await
        .map_err(|error| format!("cannot open read-only database: {error}"))?;

    let score = sqlx::query(
        "SELECT s.track_id, s.venue_id, s.name AS score_name, s.updated_at AS score_updated_at, \
         t.title, t.artist, t.duration_seconds, t.updated_at AS track_updated_at, \
         v.name AS venue_name, v.updated_at AS venue_updated_at \
         FROM scores s JOIN tracks t ON t.id = s.track_id \
         JOIN venues v ON v.id = s.venue_id WHERE s.id = ?",
    )
    .bind(&args.score)
    .fetch_one(&pool)
    .await
    .map_err(|error| format!("cannot load score {}: {error}", args.score))?;
    let track_id: String = score.get("track_id");
    let venue_id: String = score.get("venue_id");
    let beat_grid = luma_lib::services::tracks::get_track_beats(&pool, &track_id)
        .await?
        .ok_or_else(|| format!("track {track_id} has no beat grid"))?;
    if beat_grid.beats_per_bar <= 0 {
        return Err("beat grid beats_per_bar must be positive".into());
    }
    if beat_grid.downbeats.len() <= DEFAULT_END_BAR {
        return Err(format!(
            "beat grid has {} downbeats; bar {} needs boundary {}",
            beat_grid.downbeats.len(),
            DEFAULT_END_BAR,
            DEFAULT_END_BAR + 1
        ));
    }
    validate_grid(&beat_grid.downbeats)?;
    let default_end = f64::from(beat_grid.downbeats[DEFAULT_END_BAR]);
    let end = args.end.unwrap_or(default_end);
    if !args.start.is_finite() || !end.is_finite() || args.start < 0.0 || end <= args.start {
        return Err("profile range must be finite, nonnegative, and have end > start".into());
    }
    let sample_times = sample_times(args.start, end, args.rate_hz);
    let capture = capture_window(&args, end, &sample_times, width, height)?;
    let mut capture_pixels = Vec::new();
    if let Some(capture) = &capture {
        capture_pixels
            .try_reserve_exact(capture.rgba_bytes_per_frame)
            .map_err(|error| {
                format!(
                    "cannot reserve {} RGBA bytes for one capture frame: {error}",
                    capture.rgba_bytes_per_frame
                )
            })?;
        if capture.directory.exists() {
            return Err(format!(
                "capture directory {} already exists",
                capture.directory.display()
            ));
        }
        fs::create_dir(&capture.staging_directory).map_err(|error| {
            format!(
                "cannot create capture staging directory {}: {error}",
                capture.staging_directory.display()
            )
        })?;
    }

    let fixtures = luma_lib::headless_host::HostConfig::default().fixtures_root()?;
    let program =
        luma_lib::build_score_scene(&pool, &args.storage_root, &fixtures, &args.score, None)
            .await?;
    let geometry = VenueGeometry::load(&pool, &fixtures, &venue_id).await?;
    let (mut scene, definitions) = geometry.scene();
    scene.render = reference.settings.clone();
    scene.camera.position = three_from_world(reference.case.eye.into()).to_array();
    scene.camera.target = three_from_world(reference.case.target.into()).to_array();
    scene.render.fov = reference.case.fov;
    let mut library = Library::new(luma_lib::stage_render::meshes_root(Some(&fixtures)));
    let mut renderer = Renderer::new_profiled().map_err(|error| error.to_string())?;
    let mut arena = Arena::default();

    for _ in 0..args.warmup_frames {
        let state = program
            .render(&[args.start as f32], Scope::Composite, &mut arena)
            .pop()
            .ok_or("missing warmup score frame")?;
        let frame = build_frame_with(
            &scene,
            &definitions,
            &|id, head| primitive_state(Some(&state), id, head),
            args.start as f32,
            &mut library,
        )
        .map_err(|error| error.to_string())?;
        profile_frame(
            &mut renderer,
            &frame,
            width,
            height,
            capture.is_some(),
            &mut capture_pixels,
        )?;
        validate_capture_pixels(capture.as_ref(), &capture_pixels)?;
    }

    let sample_count = sample_times.len();
    let mut bins = bar_bins(&beat_grid.downbeats, args.start, end);
    let mut all = Vec::with_capacity(sample_count);
    let mut captured_frames = Vec::with_capacity(capture.as_ref().map_or(0, |c| c.frame_count()));
    let mut captured_png_bytes = 0_u64;
    let mut reported_bar = None;
    let run_started = Instant::now();
    for (index, time) in sample_times.into_iter().enumerate() {
        let bar = bar_at(time, &beat_grid.downbeats);
        if reported_bar != Some(bar) {
            eprintln!("profiling bar {bar} at {time:.3}s ({index}/{sample_count} frames)");
            reported_bar = Some(bar);
        }
        let started = Instant::now();
        let state = program
            .render(&[time as f32], Scope::Composite, &mut arena)
            .pop()
            .ok_or("missing score frame")?;
        let score_eval_ms = milliseconds(started.elapsed());

        let started = Instant::now();
        let frame = build_frame_with(
            &scene,
            &definitions,
            &|id, head| primitive_state(Some(&state), id, head),
            time as f32,
            &mut library,
        )
        .map_err(|error| error.to_string())?;
        let frame_build_ms = milliseconds(started.elapsed());
        let fixture_cones = frame.fixture_cones.len();
        let transparent_draws = frame.transparent.len();
        let opaque_draws = frame.draws.len().saturating_sub(transparent_draws);

        let started = Instant::now();
        let timing = profile_frame(
            &mut renderer,
            &frame,
            width,
            height,
            capture.is_some(),
            &mut capture_pixels,
        )?;
        validate_capture_pixels(capture.as_ref(), &capture_pixels)?;
        let profiler_wall_ms = milliseconds(started.elapsed());
        if let Some(capture) = capture.as_ref().filter(|capture| capture.contains(index)) {
            let image = format!("frame-{index:06}.png");
            let path = capture.staging_directory.join(&image);
            let encoded = luma_render::image_out::encode(&capture_pixels, width, height)
                .map_err(|error| format!("cannot encode {}: {error}", path.display()))?;
            let png_sha256 = sha256_bytes(&encoded);
            let png_bytes = u64::try_from(encoded.len())
                .map_err(|_| "encoded PNG length does not fit u64".to_owned())?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
            output
                .write_all(&encoded)
                .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
            captured_png_bytes = captured_png_bytes
                .checked_add(png_bytes)
                .ok_or("captured PNG byte count overflow")?;
            captured_frames.push(CapturedFrame {
                frame_index: index,
                time_seconds: time,
                score_time_f32_bits: (time as f32).to_bits(),
                image,
                png_bytes,
                png_sha256,
            });
        }
        let renderer_cpu_ms = timing.cpu_encode_submit_ms;
        let cpu_work_ms = score_eval_ms + frame_build_ms + renderer_cpu_ms;
        let budget_estimate_ms = cpu_work_ms + timing.gpu_total_ms;
        let row = FrameRow {
            index,
            time_seconds: time,
            bar,
            score_eval_ms,
            frame_build_ms,
            renderer_cpu_ms,
            cpu_work_ms,
            gpu_ms: timing.gpu_total_ms,
            budget_estimate_ms,
            profiler_wall_ms,
            gpu: json!({
                "scene_ms": timing.gpu_scene_ms,
                "volumetric_ms": timing.gpu_volumetric_ms,
                "composite_ms": timing.gpu_composite_ms,
                "light_index_ms": timing.gpu_index_ms,
                "fog_grid_ms": timing.gpu_fog_grid_ms,
                "fog_prepare_ms": timing.gpu_fog_prepare_ms,
                "fog_light_ms": timing.gpu_fog_light_ms,
                "fog_integrate_ms": timing.gpu_fog_integrate_ms,
                "passes": timing.passes,
            }),
            renderer_cpu: json!({
                "prepare_ms": milliseconds(timing.cpu.prepare),
                "clusters_ms": milliseconds(timing.cpu.clusters),
                "upload_ms": milliseconds(timing.cpu.upload),
                "targets_ms": milliseconds(timing.cpu.targets),
                "encode_ms": milliseconds(timing.cpu.encode),
                "total_ms": milliseconds(timing.cpu.total),
                "fixture_shadows": {
                    "globals_ms": milliseconds(timing.cpu.fixture_shadows.globals),
                    "cull_ms": milliseconds(timing.cpu.fixture_shadows.cull),
                    "buckets_ms": milliseconds(timing.cpu.fixture_shadows.buckets),
                    "resources_ms": milliseconds(timing.cpu.fixture_shadows.resources),
                    "maps_ms": milliseconds(timing.cpu.fixture_shadows.maps),
                    "hierarchy_ms": milliseconds(timing.cpu.fixture_shadows.hierarchy),
                },
                "submission": {
                    "staging_ms": milliseconds(timing.cpu.submission.staging),
                    "finish_ms": milliseconds(timing.cpu.submission.finish),
                    "submit_ms": milliseconds(timing.cpu.submission.submit),
                },
            }),
            fixture_cones,
            opaque_draws,
            transparent_draws,
            shadow: serde_json::to_value(renderer.shadow_stats()).map_err(|e| e.to_string())?,
            interval_cache: serde_json::to_value(renderer.interval_cache_stats())
                .map_err(|e| e.to_string())?,
            compact: serde_json::to_value(renderer.compact_stats()).map_err(|e| e.to_string())?,
        };
        if let Some(bin) = bins.get_mut(&bar) {
            bin.frames.push(row.clone());
        }
        all.push(row);
    }
    let elapsed_seconds = run_started.elapsed().as_secs_f64();
    eprintln!("profiled {sample_count} frames in {elapsed_seconds:.1}s");

    let adapter = renderer.gpu().adapter_profile();
    let bar_summaries: Vec<Value> = bins
        .into_iter()
        .map(|(bar, bin)| {
            json!({
                "bar": bar,
                "start_seconds": bin.start_seconds,
                "end_seconds": bin.end_seconds,
                "summary": summaries(&bin.frames),
            })
        })
        .collect();
    let database_modified_unix_seconds = database_metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());
    let source_path = repository.join("backend/src/bin/profile_score_timeline.rs");
    let renderer_path = repository.join("gpui/crates/render/src/gpu.rs");
    let executable = env::current_exe().map_err(|error| error.to_string())?;
    let camera_suite_sha256 = sha256_file(&args.camera_suite)?;
    let camera_catalogue_sha256 = sha256_file(&reference.catalogue_path)?;
    let executable_sha256 = sha256_file(&executable)?;
    let profiler_source_sha256 = sha256_file(&source_path)?;
    let renderer_source_sha256 = sha256_file(&renderer_path)?;
    let renderer_environment = renderer_environment();
    let score_content = score_content_provenance(&pool, &args.score).await?;
    let capture_summary = capture.as_ref().map(|capture| {
        json!({
            "directory": capture.directory,
            "manifest": capture.directory.join("capture-manifest.json"),
            "start_seconds": capture.start_seconds,
            "end_seconds": capture.end_seconds,
            "end_exclusive": true,
            "first_frame_index": capture.first_index,
            "frame_count": capture.frame_count(),
            "rgba_bytes_per_frame": capture.rgba_bytes_per_frame,
            "raw_rgba_bytes_selected": capture.rgba_bytes_per_frame * capture.frame_count(),
            "png_bytes_written": captured_png_bytes,
            "timing_eligible": false,
        })
    });
    let artifact = json!({
        "scenario": "read-only saved-score replay through production score evaluation, frame assembly, and serial profiled live-surface rendering",
        "captured_at_utc": chrono::Utc::now().to_rfc3339(),
        "score": {
            "id": args.score,
            "name": score.try_get::<Option<String>, _>("score_name").unwrap_or(None),
            "updated_at": score.get::<String, _>("score_updated_at"),
            "track": {
                "id": track_id,
                "title": score.try_get::<Option<String>, _>("title").unwrap_or(None),
                "artist": score.try_get::<Option<String>, _>("artist").unwrap_or(None),
                "duration_seconds": score.try_get::<Option<f64>, _>("duration_seconds").unwrap_or(None),
                "updated_at": score.get::<String, _>("track_updated_at"),
            },
            "venue": {
                "id": venue_id,
                "name": score.get::<String, _>("venue_name"),
                "updated_at": score.get::<String, _>("venue_updated_at"),
            },
        },
        "range": {
            "start_seconds": args.start,
            "end_seconds": end,
            "end_exclusive": true,
            "default_end_bar": DEFAULT_END_BAR,
            "sample_rate_hz": args.rate_hz,
            "sample_count": all.len(),
            "warmup_frames_at_start": args.warmup_frames,
        },
        "beat_grid": {
            "bpm": beat_grid.bpm,
            "beats_per_bar": beat_grid.beats_per_bar,
            "downbeat_offset": beat_grid.downbeat_offset,
            "beats": beat_grid.beats,
            "downbeats": beat_grid.downbeats,
            "bar_numbering": "bar 0 is the lead-in before the first detected downbeat; bar 1 begins at downbeats[0]",
        },
        "viewport": {"width": width, "height": height, "device_scale_factor": 1},
        "camera": {
            "suite": args.camera_suite,
            "case": reference.case.id,
            "source_scene": reference.case.scene,
            "eye": reference.case.eye,
            "target": reference.case.target,
            "fov_y_degrees": reference.case.fov,
        },
        "render_settings": scene.render,
        "live_subframes": luma_render::LIVE_SUBFRAMES,
        "adapter": {
            "name": adapter.name,
            "backend": adapter.backend,
            "device_type": adapter.device_type,
            "driver": adapter.driver,
            "driver_info": adapter.driver_info,
            "timestamp_query_supported": adapter.timestamp_query_supported,
            "timestamp_period_ns": adapter.timestamp_period_ns,
        },
        "measurement": {
            "timing_eligible": capture.is_none(),
            "budget_estimate_formula": "score_eval_ms + frame_build_ms + renderer_cpu_ms + gpu_ms",
            "budget_estimate_meaning": "conservative serial CPU+GPU work estimate; it is neither presented frame interval nor displayed FPS",
            "profiler_wall_meaning": "blocking live-surface render and strict timestamp completion; excludes score evaluation and frame assembly",
            "renderer_output": if capture.is_some() {
                "constant RGBA byte destination for every warmup and sampled frame; selected PNG encoding is interleaved after blocking completion"
            } else {
                "constant production compositor destination; no image or scene export is interleaved"
            },
            "capture_timing_exclusion": capture.is_some().then_some("all timing summaries are diagnostic-only because byte readback and selected PNG writes alter the run"),
            "detailed_gpu_passes": env::var_os("LUMA_PROFILE_DETAIL").is_some_and(|value| value == "1"),
            "budget_thresholds_ms": [BUDGET_75_HZ_MS, BUDGET_50_HZ_MS],
            "run_wall_seconds": elapsed_seconds,
        },
        "summary": summaries(&all),
        "fog_visibility_cache": renderer
            .fog_visibility_cache_stats()
            .map_err(|error| error.to_string())?,
        "bars": bar_summaries,
        "image_capture": capture_summary,
        "provenance": {
            "database": {
                "path": args.database,
                "read_only": true,
                "bytes": database_metadata.len(),
                "modified_unix_seconds": database_modified_unix_seconds,
                "sha256": database_sha256,
            },
            "score_content": score_content,
            "storage_root": args.storage_root.path(),
            "fixtures_root": fixtures,
            "camera_suite_sha256": camera_suite_sha256,
            "camera_catalogue": reference.catalogue_path,
            "camera_catalogue_sha256": camera_catalogue_sha256,
            "git_head": command_text(&repository, "git", &["rev-parse", "HEAD"]),
            "git_worktree_dirty": !command_success(&repository, "git", &["diff", "--quiet", "HEAD", "--"]),
            "rustc": command_text(&repository, "rustc", &["+1.97.1", "-Vv"]),
            "executable": executable,
            "executable_sha256": executable_sha256,
            "debug_assertions": cfg!(debug_assertions),
            "runtime_profiler_source_sha256": profiler_source_sha256,
            "runtime_renderer_source_sha256": renderer_source_sha256,
            "renderer_environment": renderer_environment,
            "arguments": env::args().collect::<Vec<_>>(),
        },
        "frames": all,
    });
    let artifact_bytes = serde_json::to_vec_pretty(&artifact).map_err(|error| error.to_string())?;
    if let Some(capture) = &capture {
        if captured_frames.len() != capture.frame_count() {
            return Err(format!(
                "captured {} frames but the validated window selected {}",
                captured_frames.len(),
                capture.frame_count()
            ));
        }
        let capture_manifest = json!({
            "schema": "luma-profile-score-history-capture-v1",
            "timing_eligible": false,
            "history": {
                "destination": "Bytes(RGBA) for every warmup and sampled frame",
                "warmup_frames_at_run_start": args.warmup_frames,
                "prefix_start_seconds": args.start,
                "sample_rate_hz": args.rate_hz,
                "no_extra_rendered_frames": true,
                "camera_installed_before_every_frame_build": true,
                "haze_medium_time": "deterministic sampled score time; not live-app wall-clock haze age",
                "selected_window_start_seconds": capture.start_seconds,
                "selected_window_end_seconds": capture.end_seconds,
                "selected_window_end_exclusive": true,
            },
            "source": {
                "profile_output": args.output,
                "profile_output_sha256": sha256_bytes(&artifact_bytes),
                "score_id": args.score,
                "score_content": score_content,
                "database": args.database,
                "database_sha256": database_sha256,
                "camera_suite": args.camera_suite,
                "camera_suite_sha256": camera_suite_sha256,
                "camera_case": reference.case.id,
                "camera_eye": reference.case.eye,
                "camera_target": reference.case.target,
                "camera_fov_y_degrees": reference.case.fov,
                "camera_catalogue": reference.catalogue_path,
                "camera_catalogue_sha256": camera_catalogue_sha256,
                "executable": executable,
                "executable_sha256": executable_sha256,
                "runtime_profiler_source_sha256": profiler_source_sha256,
                "runtime_renderer_source_sha256": renderer_source_sha256,
                "renderer_environment": renderer_environment,
            },
            "storage": {
                "width": width,
                "height": height,
                "rgba_bytes_per_frame": capture.rgba_bytes_per_frame,
                "raw_rgba_bytes_selected": capture.rgba_bytes_per_frame * capture.frame_count(),
                "png_bytes_written": captured_png_bytes,
            },
            "frames": captured_frames,
            "app_isolation": {
                "required": true,
                "proof": "external run_isolated.py metadata must show the foreground app was resumed after the command",
            },
        });
        let manifest_path = capture.staging_directory.join("capture-manifest.json");
        let mut manifest = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&manifest_path)
            .map_err(|error| format!("cannot create {}: {error}", manifest_path.display()))?;
        manifest
            .write_all(
                &serde_json::to_vec_pretty(&capture_manifest).map_err(|error| error.to_string())?,
            )
            .map_err(|error| format!("cannot write {}: {error}", manifest_path.display()))?;
        manifest
            .flush()
            .map_err(|error| format!("cannot flush {}: {error}", manifest_path.display()))?;
        fs::rename(&capture.staging_directory, &capture.directory).map_err(|error| {
            format!(
                "cannot publish complete capture {}: {error}",
                capture.directory.display()
            )
        })?;
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args.output)
        .map_err(|error| {
            format!(
                "cannot create new output {}: {error}",
                args.output.display()
            )
        })?;
    output
        .write_all(&artifact_bytes)
        .map_err(|error| format!("cannot write {}: {error}", args.output.display()))?;
    output
        .flush()
        .map_err(|error| format!("cannot flush {}: {error}", args.output.display()))?;
    println!("{}", args.output.display());
    Ok(())
}

impl Arguments {
    fn parse(repository: &Path) -> Result<Self, String> {
        let storage_root = match env::var_os("LUMA_CONFIG_DIR") {
            Some(path) => StorageRoot::from_path(path.into()),
            None => StorageRoot::from_env_default()?,
        };
        let mut parsed = Self {
            score: REFERENCE_SCORE.into(),
            database: storage_root.luma_db_path(),
            storage_root,
            output: PathBuf::from("profile-score-timeline.json"),
            start: 0.0,
            end: None,
            rate_hz: DEFAULT_RATE_HZ,
            width: None,
            height: None,
            warmup_frames: DEFAULT_WARMUP_FRAMES,
            camera_suite: repository.join("harness/perf/mac-gasworks-2026-09-09/native-suite.json"),
            camera_case: REFERENCE_CAMERA_CASE.into(),
            capture_dir: None,
            capture_start: None,
            capture_end: None,
        };
        let mut args = env::args().skip(1);
        while let Some(flag) = args.next() {
            let value = |args: &mut std::iter::Skip<env::Args>, flag: &str| {
                args.next()
                    .ok_or_else(|| format!("{flag} requires a value"))
            };
            match flag.as_str() {
                "--help" | "-h" => {
                    println!("{}", usage());
                    std::process::exit(0);
                }
                "--score" => parsed.score = value(&mut args, "--score")?,
                "--db" => parsed.database = value(&mut args, "--db")?.into(),
                "--storage-root" => {
                    parsed.storage_root =
                        StorageRoot::from_path(value(&mut args, "--storage-root")?.into());
                }
                "--output" => parsed.output = value(&mut args, "--output")?.into(),
                "--start" => parsed.start = parse_number(&value(&mut args, "--start")?, "start")?,
                "--end" => parsed.end = Some(parse_number(&value(&mut args, "--end")?, "end")?),
                "--rate" => parsed.rate_hz = parse_number(&value(&mut args, "--rate")?, "rate")?,
                "--width" => {
                    parsed.width = Some(parse_integer(&value(&mut args, "--width")?, "width")?)
                }
                "--height" => {
                    parsed.height = Some(parse_integer(&value(&mut args, "--height")?, "height")?)
                }
                "--warmup" => {
                    parsed.warmup_frames = parse_integer(&value(&mut args, "--warmup")?, "warmup")?;
                }
                "--camera" => parsed.camera_suite = value(&mut args, "--camera")?.into(),
                "--camera-case" => parsed.camera_case = value(&mut args, "--camera-case")?,
                "--capture-dir" => {
                    parsed.capture_dir = Some(value(&mut args, "--capture-dir")?.into())
                }
                "--capture-start" => {
                    parsed.capture_start = Some(parse_number(
                        &value(&mut args, "--capture-start")?,
                        "capture-start",
                    )?)
                }
                "--capture-end" => {
                    parsed.capture_end = Some(parse_number(
                        &value(&mut args, "--capture-end")?,
                        "capture-end",
                    )?)
                }
                other => return Err(format!("unknown argument {other}\n{}", usage())),
            }
        }
        if !parsed.rate_hz.is_finite() || !(1.0..=240.0).contains(&parsed.rate_hz) {
            return Err("rate must be finite and in 1..=240 Hz".into());
        }
        if parsed.warmup_frames > 600 {
            return Err("warmup must be in 0..=600 frames".into());
        }
        Ok(parsed)
    }
}

fn usage() -> &'static str {
    r#"profile_score_timeline [--score ID] [--db SNAPSHOT_DB] [--storage-root DIR]
     [--output JSON] [--start SEC] [--end SEC] [--rate HZ]
     [--width PX --height PX] [--warmup FRAMES]
     [--camera SUITE_JSON] [--camera-case CASE_ID]
     [--capture-dir DIR --capture-start SEC --capture-end SEC]

Defaults replay the Gasworks/Get Lucky harness score at 2227x1391 from 0 seconds
through the stored end of bar 41. Supply a SQLite backup with --db for an
immutable run input; the database is always opened read-only. Capture mode uses
one RGBA byte destination for the complete warmup and sampled prefix, streams
only selected frames to disk, and makes every timing in that run ineligible."#
}

fn parse_number(value: &str, name: &str) -> Result<f64, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {name}: {value}"))
}

fn parse_integer(value: &str, name: &str) -> Result<u32, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {name}: {value}"))
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err("dimensions must be in 1..=4096".into());
    }
    Ok(())
}

fn validate_grid(downbeats: &[f32]) -> Result<(), String> {
    if downbeats.is_empty()
        || downbeats.iter().any(|time| !time.is_finite())
        || downbeats.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err("downbeat grid must be finite and strictly increasing".into());
    }
    Ok(())
}

fn load_reference_view(path: &Path, case_id: &str) -> Result<ReferenceView, String> {
    let suite: CameraSuite = serde_json::from_slice(
        &fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?,
    )
    .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    let case = suite
        .cases
        .into_iter()
        .find(|case| case.id == case_id)
        .ok_or_else(|| format!("camera case {case_id} is absent from {}", path.display()))?;
    let catalogue_path = path
        .parent()
        .unwrap_or(Path::new("."))
        .join(&suite.catalogue);
    let catalogue = Catalogue::load(&catalogue_path).map_err(|error| error.to_string())?;
    let source_scene = catalogue
        .scenes
        .iter()
        .find(|scene| scene.id == case.scene)
        .ok_or_else(|| format!("camera source scene {} is absent", case.scene))?;
    Ok(ReferenceView {
        settings: source_scene.render.clone(),
        case,
        catalogue_path,
        width: suite.width,
        height: suite.height,
    })
}

fn capture_window(
    args: &Arguments,
    run_end: f64,
    sample_times: &[f64],
    width: u32,
    height: u32,
) -> Result<Option<CaptureWindow>, String> {
    let request = match (
        args.capture_dir.as_ref(),
        args.capture_start,
        args.capture_end,
    ) {
        (None, None, None) => return Ok(None),
        (Some(directory), Some(start), Some(end)) => (directory.clone(), start, end),
        _ => {
            return Err(
                "capture-dir, capture-start, and capture-end must be supplied together".into(),
            )
        }
    };
    let (directory, start, end) = request;
    if !start.is_finite() || !end.is_finite() || start < args.start || end > run_end || end <= start
    {
        return Err(format!(
            "capture range must be finite, nonempty, and within [{}, {run_end}]",
            args.start
        ));
    }
    let first_index = sample_times.partition_point(|time| *time < start);
    let end_index = sample_times.partition_point(|time| *time < end);
    let frame_count = end_index.saturating_sub(first_index);
    if frame_count == 0 {
        return Err("capture range selects no sampled frames".into());
    }
    let rgba_bytes_per_frame = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or("capture RGBA byte length overflow")?;
    let mut staging_name = directory.as_os_str().to_os_string();
    staging_name.push(format!(".partial-{}", std::process::id()));
    Ok(Some(CaptureWindow {
        directory,
        staging_directory: staging_name.into(),
        start_seconds: start,
        end_seconds: end,
        first_index,
        end_index,
        rgba_bytes_per_frame,
    }))
}

fn profile_frame(
    renderer: &mut Renderer,
    frame: &Frame,
    width: u32,
    height: u32,
    capture_mode: bool,
    pixels: &mut Vec<u8>,
) -> Result<FrameTimings, String> {
    if capture_mode {
        renderer
            .profile_live_into(frame, width, height, luma_render::LIVE_SUBFRAMES, pixels)
            .map_err(|error| error.to_string())
    } else {
        renderer
            .profile_live_surface(frame, width, height, luma_render::LIVE_SUBFRAMES)
            .map_err(|error| error.to_string())
    }
}

fn validate_capture_pixels(capture: Option<&CaptureWindow>, pixels: &[u8]) -> Result<(), String> {
    if let Some(capture) = capture {
        if pixels.len() != capture.rgba_bytes_per_frame {
            return Err(format!(
                "capture returned {} bytes; expected {} tightly packed RGBA bytes",
                pixels.len(),
                capture.rgba_bytes_per_frame
            ));
        }
    }
    Ok(())
}

fn sample_times(start: f64, end: f64, rate_hz: f64) -> Vec<f64> {
    let count = ((end - start) * rate_hz).ceil() as usize;
    (0..count)
        .map(|index| start + index as f64 / rate_hz)
        .take_while(|time| *time < end)
        .collect()
}

fn bar_at(time: f64, downbeats: &[f32]) -> usize {
    downbeats.partition_point(|downbeat| f64::from(*downbeat) <= time)
}

fn bar_bins(downbeats: &[f32], start: f64, end: f64) -> BTreeMap<usize, Bin> {
    let first = bar_at(start, downbeats);
    let last = bar_at(end.next_down(), downbeats);
    (first..=last)
        .map(|bar| {
            let natural_start = if bar == 0 {
                f64::NEG_INFINITY
            } else {
                f64::from(downbeats[bar - 1])
            };
            let natural_end = downbeats
                .get(bar)
                .map_or(f64::INFINITY, |value| f64::from(*value));
            (
                bar,
                Bin {
                    start_seconds: start.max(natural_start),
                    end_seconds: end.min(natural_end),
                    frames: Vec::new(),
                },
            )
        })
        .collect()
}

fn summaries(frames: &[FrameRow]) -> Value {
    json!({
        "score_eval": distribution(frames.iter().map(|frame| frame.score_eval_ms)),
        "frame_build": distribution(frames.iter().map(|frame| frame.frame_build_ms)),
        "renderer_cpu": distribution(frames.iter().map(|frame| frame.renderer_cpu_ms)),
        "cpu_work": distribution(frames.iter().map(|frame| frame.cpu_work_ms)),
        "gpu": distribution(frames.iter().map(|frame| frame.gpu_ms)),
        "budget_estimate": distribution(frames.iter().map(|frame| frame.budget_estimate_ms)),
        "profiler_wall": distribution(frames.iter().map(|frame| frame.profiler_wall_ms)),
    })
}

fn distribution(samples: impl Iterator<Item = f64>) -> Option<Distribution> {
    let mut values: Vec<f64> = samples.collect();
    if values.is_empty() {
        return None;
    }
    let over_13_333_ms = values
        .iter()
        .filter(|value| **value > BUDGET_75_HZ_MS)
        .count();
    let over_20_ms = values
        .iter()
        .filter(|value| **value > BUDGET_50_HZ_MS)
        .count();
    values.sort_by(f64::total_cmp);
    let rank = |quantile: f64| {
        ((quantile * values.len() as f64).ceil() as usize)
            .saturating_sub(1)
            .min(values.len() - 1)
    };
    Some(Distribution {
        samples: values.len(),
        p50_ms: values[rank(0.50)],
        p95_ms: values[rank(0.95)],
        p99_ms: values[rank(0.99)],
        max_ms: values[values.len() - 1],
        over_13_333_ms,
        over_20_ms,
    })
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let file =
        File::open(path).map_err(|error| format!("cannot hash {}: {error}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("cannot hash {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

async fn score_content_provenance(
    pool: &sqlx::SqlitePool,
    score_id: &str,
) -> Result<Value, String> {
    let definitions: Vec<String> = sqlx::query_scalar(
        "SELECT definition_json FROM score_definitions WHERE score_id = ? ORDER BY id",
    )
    .bind(score_id)
    .fetch_all(pool)
    .await
    .map_err(|error| error.to_string())?;
    let clips: Vec<String> = sqlx::query_scalar(
        "SELECT json_object(\
         'id', id, 'graph', graph, 'start', start, 'duration', duration, 'seed', seed, \
         'selection_seed', selection_seed, 'selection_json', selection_json, \
         'z_index', z_index, 'blend_mode', blend_mode, 'inputs_json', inputs_json) \
         FROM clips WHERE score_id = ? ORDER BY id",
    )
    .bind(score_id)
    .fetch_all(pool)
    .await
    .map_err(|error| error.to_string())?;
    let mut digest = Sha256::new();
    for value in definitions.iter().chain(&clips) {
        digest.update((value.len() as u64).to_le_bytes());
        digest.update(value.as_bytes());
    }
    Ok(json!({
        "algorithm": "sha256(length-prefixed score definitions followed by SQLite JSON clip rows; each table ordered by id)",
        "definition_count": definitions.len(),
        "clip_count": clips.len(),
        "sha256": format!("{:x}", digest.finalize()),
    }))
}

fn renderer_environment() -> BTreeMap<&'static str, String> {
    const KEYS: &[&str] = &[
        "LUMA_FOG_BLOCKS",
        "LUMA_FOG_VISIBILITY_CACHE",
        "LUMA_FOG_VISIBILITY_ASSERT",
        "LUMA_FOG_DEPTH_CULL",
        "LUMA_FOG_TILE_SIZE",
        "LUMA_GEOMETRY_SHADOWS",
        "LUMA_GEOMETRY_SHADOW_SAMPLES",
        "LUMA_GRID_FOG",
        "LUMA_HAZE_COMPACT",
        "LUMA_HAZE_COMPACT_LATE",
        "LUMA_HAZE_COMPUTE",
        "LUMA_HAZE_DIRECT_ARENA_MB",
        "LUMA_HAZE_FILL",
        "LUMA_HAZE_LIST_REUSE",
        "LUMA_HAZE_COMPACT_RESERVE_HINT",
        "LUMA_HAZE_QUADSHARE",
        "LUMA_HAZE_QUAD_APEX",
        "LUMA_HAZE_QUAD_DIAG",
        "LUMA_HAZE_QUAD_GATE",
        "LUMA_HAZE_QUAD_GATE_FRAC",
        "LUMA_HAZE_QUAD_INTERP",
        "LUMA_HAZE_QUAD_SPAN_MEAN",
        "LUMA_HAZE_QUAD_SPLIT_FETCH",
        "LUMA_HAZE_QUAD_UNIFORM",
        "LUMA_HAZE_RESID_AFTER",
        "LUMA_HAZE_RESID_CAP",
        "LUMA_HAZE_RESID_LANES",
        "LUMA_HAZE_RESID_MAX_AGE_MS",
        "LUMA_HAZE_RESID_STAGE",
        "LUMA_HAZE_RESID_TEMPORAL",
        "LUMA_HAZE_RESID_PER_RESIDENT",
        "LUMA_HAZE_VENUE_DOMAIN",
        "LUMA_HAZE_LIGHTING_DOMAIN",
        "LUMA_HAZE_WHOLE_K_MB",
        "LUMA_HAZE_WORK_COUNTS",
        "LUMA_HAZE_SCENE_LATE",
        "LUMA_INTERVAL_CACHE",
        "LUMA_INTERVAL_RETAIN_INACTIVE",
        "LUMA_INTERVAL_CACHE_ALL_OFF",
        "LUMA_INTERVAL_CACHE_PAYLOAD",
        "LUMA_INTERVAL_CACHE_SKIP_RESIDUAL",
        "LUMA_INTERVAL_CACHE_TABLE_BITS",
        "LUMA_INTERVAL_CACHE_UNIFY",
        "LUMA_LANE_LATE",
        "LUMA_PROFILE_DETAIL",
        "LUMA_PROFILE_OMIT",
        "LUMA_PROFILE_REPEAT",
        "LUMA_SURFACE_DEPTH_CULL",
        "LUMA_SURFACE_DEPTH_SPLIT",
        "LUMA_SURFACE_T_SOURCE",
        "LUMA_VISIBILITY_REFERENCE",
        "LUMA_WIDE_LIGHT_GROUP",
        "LUMA_WITHHOLD_SHARED_SURFACES",
    ];
    KEYS.iter()
        .filter_map(|key| {
            env::var_os(key).map(|value| (*key, value.to_string_lossy().into_owned()))
        })
        .collect()
}

fn repository_root() -> Result<PathBuf, String> {
    let root = command_text(Path::new("."), "git", &["rev-parse", "--show-toplevel"]);
    if root == "unavailable" {
        return Err("cannot resolve repository root".into());
    }
    Ok(root.into())
}

fn command_text(directory: &Path, program: &str, arguments: &[&str]) -> String {
    Command::new(program)
        .current_dir(directory)
        .args(arguments)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unavailable".into())
}

fn command_success(directory: &Path, program: &str, arguments: &[&str]) -> bool {
    Command::new(program)
        .current_dir(directory)
        .args(arguments)
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture_args() -> Arguments {
        Arguments {
            score: "score".into(),
            database: "snapshot.db".into(),
            storage_root: StorageRoot::from_path("storage".into()),
            output: "profile.json".into(),
            start: 0.0,
            end: Some(2.0),
            rate_hz: 10.0,
            width: Some(4),
            height: Some(3),
            warmup_frames: 2,
            camera_suite: "suite.json".into(),
            camera_case: "case".into(),
            capture_dir: Some("captures".into()),
            capture_start: Some(0.3),
            capture_end: Some(0.7),
        }
    }

    #[test]
    fn samples_cover_a_half_open_range_without_accumulated_drift() {
        assert_eq!(sample_times(0.0, 2.1, 2.0), vec![0.0, 0.5, 1.0, 1.5, 2.0]);
        let samples = sample_times(4.8, 5.12, 75.0);
        assert_eq!(samples.len(), 24);
        assert_eq!(samples[0], 4.8);
        assert!(samples.last().copied().unwrap() < 5.12);
    }

    #[test]
    fn capture_window_is_half_open_bounded_and_byte_addressed() {
        let args = capture_args();
        let samples = sample_times(args.start, args.end.unwrap(), args.rate_hz);
        let capture = capture_window(&args, 2.0, &samples, 4, 3).unwrap().unwrap();
        assert_eq!((capture.first_index, capture.end_index), (3, 7));
        assert_eq!(capture.frame_count(), 4);
        assert!(capture.contains(3));
        assert!(capture.contains(6));
        assert!(!capture.contains(2));
        assert!(!capture.contains(7));
        assert_eq!(capture.rgba_bytes_per_frame, 4 * 3 * 4);
        assert!(validate_capture_pixels(Some(&capture), &[0; 4 * 3 * 4]).is_ok());
        assert!(validate_capture_pixels(Some(&capture), &[0; 4 * 3 * 4 - 1]).is_err());
        assert!(validate_capture_pixels(None, &[]).is_ok());
    }

    #[test]
    fn capture_window_requires_a_complete_finite_request_within_the_run() {
        let samples = sample_times(0.0, 2.0, 10.0);
        let mut args = capture_args();
        args.capture_end = None;
        assert!(capture_window(&args, 2.0, &samples, 4, 3).is_err());

        args = capture_args();
        args.capture_start = Some(f64::NAN);
        assert!(capture_window(&args, 2.0, &samples, 4, 3).is_err());

        args = capture_args();
        args.capture_start = Some(-0.1);
        assert!(capture_window(&args, 2.0, &samples, 4, 3).is_err());

        args = capture_args();
        args.capture_end = Some(2.1);
        assert!(capture_window(&args, 2.0, &samples, 4, 3).is_err());

        args = capture_args();
        args.capture_start = Some(0.31);
        args.capture_end = Some(0.39);
        assert!(capture_window(&args, 2.0, &samples, 4, 3).is_err());
    }

    #[test]
    fn capture_window_streams_explicit_ranges_larger_than_sixty_four_frames() {
        let mut args = capture_args();
        args.rate_hz = 75.0;
        args.capture_start = Some(0.0);
        args.capture_end = Some(1.0);
        let samples = sample_times(0.0, 2.0, args.rate_hz);
        assert_eq!(
            capture_window(&args, 2.0, &samples, 4, 3)
                .unwrap()
                .unwrap()
                .frame_count(),
            75
        );

        args.capture_dir = None;
        args.capture_start = None;
        args.capture_end = None;
        assert!(capture_window(&args, 2.0, &samples, 4, 3)
            .unwrap()
            .is_none());
    }

    #[test]
    fn lead_in_and_detected_bars_have_stable_numbers() {
        let downbeats = [0.26, 2.34, 4.4, 6.46];
        assert_eq!(bar_at(0.0, &downbeats), 0);
        assert_eq!(bar_at(0.26, &downbeats), 1);
        assert_eq!(bar_at(2.339, &downbeats), 1);
        assert_eq!(bar_at(2.34, &downbeats), 2);
        let bins = bar_bins(&downbeats, 0.0, 4.4);
        assert_eq!(bins.keys().copied().collect::<Vec<_>>(), vec![0, 1, 2]);
        assert!((bins[&0].end_seconds - 0.26).abs() < 1e-6);
        assert_eq!(bins[&2].end_seconds, 4.4);
    }

    #[test]
    fn distribution_uses_nearest_rank_and_counts_strict_budget_misses() {
        let summary = distribution((1..=100).map(f64::from)).unwrap();
        assert_eq!(summary.p50_ms, 50.0);
        assert_eq!(summary.p95_ms, 95.0);
        assert_eq!(summary.p99_ms, 99.0);
        assert_eq!(summary.max_ms, 100.0);
        assert_eq!(summary.over_20_ms, 80);
        assert_eq!(summary.over_13_333_ms, 87);
    }
}
