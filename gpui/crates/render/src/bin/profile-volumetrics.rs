//! Acceptance profiler for production volumetric rendering.
//!
//! Run from `gpui/` with `cargo run -p luma-render --release --bin
//! profile-volumetrics`. Hardware pass-boundary timestamps measure GPU work;
//! CPU encode time ends at queue submission and excludes waits/readback.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

use glam::Vec3;
use luma_render::assets::Library;
use luma_render::frame::FixtureCone;
use luma_render::scene_desc::{
    CameraPose, DebugView, Environment, Geometry, Piece, RenderSettings, Scene,
};
use luma_render::{build_frame_with, FrameTimings, MetricSummary, Renderer, LIVE_SUBFRAMES};
use serde::Serialize;

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const WARMUP_FRAMES: usize = 120;
const MEASURED_FRAMES: usize = 600;
const PROFILE_ARTIFACT: &str = "gpui/crates/render/goldens/volumetric-profile-m3-max.json";
const PROVENANCE_SCOPES: [&str; 2] = ["gpui/crates/render", "gpui/crates/app"];

/// One profiled configuration.
///
/// A struct rather than a parameter list because the axes are not
/// interchangeable and a reader at the call site should see which is which:
/// `cones` and `camera_radius` scale completely different costs, and a
/// positional `f32` between two `f64` budgets is a bug waiting to be typed.
#[derive(Clone, Copy)]
struct Case {
    id: &'static str,
    cones: usize,
    fixture_shadows: bool,
    /// Distance the camera orbits the rig at, in metres.
    ///
    /// This is the zoom axis. Volumetric cost scales with the *screen area* the
    /// beams cover, not with how many there are, so a close camera is a
    /// fundamentally different measurement from a distant one — and until these
    /// cases existed every number in this file was the cheap, zoomed-out view.
    camera_radius: f32,
    /// Height the camera looks at, in metres.
    ///
    /// The cones throw upward from `z≈0`, so this is what decides whether the
    /// frame is filled with beam or with the empty room beside it. Looking
    /// along the throw is what a zoomed-in operator is actually doing, and it
    /// is worth far more coverage than moving the eye closer is.
    look_height: f32,
    /// Aim every cone at the eye instead of upward.
    ///
    /// The category's named worst case — Capture and Depence both list a
    /// fixture focused into the camera as a top cost, and it is the case a
    /// per-beam proxy rasterizer degenerates to full-screen on. Nothing in
    /// this file measured it until it had a flag.
    aim_at_camera: bool,
    /// How many extra copies of the scene's geometry to draw.
    ///
    /// The synthetic scene has 17 opaque draws — a deck and a truss. A real rig
    /// draws a body per fixture, so zooming in fills the screen with *geometry*
    /// shaded by the clustered surface loop, not just with beam. That is a
    /// different cost from the volumetric march and the one case count alone
    /// cannot produce.
    geometry_copies: usize,
    budgets: Budgets,
}

/// Measured ceilings with headroom, so a change that makes a case slower is
/// caught — not targets. They are loose: the three runs they come from were
/// taken on a machine under heavy build load, where the same case's p95 varied
/// by 2x (`dense-geometry-120`: 48, 53, 93 ms), so they are the worst of the
/// three plus 30%. Re-derive them on an idle machine before treating a pass
/// here as evidence of anything but the absence of a large regression. A 60 Hz frame is 16.7 ms and no case below is within
/// an order of magnitude of it; closing that gap is what
/// `docs/design/volumetrics-v2.md` exists for. Every GPU figure here was
/// re-baselined on 2026-08-25, against numbers near that deadline that were an
/// artefact: the
/// profiler differenced pass *starts*, read back only half its query set, and
/// resolved the set before the GPU had written it, which between them
/// under-reported GPU time by one to three orders of magnitude. See
/// `FrameTimings` for what the samples now mean.
#[derive(Clone, Copy)]
struct Budgets {
    gpu_total_p95: f64,
    gpu_total_max: f64,
    gpu_volumetric_p95: Option<f64>,
    cpu_encode_p95: f64,
    /// Ceiling on mean cluster list length, set just above what the current
    /// builder achieves so drift is caught. High because the grid does not cull
    /// well yet — see `docs/design/volumetrics-v2.md` §3.2.
    mean_lights_per_tile: f64,
}

#[derive(Serialize)]
struct CaseResult {
    case_id: &'static str,
    cones: usize,
    fixture_shadows: bool,
    camera_radius: f32,
    opaque_draws: usize,
    shadowed_fixtures: usize,
    samples: usize,
    samples_fnv64: String,
    gpu_total: MetricSummary,
    gpu_volumetric: MetricSummary,
    cpu_encode_submit: MetricSummary,
    cpu_cluster: MetricSummary,
    cold_cluster_build_ms: f64,
    /// Mean `mask ∩ zbin` candidates the surface pass walked per lit
    /// fragment, over every measured frame — the number that predicts shading
    /// cost (`light-index-unification.md` §8), from the GPU counter pair.
    mean_lights_per_fragment: f64,
    light_index_stats: luma_render::LightIndexStats,
    shadow_stats: luma_render::ShadowStats,
    budgets_ms: serde_json::Value,
    within_budget: bool,
}

#[derive(Serialize)]
struct ProvenanceEntry {
    path: String,
    state: &'static str,
    bytes: Option<usize>,
    content_fnv64: Option<String>,
}

