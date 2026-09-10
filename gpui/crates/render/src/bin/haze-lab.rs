//! Repeatable image-quality experiments on frozen production scenes.
//! See experiments/haze-lab.md. This tool never reads or writes the library DB.
use anyhow::{bail, Context, Result};
use glam::Vec3;
use luma_render::{assets::Library, build_frame, frame::Camera, Catalogue, Renderer};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
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
    let catalogue = Catalogue::load(&catalogue_path)?;
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
            .iter()
            .find(|s| s.id == case.scene)
            .context("missing scene")?;
        let mut frame = build_frame(scene, &catalogue.definitions, case.time, &mut library)?;
        frame.camera = Camera {
            eye: case.eye.into(),
            target: case.target.into(),
            fov_y_deg: case.fov,
        };
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
                let t = renderer.profile_live_frame(
                    &frame,
                    suite.width,
                    suite.height,
                    luma_render::LIVE_SUBFRAMES,
                )?;
                if i >= 32 {
                    timings.push(json!({"gpu_ms":t.gpu_total_ms,"haze_ms":t.gpu_volumetric_ms,"grid_ms":t.gpu_fog_grid_ms,"scene_ms":t.gpu_scene_ms,"encode_ms":t.cpu_encode_submit_ms,"shadow_redraws":renderer.shadow_stats().redrawn_maps}));
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
        write_json(
            &dir.join("capture.json"),
            &json!({"case":case,"mode":mode,"size":[suite.width,suite.height],
            "adapter":{"name":renderer.gpu().adapter_profile().name,"driver":renderer.gpu().adapter_profile().driver_info,"backend":renderer.gpu().adapter_profile().backend},"sourceCatalogue":suite.catalogue,
            "settings":{"hazeResolution":frame.haze_resolution,"hazeSteps":frame.haze_steps,"density":frame.haze_density,"appearance":frame.haze_appearance,"geometryShadows":frame.geometry_shadows,"fixtureShadows":frame.fixture_shadows,"referenceSubframes":reference_samples,"liveSubframes":luma_render::LIVE_SUBFRAMES},
            "environment":(["LUMA_GRID_FOG", "LUMA_WIDE_LIGHT_GROUP", "LUMA_VISIBILITY_REFERENCE", "LUMA_GEOMETRY_SHADOWS", "LUMA_FOG_TILE_SIZE", "LUMA_FOG_DEPTH_CULL", "LUMA_FOG_BLOCKS", "LUMA_GEOMETRY_SHADOW_SAMPLES"].into_iter().filter_map(|k| std::env::var(k).ok().map(|v| (k,v))).collect::<std::collections::BTreeMap<_,_>>()),
            "note":"Reference converges current transport with full-resolution all-light integration; not an independent physical ground truth. Timings use the live path and exclude GPUI/presentation/readback waits.","frames":timings}),
        )?;
        println!("{mode} {}", case.id);
    }
    anyhow::ensure!(selected > 0, "no cases matched");
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("prepare") if args.len() == 4 => prepare(Path::new(&args[2]), Path::new(&args[3])),
        Some("capture") if (5..=6).contains(&args.len()) => capture(Path::new(&args[2]), Path::new(&args[3]), &args[4], args.get(5).map(String::as_str)),
        _ => bail!("haze-lab prepare CATALOGUE OUTPUT_DIR | capture SUITE OUTPUT_DIR MODE [CASE_SUBSTRING]"),
    }
}
