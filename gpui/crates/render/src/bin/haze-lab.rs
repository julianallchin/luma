//! Repeatable image-quality experiments on pinned scenes and saved-score sequences.
//! See experiments/haze-lab.md. This tool never reads or writes the library DB.
use anyhow::{bail, Context, Result};
use glam::Vec3;
use luma_render::{assets::Library, build_frame, coords::three_from_world, Catalogue, Renderer};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[path = "haze-lab/reuse.rs"]
mod reuse;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Serialize, Deserialize)]
struct Case {
    id: String,
    scene: String,
    time: f32,
    eye: [f32; 3],
    target: [f32; 3],
    fov: f32,
    purpose: String,
}
#[derive(Serialize, Deserialize)]
struct Suite {
    catalogue: PathBuf,
    width: u32,
    height: u32,
    cases: Vec<Case>,
}
fn library() -> Library {
    Library::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes"))
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
fn png(path: &Path, bytes: &[u8], width: u32, height: u32) -> Result<()> {
    let mut encoder = png::Encoder::new(fs::File::create(path)?, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(bytes)?;
    Ok(())
}
struct ReplayInputs {
    suite: Suite,
    catalogue_path: PathBuf,
    frames: Vec<(luma_render::Frame, f64)>,
}

impl ReplayInputs {
    fn load(suite_path: &Path) -> Result<Self> {
        let suite: Suite = serde_json::from_slice(&fs::read(suite_path)?)?;
        anyhow::ensure!(
            suite.width > 0 && suite.height > 0 && suite.width <= 4096 && suite.height <= 4096,
            "invalid replay dimensions"
        );
        anyhow::ensure!(
            (2..=64).contains(&suite.cases.len()),
            "replay requires 2..64 frames"
        );
        anyhow::ensure!(
            suite
                .cases
                .iter()
                .all(|case| case.time.is_finite() && case.time >= 0.0)
                && suite.cases.windows(2).all(|pair| {
                    let delta = pair[1].time - pair[0].time;
                    delta > 0.0 && delta <= 0.25
                }),
            "replay times must increase by at most 0.25 seconds per frame"
        );
        let catalogue_path = suite_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(&suite.catalogue);
        let mut catalogue = Catalogue::load(&catalogue_path)?;
        let mut library = library();
        let mut frames = Vec::new();
        for case in &suite.cases {
            let scene = catalogue
                .scenes
                .iter_mut()
                .find(|scene| scene.id == case.scene)
                .with_context(|| format!("missing scene {}", case.scene))?;
            anyhow::ensure!(
                scene.times == [case.time],
                "score snapshot does not match replay time"
            );
            scene.camera.position = three_from_world(Vec3::from_array(case.eye)).to_array();
            scene.camera.target = three_from_world(Vec3::from_array(case.target)).to_array();
            scene.render.fov = case.fov;
            let started = Instant::now();
            let mut frame = build_frame(scene, &catalogue.definitions, case.time, &mut library)?;
            frame.haze_resolution = luma_render::LIVE_HAZE_RESOLUTION;
            frames.push((frame, started.elapsed().as_secs_f64() * 1000.0));
        }
        Ok(Self {
            suite,
            catalogue_path,
            frames,
        })
    }

    /// Archive exact inputs in a new directory; never overwrite a previous run.
    fn archive(&self, output: &Path) -> Result<()> {
        fs::create_dir_all(output.parent().unwrap_or(Path::new(".")))?;
        fs::create_dir(output).context("replay output must be a new directory")?;
        fs::copy(&self.catalogue_path, output.join("source-catalogue.json"))?;
        let mut archived_suite = serde_json::to_value(&self.suite)?;
        archived_suite["catalogue"] = json!("source-catalogue.json");
        write_json(&output.join("source-suite.json"), &archived_suite)?;
        Ok(())
    }
}

fn replay(suite_path: &Path, output: &Path) -> Result<()> {
    let inputs = ReplayInputs::load(suite_path)?;
    inputs.archive(output)?;
    let ReplayInputs { suite, frames, .. } = inputs;
    let mut renderer = Renderer::new_profiled()?;
    const WARMUP: usize = 16;
    for _ in 0..WARMUP {
        renderer.render_next(
            &frames[0].0,
            suite.width,
            suite.height,
            luma_render::LIVE_SUBFRAMES,
        )?;
    }
    let mut images = Vec::new();
    let mut rows = Vec::new();
    // Hold readbacks until the timed sequence finishes. PNG compression and
    // filesystem writes must not interrupt successive GPU submissions.
    for (index, ((frame, build_ms), case)) in frames.iter().zip(&suite.cases).enumerate() {
        let mut pixels = Vec::new();
        let started = Instant::now();
        let timing = renderer.profile_live_into(
            frame,
            suite.width,
            suite.height,
            luma_render::LIVE_SUBFRAMES,
            &mut pixels,
        )?;
        let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        rows.push(json!({"case":case, "image":format!("frame-{index:03}.png"),
            "build_ms":build_ms, "wall_ms":wall_ms, "detail":timing,
            "draws":frame.draws.len(), "cones":frame.fixture_cones.len(),
            "shadowStats":renderer.shadow_stats(),
            "intervalCache":renderer.interval_cache_stats(),
            "compact":renderer.compact_stats(),
            "settings":{"hazeResolution":frame.haze_resolution,"hazeSteps":frame.haze_steps,
                "density":frame.haze_density,"appearance":frame.haze_appearance,
                "geometryShadows":frame.geometry_shadows,"fixtureShadows":frame.fixture_shadows,
                "liveSubframes":luma_render::LIVE_SUBFRAMES}}));
        images.push(pixels);
    }
    for (index, pixels) in images.iter().enumerate() {
        png(
            &output.join(format!("frame-{index:03}.png")),
            pixels,
            suite.width,
            suite.height,
        )?;
    }
    write_json(
        &output.join("replay.json"),
        &json!({
            "size":[suite.width,suite.height], "warmupFrames":WARMUP,
            "adapter":{"name":renderer.gpu().adapter_profile().name,
                "driver":renderer.gpu().adapter_profile().driver_info,
                "backend":renderer.gpu().adapter_profile().backend},
            "environment":capture_environment(),
            "fogVisibilityCache":renderer.fog_visibility_cache_stats()?,
            "qualityReferenceEligible":std::env::var_os("LUMA_PROFILE_OMIT").is_none(),
            "performanceReferenceEligible":std::env::var_os("LUMA_PROFILE_OMIT").is_none()
                && std::env::var_os("LUMA_PROFILE_REPEAT").is_none()
                && !std::env::var_os("LUMA_HAZE_WORK_COUNTS").is_some_and(|v| v == "1")
                && !std::env::var_os("LUMA_FOG_VISIBILITY_ASSERT").is_some_and(|v| v == "1"),
            "note":"Offscreen production rendering of successive saved-score states. State evaluation and frame assembly occur before warmup; build_ms is reported separately. Each PNG is the same frame as its timing. PNG encoding occurs after all submissions. GPU timestamps exclude readback and presentation; blocking wall includes CPU submission, GPU completion and readback. This is not sustained presented FPS.",
            "frames":rows
        }),
    )?;
    println!("replay {} frames", rows.len());
    Ok(())
}

fn capture_environment() -> std::collections::BTreeMap<&'static str, String> {
    [
        "LUMA_SHADOW_UPDATE_PROBES",
        "LUMA_FOG_BLOCK_STATS",
        "LUMA_PROFILE_REPEAT",
        "LUMA_HAZE_WORK_COUNTS",
        "LUMA_HAZE_COMPUTE",
        "LUMA_PROFILE_DETAIL",
        "LUMA_SURFACE_DEPTH_CULL",
        "LUMA_PROFILE_OMIT",
        "LUMA_GRID_FOG",
        "LUMA_WIDE_LIGHT_GROUP",
        "LUMA_VISIBILITY_REFERENCE",
        "LUMA_GEOMETRY_SHADOWS",
        "LUMA_FOG_TILE_SIZE",
        "LUMA_FOG_DEPTH_CULL",
        "LUMA_FOG_BLOCKS",
        "LUMA_FOG_VISIBILITY_CACHE",
        "LUMA_FOG_VISIBILITY_ASSERT",
        "LUMA_GEOMETRY_SHADOW_SAMPLES",
        "LUMA_SURFACE_T_SOURCE",
        "LUMA_HAZE_QUADSHARE",
        "LUMA_HAZE_QUAD_UNIFORM",
        "LUMA_HAZE_QUAD_APEX",
        "LUMA_HAZE_QUAD_GATE",
        "LUMA_HAZE_QUAD_GATE_FRAC",
        "LUMA_HAZE_QUAD_SPAN_MEAN",
        "LUMA_HAZE_QUAD_SPLIT_FETCH",
        "LUMA_HAZE_QUAD_INTERP",
        "LUMA_HAZE_COMPACT",
        "LUMA_HAZE_RESID_AFTER",
        "LUMA_HAZE_RESID_CAP",
        "LUMA_HAZE_RESID_STAGE",
        "LUMA_HAZE_RESID_LANES",
        "LUMA_HAZE_RESID_MAX_AGE_MS",
        "LUMA_HAZE_RESID_TEMPORAL",
        "LUMA_HAZE_RESID_PER_RESIDENT",
        "LUMA_HAZE_VENUE_DOMAIN",
        "LUMA_HAZE_LIGHTING_DOMAIN",
        "LUMA_HAZE_WHOLE_K_MB",
        "LUMA_HAZE_DIRECT_ARENA_MB",
        "LUMA_HAZE_SCENE_LATE",
        "LUMA_HAZE_COMPACT_LATE",
        "LUMA_HAZE_LIST_REUSE",
        "LUMA_HAZE_COMPACT_RESERVE_HINT",
        "LUMA_INTERVAL_RETAIN_INACTIVE",
        "LUMA_INTERVAL_CACHE_TABLE_BITS",
        "LUMA_INTERVAL_CACHE",
        "LUMA_INTERVAL_CACHE_PAYLOAD",
        "LUMA_INTERVAL_CACHE_UNIFY",
        "LUMA_INTERVAL_CACHE_SKIP_RESIDUAL",
        "LUMA_INTERVAL_CACHE_ALL_OFF",
    ]
    .into_iter()
    .filter_map(|key| std::env::var(key).ok().map(|value| (key, value)))
    .collect()
}

fn prepare(catalogue_path: &Path, output: &Path) -> Result<()> {
    let catalogue = Catalogue::load(catalogue_path)?;
    let mut library = library();
    let mut cases = Vec::new();
    let mut inventory = Vec::new();
    for scene in &catalogue.scenes {
        let time = *scene.times.first().context("scene has no capture time")?;
        let frame = build_frame(scene, &catalogue.definitions, time, &mut library)?;
        let framing = scene.framing(&catalogue.definitions);
        let finder = luma_scene::Viewfinder::new(50.0, 1.5).open_air(scene.render.sky.is_some());
        let front = luma_scene::Camera::for_view(luma_scene::View::Front, &framing, None, &finder);
        let center = front.target;
        let radius = front.position().distance(center);
        let mut add = |name: &str, eye: Vec3, target: Vec3, purpose: &str| {
            cases.push(Case {
                id: format!("{}-{name}", scene.id),
                scene: scene.id.clone(),
                time,
                eye: eye.to_array(),
                target: target.to_array(),
                fov: 50.0,
                purpose: purpose.into(),
            });
        };
        add(
            "wide",
            front.position(),
            center,
            "Overlapping lights and overall contrast",
        );
        add(
            "side",
            center + Vec3::new(radius, 0.0, 1.0),
            center,
            "Side-on beam endings and volume boundaries",
        );
        add(
            "inside",
            center + Vec3::new(0.0, 4.0, -5.0),
            center + Vec3::new(0.0, -8.0, -1.0),
            "Rig silhouettes and haze occlusion inside the volume",
        );
        if let Some(light) = frame
            .fixture_cones
            .iter()
            .filter(|l| l.wash >= 0.65)
            .max_by(|a, b| a.intensity.total_cmp(&b.intensity))
        {
            let axis = light.direction.normalize();
            let side = if axis.z.abs() < 0.9 {
                axis.cross(Vec3::Z).normalize()
            } else {
                Vec3::X
            };
            add(
                "source",
                light.position + axis * 5.0 + side * 1.2,
                light.position,
                "Near-source highlight and fine shadow shafts; five metres down the beam",
            );
            add(
                "source-side",
                light.position + axis * 5.0 + side * 7.0,
                light.position + axis * 4.0,
                "Close side view across the direct/grid handoff",
            );
        }
        if let Some(light) = frame
            .fixture_cones
            .iter()
            .filter(|l| l.wash >= 0.65 && l.wash < 0.95)
            .max_by(|a, b| a.intensity.total_cmp(&b.intensity))
        {
            let axis = light.direction.normalize();
            let side = if axis.z.abs() < 0.9 {
                axis.cross(Vec3::Z).normalize()
            } else {
                Vec3::X
            };
            add(
                "bar-close",
                light.position + axis * 3.5 + side * 0.7,
                light.position,
                "Close LED-bar emitter, self-shadow shafts and small overlapping emitters",
            );
        }
        inventory.push(json!({"scene":scene.id,"time":time,"haze":scene.render.haze,
            "draws":frame.draws.len(),"bounds":[frame.haze_bounds.min.to_array(),frame.haze_bounds.max.to_array()],
            "cones":frame.fixture_cones.iter().map(|l| json!({"position":l.position.to_array(),"direction":l.direction.to_array(),"range":l.range,"cosBeam":l.cos_beam,"cosField":l.cos_field,"color":l.color.to_array(),"intensity":l.intensity,"wash":l.wash})).collect::<Vec<_>>() }));
    }
    fs::create_dir_all(output)?;
    write_json(
        &output.join("suite.json"),
        &Suite {
            catalogue: fs::canonicalize(catalogue_path)?,
            width: 960,
            height: 640,
            cases,
        },
    )?;
    write_json(&output.join("inventory.json"), &inventory)?;
    println!("{}", output.join("suite.json").display());
    Ok(())
}
fn capture(suite_path: &Path, output: &Path, mode: &str, filter: Option<&str>) -> Result<()> {
    let reference_samples = match mode {
        "reference16" => 16,
        "reference32" => 32,
        "reference64" => 64,
        "reference128" => 128,
        _ => 0,
    };
    if ![
        "live",
        "uniform",
        "no-haze",
        "no-grid",
        "exact-live",
        "full-live",
        "reference16",
        "reference32",
        "reference64",
        "reference128",
    ]
    .contains(&mode)
    {
        bail!("unknown experiment {mode}");
    }
    // Set before creating the GPU or renderer. Independent invocations isolate
    // process-wide shader/environment switches; no changes to app defaults.
    if mode == "no-grid" || mode == "exact-live" || reference_samples > 0 {
        std::env::set_var("LUMA_GRID_FOG", "0");
    }
    if mode == "exact-live" || reference_samples > 0 {
        std::env::set_var("LUMA_WIDE_LIGHT_GROUP", "1");
    }
    let suite: Suite = serde_json::from_slice(&fs::read(suite_path)?)?;
    anyhow::ensure!(
        suite.width > 0 && suite.height > 0 && suite.width <= 4096 && suite.height <= 4096,
        "invalid capture dimensions"
    );
    let catalogue_path = suite_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(&suite.catalogue);
    let mut catalogue = Catalogue::load(&catalogue_path)?;
    fs::create_dir_all(output)?;
    let mut library = library();
    let mut selected = 0;
    for case in suite
        .cases
        .iter()
        .filter(|c| filter.is_none_or(|f| c.id.contains(f)))
    {
        selected += 1;
        let scene = catalogue
            .scenes
            .iter_mut()
            .find(|s| s.id == case.scene)
            .context("missing scene")?;
        scene.camera.position = three_from_world(Vec3::from_array(case.eye)).to_array();
        scene.camera.target = three_from_world(Vec3::from_array(case.target)).to_array();
        scene.render.fov = case.fov;
        let mut frame = build_frame(scene, &catalogue.definitions, case.time, &mut library)?;
        if mode == "no-haze" {
            frame.haze_density = 0.0;
        }
        if mode == "uniform" {
            frame.haze_appearance.cloudiness = 0.0;
        }
        if reference_samples == 0 {
            frame.haze_resolution = luma_render::LIVE_HAZE_RESOLUTION;
        }
        if mode == "full-live" || reference_samples > 0 {
            frame.haze_resolution = 1.0;
        }
        if reference_samples > 0 {
            frame.haze_steps = 32;
        }
        let mut renderer = Renderer::new_profiled()?;
        let dir = output.join(&case.id);
        fs::create_dir_all(&dir)?;
        let mut timings = Vec::new();
        if reference_samples > 0 {
            let pixels =
                renderer.render_reference(&frame, suite.width, suite.height, reference_samples)?;
            png(&dir.join("settled.png"), &pixels, suite.width, suite.height)?;
            frame.camera.eye.x += 0.2;
            frame.camera.target.x += 0.2;
            let pixels =
                renderer.render_reference(&frame, suite.width, suite.height, reference_samples)?;
            png(&dir.join("pan.png"), &pixels, suite.width, suite.height)?;
        } else {
            let first = renderer.render_next(
                &frame,
                suite.width,
                suite.height,
                luma_render::LIVE_SUBFRAMES,
            )?;
            png(&dir.join("first.png"), &first, suite.width, suite.height)?;
            // Frozen scene time keeps the physical medium fixed. render_next
            // still advances sample seeds/history exactly as a paused viewport.
            for i in 1..64 {
                let wall_started = Instant::now();
                let t = renderer.profile_live_frame(
                    &frame,
                    suite.width,
                    suite.height,
                    luma_render::LIVE_SUBFRAMES,
                )?;
                let wall_ms = wall_started.elapsed().as_secs_f64() * 1000.0;
                if i >= 32 {
                    timings.push(json!({"wall_ms":wall_ms,"gpu_ms":t.gpu_total_ms,"haze_ms":t.gpu_volumetric_ms,"grid_ms":t.gpu_fog_grid_ms,"scene_ms":t.gpu_scene_ms,"encode_ms":t.cpu_encode_submit_ms,"shadow_redraws":renderer.shadow_stats().redrawn_maps,"detail":t}));
                }
            }
            for i in 0..8 {
                let pixels = renderer.render_next(
                    &frame,
                    suite.width,
                    suite.height,
                    luma_render::LIVE_SUBFRAMES,
                )?;
                png(
                    &dir.join(if i == 0 {
                        "settled.png".into()
                    } else {
                        format!("still-{i}.png")
                    }),
                    &pixels,
                    suite.width,
                    suite.height,
                )?;
            }
            // The counted build's still-frame record: the camera has not moved
            // since warm-up, so this is the lit-interval cache's hit path.
            if let Some(work) = renderer.haze_work_stats()? {
                let mut work = serde_json::to_value(work)?;
                work["probe"] = json!("still-7");
                work["intervalCache"] = serde_json::to_value(renderer.interval_cache_stats())?;
                work["compact"] = serde_json::to_value(renderer.compact_stats())?;
                work["fogVisibilityCache"] =
                    serde_json::to_value(renderer.fog_visibility_cache_stats()?)?;
                write_json(&dir.join("work-counts-still.json"), &work)?;
            }
            if let Some(counts) = renderer.fog_grid_counts()? {
                let names = [
                    "cells",
                    "visibility_calls",
                    "calls_reference_layer",
                    "cell_proven_lit",
                    "cell_proven_shadowed",
                    "cell_fallback",
                    "union_proven_lit",
                    "union_proven_shadowed",
                    "union_unproven",
                    "union_unproven_cell_proven",
                    "union_proven_cell_unproven",
                    "light_iterations",
                    "block_visible_skips",
                    "cells_depth_culled",
                    "cells_span_culled",
                    "quad_calls_z0",
                ];
                let mut out = serde_json::Map::new();
                for (name, value) in names.iter().zip(counts) {
                    out.insert((*name).to_string(), json!(value));
                }
                out.insert("probe".into(), json!("still-7"));
                write_json(
                    &dir.join("fog-grid-counts.json"),
                    &serde_json::Value::Object(out),
                )?;
            }
            // Diagnostic copy runs after all timed frames, with their camera.
            if std::env::var_os("LUMA_FOG_BLOCK_STATS").is_some_and(|v| v == "1") {
                if let Some(blocks) = renderer.fog_block_stats()? {
                    let mut blocks = serde_json::to_value(blocks)?;
                    blocks["probe"] = json!("still-7");
                    blocks["cameraEye"] = json!(frame.camera.eye.to_array());
                    blocks["cameraTarget"] = json!(frame.camera.target.to_array());
                    write_json(&dir.join("fog-blocks.json"), &blocks)?;
                }
            }
            // Move both eye and target so there is no aim/zoom ambiguity.
            // History rejection is part of the production path being tested.
            for i in 1..=8 {
                frame.camera.eye.x += 0.025;
                frame.camera.target.x += 0.025;
                let pixels = renderer.render_next(
                    &frame,
                    suite.width,
                    suite.height,
                    luma_render::LIVE_SUBFRAMES,
                )?;
                if i == 8 {
                    png(&dir.join("pan.png"), &pixels, suite.width, suite.height)?;
                }
            }
            let intensities: Vec<_> = frame.fixture_cones.iter().map(|l| l.intensity).collect();
            for light in &mut frame.fixture_cones {
                light.intensity = 0.0;
            }
            let pixels = renderer.render_next(
                &frame,
                suite.width,
                suite.height,
                luma_render::LIVE_SUBFRAMES,
            )?;
            png(
                &dir.join("blackout.png"),
                &pixels,
                suite.width,
                suite.height,
            )?;
            for (light, intensity) in frame.fixture_cones.iter_mut().zip(intensities) {
                light.intensity = intensity;
            }
            let pixels = renderer.render_next(
                &frame,
                suite.width,
                suite.height,
                luma_render::LIVE_SUBFRAMES,
            )?;
            png(&dir.join("relight.png"), &pixels, suite.width, suite.height)?;
        }
        if let Some(work) = renderer.haze_work_stats()? {
            let mut work = serde_json::to_value(work)?;
            work["probe"] = json!("relight");
            work["cameraEye"] = json!(frame.camera.eye.to_array());
            work["cameraTarget"] = json!(frame.camera.target.to_array());
            work["lightIndex"] = serde_json::to_value(renderer.light_index_stats())?;
            work["shadowedFixtures"] = json!(renderer.shadowed_fixture_count());
            work["intervalCache"] = serde_json::to_value(renderer.interval_cache_stats())?;
            work["compact"] = serde_json::to_value(renderer.compact_stats())?;
            work["fogVisibilityCache"] =
                serde_json::to_value(renderer.fog_visibility_cache_stats()?)?;
            write_json(&dir.join("work-counts.json"), &work)?;
        }
        // Explicit update probes run after every quality capture and frozen
        // timing. Shift this in-memory rig by fractions of a millimetre so
        // every occupied map changes, without editing the saved venue/score.
        if reference_samples == 0
            && std::env::var_os("LUMA_SHADOW_UPDATE_PROBES").is_some_and(|v| v == "1")
        {
            let positions: Vec<_> = frame.fixture_cones.iter().map(|l| l.position).collect();
            let mut updates = Vec::new();
            for i in 1..=4 {
                let offset = Vec3::new(i as f32 * 0.0001, 0.0, 0.0);
                for (light, position) in frame.fixture_cones.iter_mut().zip(&positions) {
                    light.position = *position + offset;
                }
                let started = Instant::now();
                let mut pixels = Vec::new();
                let timing = renderer.profile_live_into(
                    &frame,
                    suite.width,
                    suite.height,
                    luma_render::LIVE_SUBFRAMES,
                    &mut pixels,
                )?;
                let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
                let image = format!("update-{i}.png");
                png(&dir.join(&image), &pixels, suite.width, suite.height)?;
                updates.push(json!({"offset":offset.to_array(), "image":image,
                    "wall_ms":wall_ms,
                    "shadowStats":renderer.shadow_stats(), "detail":timing}));
            }
            for (light, position) in frame.fixture_cones.iter_mut().zip(positions) {
                light.position = position;
            }
            write_json(
                &dir.join("shadow-updates.json"),
                &json!({
                    "case":case, "size":[suite.width,suite.height],
                    "cameraEye":frame.camera.eye.to_array(), "cameraTarget":frame.camera.target.to_array(),
                    "note":"Four isolated all-map update probes after relight; synthetic submillimetre fixture translations, not a playback-FPS measurement. No update probe enters frozen timings or quality images.",
                    "updates":updates,
                }),
            )?;
        }
        write_json(
            &dir.join("capture.json"),
            &json!({"case":case,"mode":mode,"size":[suite.width,suite.height],
            "adapter":{"name":renderer.gpu().adapter_profile().name,"driver":renderer.gpu().adapter_profile().driver_info,"backend":renderer.gpu().adapter_profile().backend},"sourceCatalogue":suite.catalogue,
            "fogVisibilityCache":renderer.fog_visibility_cache_stats()?,
            "qualityReferenceEligible":std::env::var_os("LUMA_PROFILE_OMIT").is_none(),
            "performanceReferenceEligible":std::env::var_os("LUMA_PROFILE_OMIT").is_none()
                && std::env::var_os("LUMA_PROFILE_REPEAT").is_none()
                && !std::env::var_os("LUMA_HAZE_WORK_COUNTS").is_some_and(|v| v == "1")
                && !std::env::var_os("LUMA_FOG_VISIBILITY_ASSERT").is_some_and(|v| v == "1"),
            "settings":{"hazeResolution":frame.haze_resolution,"hazeSteps":frame.haze_steps,"density":frame.haze_density,"appearance":frame.haze_appearance,"geometryShadows":frame.geometry_shadows,"fixtureShadows":frame.fixture_shadows,"referenceSubframes":reference_samples,"liveSubframes":luma_render::LIVE_SUBFRAMES},
            "environment":capture_environment(),
            "note":"Reference converges current transport with full-resolution all-light integration; not an independent physical ground truth. GPU timings use the live path and exclude GPUI/presentation/readback waits. wall_ms includes CPU submission, the complete GPU queue, pixel/query readback and blocking completion; it is not displayed FPS.","frames":timings}),
        )?;
        println!("{mode} {}", case.id);
    }
    anyhow::ensure!(selected > 0, "no cases matched");
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("inspect-reuse") if args.len() == 4 => reuse::inspect(Path::new(&args[2]), Path::new(&args[3])),
        Some("replay") if args.len() == 4 => replay(Path::new(&args[2]), Path::new(&args[3])),
        Some("prepare") if args.len() == 4 => prepare(Path::new(&args[2]), Path::new(&args[3])),
        Some("capture") if (5..=6).contains(&args.len()) => capture(Path::new(&args[2]), Path::new(&args[3]), &args[4], args.get(5).map(String::as_str)),
        _ => bail!("haze-lab prepare CATALOGUE OUTPUT_DIR | capture SUITE OUTPUT_DIR MODE [CASE_SUBSTRING] | replay SUITE OUTPUT_DIR | inspect-reuse SUITE OUTPUT_DIR"),
    }
}