#[derive(Serialize)]
struct SourceProvenance {
    algorithm: &'static str,
    scopes: [&'static str; 2],
    extensions: [&'static str; 4],
    excluded: [&'static str; 2],
    entries: Vec<ProvenanceEntry>,
    manifest_fnv64: String,
}

fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        !cfg!(debug_assertions),
        "acceptance timings require a --release build"
    );
    let arguments: Vec<_> = std::env::args().collect();
    let smoke_clusters = arguments
        .iter()
        .any(|argument| argument == "--smoke-clusters");
    let smoke = smoke_clusters || arguments.iter().any(|argument| argument == "--smoke");
    let case_filter = arguments
        .iter()
        .find_map(|argument| argument.strip_prefix("--case="));
    let wants = |id: &str| case_filter.is_none_or(|filter| filter == id);
    // `--orbit` holds the rig still and moves only the camera: the editor-drag
    // case, and the only one in which the caches can be observed working.
    let motion = if arguments.iter().any(|argument| argument == "--orbit") {
        Motion::Orbit
    } else {
        Motion::Show
    };
    let warmup_frames = if smoke { 2 } else { WARMUP_FRAMES };
    let measured_frames = if smoke { 20 } else { MEASURED_FRAMES };
    // `--capture` writes one frame per case next to the artifact. A timing
    // benchmark that is not looking at what it renders can measure the wrong
    // scene forever and never say so; the zoom cases in particular are only
    // meaningful if the beams really do fill the frame.
    if arguments.iter().any(|argument| argument == "--capture") {
        return capture_cases(&base_frame(false)?, &base_frame(true)?);
    }
    let base = base_frame(false)?;
    let shadow_base = base_frame(true)?;
    let mut renderer = Renderer::new_profiled()?;
    let adapter = renderer.gpu().adapter_profile().clone();
    let mut cases = Vec::new();
    // The zoomed-out radius every case used before the zoom axis existed.
    const WIDE: f32 = 7.4;
    let mut run =
        |renderer: &mut Renderer, base: &luma_render::Frame, case: Case| -> anyhow::Result<()> {
            if wants(case.id) {
                cases.push(profile_case(
                    renderer,
                    base,
                    &case,
                    warmup_frames,
                    measured_frames,
                    motion,
                )?);
            }
            Ok(())
        };

    run(
        &mut renderer,
        &base,
        Case {
            id: "transport-32",
            cones: 32,
            fixture_shadows: false,
            camera_radius: WIDE,
            look_height: 0.8,
            geometry_copies: 0,
            aim_at_camera: false,
            budgets: Budgets {
                gpu_total_p95: 17.0,
                gpu_total_max: 18.0,
                gpu_volumetric_p95: smoke_clusters.then_some(16.0),
                // Was 2.0. This case is the smallest, so its CPU span is the
                // one most exposed once the GPU stopped hiding submit
                // back-pressure (`fixture-shadows-120` below carries the full
                // explanation). Unlike that one, this is *noise* rather than a
                // stable cost: across four identical runs on an idle machine
                // p95 swung 0.98-2.14, p50 varied threefold (0.30-0.93) and
                // max spiked to 12 ms. A p95 that straddles its own budget is
                // measuring the scheduler. Widened to document what is known
                // rather than to invent precision it does not have.
                cpu_encode_p95: 3.0,
                mean_lights_per_tile: 30.0,
            },
        },
    )?;
    if !smoke || smoke_clusters {
        run(
            &mut renderer,
            &base,
            Case {
                id: "transport-128",
                cones: 128,
                fixture_shadows: false,
                camera_radius: WIDE,
                look_height: 0.8,
                geometry_copies: 0,
                aim_at_camera: false,
                budgets: Budgets {
                    gpu_total_p95: 75.0,
                    gpu_total_max: 89.0,
                    gpu_volumetric_p95: Some(71.0),
                    cpu_encode_p95: 2.5,
                    mean_lights_per_tile: 100.0,
                },
            },
        )?;
        run(
            &mut renderer,
            &base,
            Case {
                id: "transport-512",
                cones: 512,
                fixture_shadows: false,
                camera_radius: WIDE,
                look_height: 0.8,
                geometry_copies: 0,
                aim_at_camera: false,
                budgets: Budgets {
                    gpu_total_p95: 324.0,
                    gpu_total_max: 398.0,
                    // The pathological all-overlap stress case, more than 4x
                    // the 120-fixture production target. `transport-128` above
                    // is the near-production budget.
                    gpu_volumetric_p95: Some(305.0),
                    cpu_encode_p95: 2.0,
                    // Broad-phase candidates per 8 px tile over the whole
                    // frame (the light index's stat; the CSR-era number was
                    // per *occupied* cluster and is not comparable). Measures
                    // 346 on the all-overlap stress; the narrow phase is the
                    // lever that should pull this down.
                    mean_lights_per_tile: 360.0,
                },
            },
        )?;
        // The zoom axis. Same rig, same lights, camera walked in until the
        // beams cover the frame — which is the configuration the product is
        // actually used in and the one nothing here measured before.
        run(
            &mut renderer,
            &base,
            Case {
                id: "zoom-near-128",
                cones: 128,
                fixture_shadows: false,
                camera_radius: 2.2,
                look_height: 4.0,
                geometry_copies: 0,
                aim_at_camera: false,
                budgets: Budgets {
                    gpu_total_p95: 175.0,
                    gpu_total_max: 192.0,
                    gpu_volumetric_p95: Some(173.0),
                    cpu_encode_p95: 1.5,
                    mean_lights_per_tile: 130.0,
                },
            },
        )?;
        run(
            &mut renderer,
            &base,
            Case {
                id: "beams-at-camera-128",
                cones: 128,
                fixture_shadows: false,
                camera_radius: WIDE,
                look_height: 0.8,
                geometry_copies: 0,
                // The category's named worst case, measured on purpose: every
                // cone contains the eye, so every culler's bound degenerates
                // toward full screen at once — and it is the case a per-beam
                // proxy rasterizer would pay full-screen fill per light on.
                //
                // Budgets are calibrated to what the current marcher does with
                // it on an M3 Max (2026-08-25: gpu total p95 230 ms, of which
                // 222 ms is the volumetric march; mean lights per tile 116;
                // cluster grid rebuilt every frame), NOT to a target. The
                // doc's Phase 2 work is what must bring it down; this case
                // exists to catch the day a beam-path change makes the worst
                // case worse, and to hold the before/after number when one
                // makes it better.
                aim_at_camera: true,
                budgets: Budgets {
                    gpu_total_p95: 348.0,
                    gpu_total_max: 442.0,
                    gpu_volumetric_p95: Some(339.0),
                    cpu_encode_p95: 7.0,
                    mean_lights_per_tile: 130.0,
                },
            },
        )?;
        run(
            &mut renderer,
            &base,
            Case {
                id: "zoom-inside-128",
                cones: 128,
                fixture_shadows: false,
                camera_radius: 0.9,
                look_height: 6.5,
                geometry_copies: 0,
                aim_at_camera: false,
                budgets: Budgets {
                    gpu_total_p95: 161.0,
                    gpu_total_max: 196.0,
                    gpu_volumetric_p95: Some(159.0),
                    cpu_encode_p95: 1.5,
                    mean_lights_per_tile: 130.0,
                },
            },
        )?;
    }
    run(
        &mut renderer,
        &shadow_base,
        Case {
            id: "fixture-shadows-120",
            cones: 120,
            fixture_shadows: true,
            camera_radius: WIDE,
            look_height: 0.8,
            geometry_copies: 0,
            aim_at_camera: false,
            budgets: Budgets {
                gpu_total_p95: 73.0,
                gpu_total_max: 81.0,
                gpu_volumetric_p95: Some(68.0),
                // Was 3.0, and the 3.75 that broke it is a *revealed*
                // pre-existing cost, not a regression. `cpu_encode_submit`
                // spans frame entry through `queue.submit`, so it absorbs
                // back-pressure: while the volumetric pass took 34 ms this
                // scene's 366 draws encoded inside the GPU's shadow and the
                // span read 0.62 ms. Baking the density field
                // (`docs/design/haze-noise-field.md` §7 step 4) took the pass
                // to 9.5 ms, the CPU became the bottleneck, and the cost
                // surfaced. The control that proves it is not the texture:
                // sampling the field for real while keeping the old ALU noise,
                // so the GPU stays slow, measures 0.58 ms. Its 366-draw
                // siblings sit at 12-17 here, so 3.0 was the outlier.
                // CPU encode is the next perf task; this budget is a
                // regression guard, not a target.
                cpu_encode_p95: 8.0,
                mean_lights_per_tile: 100.0,
            },
        },
    )?;
    // A real rig draws a body per fixture, so a zoomed-in view is full of
    // *geometry* shaded by the clustered surface loop. 120 copies of the deck
    // and truss stands in for that; the synthetic scene's 17 draws cannot.
    run(
        &mut renderer,
        &shadow_base,
        Case {
            id: "dense-geometry-noshadow-120",
            cones: 120,
            fixture_shadows: false,
            camera_radius: WIDE,
            look_height: 0.8,
            geometry_copies: 120,
            aim_at_camera: false,
            budgets: Budgets {
                gpu_total_p95: 103.0,
                gpu_total_max: 120.0,
                gpu_volumetric_p95: Some(95.0),
                cpu_encode_p95: 12.0,
                mean_lights_per_tile: 100.0,
            },
        },
    )?;
    run(
        &mut renderer,
        &shadow_base,
        Case {
            id: "dense-geometry-120",
            cones: 120,
            fixture_shadows: true,
            camera_radius: WIDE,
            look_height: 0.8,
            geometry_copies: 120,
            aim_at_camera: false,
            budgets: Budgets {
                gpu_total_p95: 121.0,
                gpu_total_max: 141.0,
                gpu_volumetric_p95: Some(108.0),
                cpu_encode_p95: 14.0,
                mean_lights_per_tile: 100.0,
            },
        },
    )?;
    run(
        &mut renderer,
        &shadow_base,
        Case {
            id: "zoom-dense-geometry-120",
            cones: 120,
            fixture_shadows: true,
            camera_radius: 2.2,
            look_height: 1.5,
            geometry_copies: 120,
            aim_at_camera: false,
            budgets: Budgets {
                gpu_total_p95: 78.0,
                gpu_total_max: 92.0,
                gpu_volumetric_p95: Some(57.0),
                cpu_encode_p95: 17.0,
                mean_lights_per_tile: 130.0,
            },
        },
    )?;
    run(
        &mut renderer,
        &shadow_base,
        Case {
            id: "zoom-inside-shadows-120",
            cones: 120,
            fixture_shadows: true,
            camera_radius: 0.9,
            look_height: 6.5,
            geometry_copies: 0,
            aim_at_camera: false,
            budgets: Budgets {
                gpu_total_p95: 163.0,
                gpu_total_max: 197.0,
                gpu_volumetric_p95: Some(160.0),
                cpu_encode_p95: 3.0,
                mean_lights_per_tile: 130.0,
            },
        },
    )?;
    drop(run);
    anyhow::ensure!(!cases.is_empty(), "unknown profile case filter");
    let all_pass = cases.iter().all(|case| case.within_budget);
    let repository = git_repository_root()?;
    let status = checked_command_text(Command::new("git").current_dir(&repository).args([
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
    ]))?;
    let provenance = source_provenance(&repository)?;
    let artifact = serde_json::json!({
        "schema": "luma.renderer-profile/2",
        "captured_at_utc": command_output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]),
        "source": {
            "commit": checked_git_text(&repository, &["rev-parse", "HEAD"] )?,
            "branch": checked_git_text(&repository, &["branch", "--show-current"] )?,
            "dirty": !status.is_empty(),
            "provenance": provenance,
        },
        "build": {
            "profile": "release",
            "debug_assertions": cfg!(debug_assertions),
            "target": rustc_host()?,
            "arch": std::env::consts::ARCH,
            "rustc": checked_command_text(Command::new("rustc").arg("--version"))?,
            "crate_version": env!("CARGO_PKG_VERSION"),
            // Read from the lockfile, not repeated here: a literal drifted
            // silently through one wgpu major already.
            "wgpu_lock": wgpu_lock_version(&repository)?,
        },
        "host": {
            "os": std::env::consts::OS,
            "os_version": os_version(),
            "arch": std::env::consts::ARCH,
        },
        "adapter": {
            "name": adapter.name,
            "backend": adapter.backend,
            "device_type": adapter.device_type,
            "driver": adapter.driver,
            "driver_info": adapter.driver_info,
            "timestamp_query_supported": adapter.timestamp_query_supported,
            "timestamp_period_ns": adapter.timestamp_period_ns,
        },
        "display": {
            "scale_factor": 1.0,
            "interpretation": "offscreen physical pixels",
        },
        "repro": {
            "command": if smoke_clusters {
                "cargo run -p luma-render --release --bin profile-volumetrics -- --smoke-clusters"
            } else if smoke {
                "cargo run -p luma-render --release --bin profile-volumetrics -- --smoke"
            } else {
                "cargo run -p luma-render --release --bin profile-volumetrics"
            },
            "cwd": "gpui",
            "width": WIDTH,
            "height": HEIGHT,
            "haze_resolution": 0.5,
            "haze_steps": 8,
            "subframes": LIVE_SUBFRAMES,
            "temporal": true,
            "debug_view": "pbr",
            "warmup_frames": warmup_frames,
            "measured_frames": measured_frames,
            "motion": if motion == Motion::Orbit { "orbit (camera only)" } else { "show (camera + heads)" },
            "time_step_seconds": 1.0 / 60.0,
            "percentile": "nearest-rank ceil(q*n)-1",
        },
        "cases": cases,
        "all_within_budget": all_pass,
    });
    println!("{}", serde_json::to_string_pretty(&artifact)?);
    anyhow::ensure!(all_pass, "one or more renderer acceptance budgets failed");
    Ok(())
}

fn profile_case(
    renderer: &mut Renderer,
    base: &luma_render::Frame,
    case: &Case,
    warmup_frames: usize,
    measured_frames: usize,
    motion: Motion,
) -> anyhow::Result<CaseResult> {
    let Case {
        id: case_id,
        cones,
        fixture_shadows,
        camera_radius,
        look_height,
        geometry_copies,
        aim_at_camera,
        budgets,
    } = *case;
    let mut frame = frame_with_lights(base, cones);
    multiply_geometry(&mut frame, geometry_copies);
    frame.fixture_shadows = fixture_shadows;
    let opaque_draws = frame.draws.len() - frame.grid_draws;
    let shadowed_fixtures = usize::from(fixture_shadows) * cones.min(128);
    if fixture_shadows {
        anyhow::ensure!(
            opaque_draws > 1 && shadowed_fixtures == 120,
            "fixture-shadow profile must exercise representative geometry and all 120 lights"
        );
    }
    let mut cold_cluster_build_ms = 0.0;
    for sample in 0..warmup_frames {
        animate(
            &mut frame,
            sample as f32 / 60.0,
            motion,
            camera_radius,
            look_height,
            aim_at_camera,
        );
        let timing = renderer.profile_live_frame(&frame, WIDTH, HEIGHT, LIVE_SUBFRAMES)?;
        if sample == 0 {
            cold_cluster_build_ms = timing.cpu_cluster_ms;
        }
    }
    let mut samples = Vec::with_capacity(measured_frames);
    let (mut fragments_total, mut candidates_total) = (0_u64, 0_u64);
    for sample in 0..measured_frames {
        animate(
            &mut frame,
            (warmup_frames + sample) as f32 / 60.0,
            motion,
            camera_radius,
            look_height,
            aim_at_camera,
        );
        samples.push(renderer.profile_live_frame(&frame, WIDTH, HEIGHT, LIVE_SUBFRAMES)?);
        // The counter readback is a blocking GPU round trip; taken every
        // frame it serialises the pipeline and roughly quadruples the run.
        // One frame in sixteen still averages ~40 samples per case.
        if sample % 16 == 0 {
            if let Some((frame_count, candidates)) = renderer.fragment_stats()? {
                fragments_total += frame_count;
                candidates_total += candidates;
            }
        }
    }
    let total = summarize(samples.iter().map(|sample| sample.gpu_total_ms));
    let volumetric = summarize(samples.iter().map(|sample| sample.gpu_volumetric_ms));
    let cpu = summarize(samples.iter().map(|sample| sample.cpu_encode_submit_ms));
    let cluster = summarize(samples.iter().map(|sample| sample.cpu_cluster_ms));
    // p95 alone let a 140 ms frame pass as "within budget": with 600 samples it
    // hides the worst 30. A hitch is exactly what a show notices, so the worst
    // frame is part of the contract.
    // Culling quality is part of the contract, not just timing: if the mean
    // list length drifts back towards the cone count the grid has stopped
    // working, and that shows up here before it shows up as milliseconds.
    let light_index_stats = renderer.light_index_stats();
    let within_budget = total.p95_ms <= budgets.gpu_total_p95
        && total.max_ms <= budgets.gpu_total_max
        && cpu.p95_ms <= budgets.cpu_encode_p95
        && light_index_stats.mean_lights_per_tile <= budgets.mean_lights_per_tile
        && budgets
            .gpu_volumetric_p95
            .is_none_or(|budget| volumetric.p95_ms <= budget);
    let mut sample_bytes = Vec::with_capacity(samples.len() * 24);
    // Destructured exhaustively on purpose: this digest is a golden, so a new
    // timing field has to be considered here rather than silently omitted. The
    // scene and composite spans are deliberately NOT hashed — they are
    // subdivisions of `gpu_total_ms`, already covered by it, and adding them
    // would invalidate every stored profile for no new information.
    for FrameTimings {
        gpu_total_ms,
        gpu_volumetric_ms,
        gpu_scene_ms: _,
        gpu_composite_ms: _,
        // A subdivision like scene/composite, and it runs outside
        // `gpu_total_ms` — covered by the wall-clocked encode span instead.
        gpu_index_ms: _,
        cpu_encode_submit_ms,
        cpu_cluster_ms,
    } in &samples
    {
        sample_bytes.extend_from_slice(&gpu_total_ms.to_bits().to_le_bytes());
        sample_bytes.extend_from_slice(&gpu_volumetric_ms.to_bits().to_le_bytes());
        sample_bytes.extend_from_slice(&cpu_encode_submit_ms.to_bits().to_le_bytes());
        sample_bytes.extend_from_slice(&cpu_cluster_ms.to_bits().to_le_bytes());
    }
    Ok(CaseResult {
        case_id,
        cones,
        fixture_shadows,
        camera_radius,
        opaque_draws,
        shadowed_fixtures,
        samples: samples.len(),
        samples_fnv64: format!("0x{:016x}", fnv64(&sample_bytes)),
        gpu_total: total,
        gpu_volumetric: volumetric,
        cpu_encode_submit: cpu,
        cpu_cluster: cluster,
        cold_cluster_build_ms,
        mean_lights_per_fragment: if fragments_total > 0 {
            candidates_total as f64 / fragments_total as f64
        } else {
            0.0
        },
        light_index_stats,
        shadow_stats: renderer.shadow_stats(),
        budgets_ms: serde_json::json!({
            "gpu_total_p95": budgets.gpu_total_p95,
            "gpu_total_max": budgets.gpu_total_max,
            "gpu_volumetric_p95": budgets.gpu_volumetric_p95,
            "cpu_encode_submit_p95": budgets.cpu_encode_p95,
            "mean_lights_per_tile": budgets.mean_lights_per_tile,
        }),
        within_budget,
    })
}

/// What is moving while the profiler runs.
///
/// The two motions invalidate different caches, and a benchmark that always
/// moves both can only ever measure the total-miss frame — it cannot show
/// whether the caches work. `Show` is the live-show worst case; `Orbit` is the
/// editor dragging the view around a rig that is holding still, which is the
/// case the cluster and shadow caches exist for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Motion {
    Show,
    Orbit,
}

fn animate(
    frame: &mut luma_render::Frame,
    time: f32,
    motion: Motion,
    radius: f32,
    look_height: f32,
    aim_at_camera: bool,
) {
    frame.time = time;
    let orbit = time * 0.35;
    frame.camera.eye = Vec3::new(
        radius * orbit.cos(),
        radius * orbit.sin(),
        (radius * 0.4).max(0.6) + 0.4 * (time * 0.5).sin(),
    );
    frame.camera.target = Vec3::new(0.0, 0.0, look_height);
    // Straight at the lens, deliberately: the small wobble the show case adds
    // would take most beams *off* the eye, and the whole point of this axis is
    // that every cone contains it. Applied under `Orbit` too — the camera is
    // what moved, so beams tracking it are re-aimed by construction.
    if aim_at_camera {
        let eye = frame.camera.eye;
        for cone in frame.fixture_cones.iter_mut() {
            cone.direction = (eye - cone.position).normalize();
        }
        return;
    }
    if motion == Motion::Orbit {
        return;
    }
    for (index, cone) in frame.fixture_cones.iter_mut().enumerate() {
        let phase = time * 1.1 + index as f32 * 0.37;
        cone.direction = Vec3::new(0.35 * phase.sin(), 0.35 * phase.cos(), 1.0).normalize();
    }
}

/// Render one settled frame per case to `target/profile-capture/`.
fn capture_cases(
    base: &luma_render::Frame,
    shadow_base: &luma_render::Frame,
) -> anyhow::Result<()> {
    let mut viewport = luma_render::Viewport::new()?;
    let out = std::path::Path::new("target/profile-capture");
    fs::create_dir_all(out)?;
    for (id, source, cones, shadows, radius, look, aim) in [
        ("transport-128", base, 128, false, 7.4_f32, 0.8_f32, false),
        ("zoom-near-128", base, 128, false, 2.2, 4.0, false),
        ("zoom-inside-128", base, 128, false, 0.9, 6.5, false),
        (
            "zoom-inside-shadows-120",
            shadow_base,
            120,
            true,
            0.9,
            6.5,
            false,
        ),
        ("beams-at-camera-128", base, 128, false, 7.4, 0.8, true),
    ] {
        let mut frame = frame_with_lights(source, cones);
        frame.fixture_shadows = shadows;
        // Warm the temporal history so the capture is a settled frame, not the
        // first one.
        for sample in 0..8 {
            animate(
                &mut frame,
                sample as f32 / 60.0,
                Motion::Show,
                radius,
                look,
                aim,
            );
            viewport.draw(&frame, WIDTH, HEIGHT)?;
        }
        let presentation = viewport.draw(&frame, WIDTH, HEIGHT)?;
        let lit = presentation
            .pixels
            .chunks_exact(4)
            .filter(|texel| texel[0] > 8 || texel[1] > 8 || texel[2] > 8)
            .count();
        let total = (WIDTH * HEIGHT) as usize;
        println!(
            "{id:<24} radius={radius:<4} lit pixels {:.1}%",
            lit as f64 / total as f64 * 100.0
        );
        image::RgbaImage::from_raw(WIDTH, HEIGHT, presentation.pixels.to_vec())
            .ok_or_else(|| anyhow::anyhow!("readback was not width * height * 4"))?
            .save(out.join(format!("{id}.png")))?;
    }
    println!("wrote {}", out.display());
    Ok(())
}

fn summarize(samples: impl Iterator<Item = f64>) -> MetricSummary {
    MetricSummary::of(samples).expect("the profiler always records samples")
}

fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn git_repository_root() -> anyhow::Result<PathBuf> {
    let root = checked_command_text(Command::new("git").args(["rev-parse", "--show-toplevel"]))?;
    let root = PathBuf::from(root);
    anyhow::ensure!(
        root.is_absolute(),
        "Git returned a non-absolute repository root"
    );
    Ok(root)
}

fn checked_git_text(repository: &Path, args: &[&str]) -> anyhow::Result<String> {
    checked_command_text(Command::new("git").current_dir(repository).args(args))
}

fn checked_git_bytes(repository: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    checked_command(Command::new("git").current_dir(repository).args(args))
        .map(|output| output.stdout)
}

fn checked_command(command: &mut Command) -> anyhow::Result<Output> {
    let debug = format!("{command:?}");
    let output = command
        .output()
        .map_err(|error| anyhow::anyhow!("failed to execute {debug}: {error}"))?;
    anyhow::ensure!(
        output.status.success(),
        "{debug} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output)
}

fn checked_command_text(command: &mut Command) -> anyhow::Result<String> {
    let output = checked_command(command)?;
    let text = String::from_utf8(output.stdout)
        .map_err(|error| anyhow::anyhow!("command output was not UTF-8: {error}"))?;
    Ok(text.trim().to_owned())
}

/// The `wgpu` version the workspace lockfile resolves for this crate.
///
/// Newest when several majors coexist, because this crate always tracks the
/// newest one it names in its manifest; the older entries belong to vendored
/// dependencies.
fn wgpu_lock_version(repository: &Path) -> anyhow::Result<String> {
    let lock = fs::read_to_string(repository.join("gpui/Cargo.lock"))?;
    lock.split("[[package]]")
        .filter(|entry| entry.contains("name = \"wgpu\"\n"))
        .filter_map(|entry| {
            entry
                .lines()
                .find_map(|line| line.strip_prefix("version = \""))
                .map(|rest| rest.trim_end_matches('"').to_string())
        })
        .max()
        .ok_or_else(|| anyhow::anyhow!("no wgpu entry in gpui/Cargo.lock"))
}

fn rustc_host() -> anyhow::Result<String> {
    let verbose = checked_command_text(Command::new("rustc").arg("-vV"))?;
    verbose
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("rustc -vV did not report a host target"))
}

fn source_provenance(repository: &Path) -> anyhow::Result<SourceProvenance> {
    let mut tracked_args = vec![
        "diff",
        "--name-only",
        "--no-renames",
        "--diff-filter=ACMRTUXBD",
        "-z",
        "HEAD",
        "--",
    ];
    tracked_args.extend(PROVENANCE_SCOPES);
    let tracked = nul_paths(&checked_git_bytes(repository, &tracked_args)?)?;

    let mut untracked_args = vec!["ls-files", "--others", "--exclude-standard", "-z", "--"];
    untracked_args.extend(PROVENANCE_SCOPES);
    let untracked = nul_paths(&checked_git_bytes(repository, &untracked_args)?)?;

    let tracked: BTreeSet<_> = tracked
        .into_iter()
        .filter(|path| relevant_path(path))
        .collect();
    let untracked: BTreeSet<_> = untracked
        .into_iter()
        .filter(|path| relevant_path(path))
        .collect();
    anyhow::ensure!(
        tracked.is_disjoint(&untracked),
        "Git reported a path as both tracked-dirty and untracked"
    );

    let mut entries = Vec::with_capacity(tracked.len() + untracked.len());
    for (path, state) in tracked
        .iter()
        .map(|path| (path, "tracked"))
        .chain(untracked.iter().map(|path| (path, "untracked")))
    {
        let absolute = repository.join(path);
        match fs::read(&absolute) {
            Ok(content) => entries.push(ProvenanceEntry {
                path: path.clone(),
                state,
                bytes: Some(content.len()),
                content_fnv64: Some(format!("0x{:016x}", fnv64(&content))),
            }),
            Err(error) if state == "tracked" && error.kind() == std::io::ErrorKind::NotFound => {
                entries.push(ProvenanceEntry {
                    path: path.clone(),
                    state: "deleted",
                    bytes: None,
                    content_fnv64: None,
                });
            }
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "cannot fingerprint relevant {state} path {path}: {error}"
                ));
            }
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let manifest_fnv64 = manifest_hash(&entries);
    Ok(SourceProvenance {
        algorithm: "fnv1a64(length(path),path,length(state),state,content_length,length(content_fnv64),content_fnv64)",
        scopes: PROVENANCE_SCOPES,
        extensions: ["rs", "wgsl", "json", "png"],
        excluded: [PROFILE_ARTIFACT, ".husky/**"],
        entries,
        manifest_fnv64: format!("0x{manifest_fnv64:016x}"),
    })
}

fn nul_paths(bytes: &[u8]) -> anyhow::Result<Vec<String>> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = std::str::from_utf8(path)
                .map_err(|error| anyhow::anyhow!("Git path was not UTF-8: {error}"))?;
            let safe = Path::new(path)
                .components()
                .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
            anyhow::ensure!(safe, "Git returned unsafe provenance path {path}");
            Ok(path.to_owned())
        })
        .collect()
}

fn relevant_path(path: &str) -> bool {
    path != PROFILE_ARTIFACT
        && !path.split('/').any(|component| component == ".husky")
        && matches!(
            Path::new(path)
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("rs" | "wgsl" | "json" | "png")
        )
}

fn manifest_hash(entries: &[ProvenanceEntry]) -> u64 {
    let mut bytes = Vec::new();
    for entry in entries {
        append_field(&mut bytes, entry.path.as_bytes());
        append_field(&mut bytes, entry.state.as_bytes());
        match (entry.bytes, entry.content_fnv64.as_deref()) {
            (Some(length), Some(hash)) => {
                bytes.extend_from_slice(&(length as u64).to_le_bytes());
                append_field(&mut bytes, hash.as_bytes());
            }
            (None, None) => bytes.extend_from_slice(&u64::MAX.to_le_bytes()),
            _ => unreachable!("manifest entry content fields move together"),
        }
    }
    fnv64(&bytes)
}

fn append_field(output: &mut Vec<u8>, field: &[u8]) {
    output.extend_from_slice(&(field.len() as u64).to_le_bytes());
    output.extend_from_slice(field);
}

#[cfg(test)]
mod provenance_tests {
    use super::{manifest_hash, relevant_path, ProvenanceEntry, PROFILE_ARTIFACT};

    fn entry(path: &str, hash: &str) -> ProvenanceEntry {
        ProvenanceEntry {
            path: path.into(),
            state: "untracked",
            bytes: Some(4),
            content_fnv64: Some(hash.into()),
        }
    }

    #[test]
    fn provenance_scope_accepts_evidence_and_excludes_its_output() {
        assert!(relevant_path("gpui/crates/render/src/gpu.rs"));
        assert!(relevant_path(
            "gpui/crates/render/goldens/volumetric-stress-32.png"
        ));
        assert!(!relevant_path(PROFILE_ARTIFACT));
        assert!(!relevant_path(".husky/_/husky.sh"));
        assert!(!relevant_path("gpui/crates/render/target/cache.bin"));
    }

    #[test]
    fn manifest_hash_binds_path_state_length_and_content_hash() {
        let original = [entry("gpui/crates/render/src/a.rs", "0x0000000000000001")];
        let changed_content = [entry("gpui/crates/render/src/a.rs", "0x0000000000000002")];
        let changed_path = [entry("gpui/crates/render/src/b.rs", "0x0000000000000001")];
        let mut changed_state = entry("gpui/crates/render/src/a.rs", "0x0000000000000001");
        changed_state.state = "tracked";
        let changed_state = [changed_state];
        assert_ne!(manifest_hash(&original), manifest_hash(&changed_content));
        assert_ne!(manifest_hash(&original), manifest_hash(&changed_path));
        assert_ne!(manifest_hash(&original), manifest_hash(&changed_state));
    }
}

fn command_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unavailable".into())
}

fn os_version() -> String {
    if cfg!(target_os = "macos") {
        command_output("sw_vers", &["-productVersion"])
    } else {
        command_output("uname", &["-sr"])
    }
}

fn base_frame(with_geometry: bool) -> anyhow::Result<luma_render::Frame> {
    let mut settings = RenderSettings::dark_stage(48.0, 0.5);
    settings.environment = Environment::DARK;
    settings.haze.enabled = true;
    settings.haze.steps = 8;
    settings.haze.density = 0.65;
    settings.debug_view = DebugView::Pbr;
    let scene = Scene {
        id: "volumetric-profile".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [4.5, 3.0, 5.0],
            target: [0.0, 0.8, 0.0],
        },
        editing: false,
        render: settings,
        selected_fixture_ids: Vec::new(),
        fixtures: Vec::new(),
        pieces: if with_geometry {
            vec![
                Piece {
                    id: "deck-l".into(),
                    geometry: Geometry::mesh("stage_lab/stage_praticavel_2x1x1.glb"),
                    kind: "floor".into(),
                    pos: [-1.0, 0.0, 0.0],
                    rot: [0.0; 3],
                    scale: 1.0,
                },
                Piece {
                    id: "deck-r".into(),
                    geometry: Geometry::mesh("stage_lab/stage_praticavel_2x1x1.glb"),
                    kind: "floor".into(),
                    pos: [1.0, 0.0, 0.0],
                    rot: [0.0; 3],
                    scale: 1.0,
                },
                Piece {
                    id: "truss".into(),
                    geometry: Geometry::mesh("stage_lab/truss_q40_1.83m.glb"),
                    kind: "truss".into(),
                    pos: [0.0, -1.6, 0.0],
                    rot: [0.0; 3],
                    scale: 1.0,
                },
                Piece {
                    id: "speaker".into(),
                    geometry: Geometry::mesh("stage_lab/speaker_dbr15.glb"),
                    kind: "speaker".into(),
                    pos: [2.2, 0.0, 0.0],
                    rot: [0.0; 3],
                    scale: 1.0,
                },
            ]
        } else {
            Vec::new()
        },
        state: BTreeMap::new(),
    };
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let mut library = Library::new(meshes);
    build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library)
}

/// Draw the scene's opaque geometry `copies` times over, spread through the
/// rig so the copies occlude one another rather than z-fighting in place.
fn multiply_geometry(frame: &mut luma_render::Frame, copies: usize) {
    if copies == 0 {
        return;
    }
    let opaque = frame.draws.len() - frame.grid_draws;
    let copy_of = |draw: &luma_render::frame::Draw| luma_render::frame::Draw {
        mesh: draw.mesh,
        model: draw.model,
        material: draw.material,
        textures: draw.textures,
        editor_object: draw.editor_object.clone(),
    };
    let originals: Vec<_> = frame.draws[..opaque].iter().map(copy_of).collect();
    let grid: Vec<_> = frame.draws[opaque..].iter().map(copy_of).collect();
    frame.draws.truncate(opaque);
    for copy in 1..=copies {
        let angle = copy as f32 * 2.399_963;
        let radius = 0.35 * (copy as f32).sqrt();
        let offset = glam::Mat4::from_translation(Vec3::new(
            radius * angle.cos(),
            radius * angle.sin(),
            (copy % 5) as f32 * 0.22,
        ));
        frame
            .draws
            .extend(originals.iter().map(|draw| luma_render::frame::Draw {
                model: offset * draw.model,
                ..copy_of(draw)
            }));
    }
    frame.draws.extend(grid);
}

fn frame_with_lights(base: &luma_render::Frame, count: usize) -> luma_render::Frame {
    let mut frame = luma_render::Frame {
        meshes: base
            .meshes
            .iter()
            .map(|mesh| luma_render::frame::MeshData {
                key: mesh.key.clone(),
                vertices: mesh.vertices.clone(),
                indices: mesh.indices.clone(),
            })
            .collect(),
        images: base.images.clone(),
        draws: base
            .draws
            .iter()
            .map(|draw| luma_render::frame::Draw {
                mesh: draw.mesh,
                model: draw.model,
                material: draw.material,
                textures: draw.textures,
                editor_object: draw.editor_object.clone(),
            })
            .collect(),
        grid_draws: base.grid_draws,
        overlays: Vec::new(),
        point_lights: base.point_lights.clone(),
        fixture_cones: Vec::with_capacity(count),
        fixture_surface_lighting: true,
        beam_proxy: false,
        fixture_shadows: true,
        cluster_debug: false,
        clear_color: base.clear_color,
        ambient: base.ambient,
        environment: base.environment.clone(),
        directional: base.directional,
        haze_density: base.haze_density,
        haze_steps: base.haze_steps,
        haze_resolution: base.haze_resolution,
        time: 0.0,
        debug_view: base.debug_view,
        camera: base.camera,
    };
    for index in 0..count {
        let column = (index % 32) as f32;
        let row = (index / 32) as f32;
        frame.fixture_cones.push(FixtureCone {
            position: Vec3::new((column - 15.5) * 0.18, (row - 7.5) * 0.18, 0.15),
            range: 8.0,
            direction: Vec3::Z,
            cos_beam: 0.975,
            color: Vec3::new(0.2, 0.55, 1.0),
            intensity: 0.01,
            cos_field: 0.93,
            wash: 0.0,
            gobo: (index % 3) as u32,
            gobo_rotation: 0.31,
        });
    }
    frame
}
