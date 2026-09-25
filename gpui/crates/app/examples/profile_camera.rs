//! Replay one exported stage view through the real renderer and report the
//! GPU time of each pass.
//!
//! The stage's "Export camera" action writes a JSON file (and a PNG) to
//! `<config>/debug/`. This example loads that venue and score from a library,
//! evaluates the score at the export's playhead, builds the frame the way the
//! stage does, applies the export's camera and sun, and renders it at the
//! export's size with hardware timestamps.
//!
//! ```sh
//! # A COPY of the library: opening a pool is read-only here, but keep the
//! # live one out of reach. Only luma.db and the score's track files are read.
//! LUMA_CONFIG_DIR=/path/to/library/copy LUMA_PROFILE_DETAIL=1 \
//! cargo +1.97.1 run --release --manifest-path gpui/Cargo.toml -p luma-app \
//!     --example profile_camera -- EXPORT.json [--quality=high|low] \
//!     [--warmup=120] [--frames=300] [--png=OUT.png] [--compare=BASE.png]
//! ```
//!
//! `LUMA_PROFILE_DETAIL=1` adds a line for each named pass (the haze
//! sub-passes among them). Renderer switches such as `LUMA_GRID_FOG=0` apply
//! as in the app. `--quality` defaults to the quality in the export.
//! `--compare` prints how far the written PNG is from another one.
//! `--list-cones` prints the lit fixture cones at the playhead and exits.
//!
//! The haze clock runs at 60 frames a second from zero, so two runs with the
//! same arguments draw the same frames.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use luma_lib::eval::{Arena, Scope};
use luma_lib::stage_render::{meshes_root, primitive_state, VenueGeometry};
use luma_lib::storage::StorageRoot;
use luma_render::camera_export::CameraExport;
use luma_render::scene_desc::{Look, Quality};
use luma_render::{assets::Library, build_frame_with, FrameTimings, Renderer};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

fn main() -> Result<(), String> {
    env_logger::init();
    if cfg!(debug_assertions) {
        return Err("GPU timings need a --release build".into());
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .find_map(|a| a.strip_prefix(&format!("--{name}=")))
    };
    let count = |name: &str, default: usize| -> Result<usize, String> {
        flag(name).map_or(Ok(default), |v| {
            v.parse().map_err(|e| format!("--{name}: {e}"))
        })
    };
    let export_path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .ok_or("usage: profile_camera EXPORT.json [--quality=high|low] [--warmup=N] [--frames=N] [--png=OUT] [--compare=BASE]")?;
    let export = CameraExport::read(Path::new(export_path)).map_err(|e| e.to_string())?;
    let quality = match flag("quality").unwrap_or(&export.quality.to_lowercase()) {
        "high" => Quality::High,
        "low" => Quality::Low,
        other => return Err(format!("unknown quality {other:?}")),
    };
    let (warmup, measured) = (count("warmup", 120)?, count("frames", 300)?.max(1));

    let config_dir = PathBuf::from(
        std::env::var_os("LUMA_CONFIG_DIR").ok_or("LUMA_CONFIG_DIR must name a library copy")?,
    );
    if StorageRoot::from_env_default().is_ok_and(|live| live.path() == config_dir) {
        return Err("LUMA_CONFIG_DIR is the live library; point it at a copy".into());
    }
    let storage = StorageRoot::from_path(config_dir);
    let fixtures = luma_lib::headless_host::HostConfig::default().fixtures_root()?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let score_id = export
        .scene
        .score_id
        .clone()
        .ok_or("the export has no score")?;
    let (program, geometry, settings) = runtime.block_on(async {
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(storage.luma_db_path())
                    .read_only(true),
            )
            .await
            .map_err(|e| e.to_string())?;
        let program =
            luma_lib::build_score_scene(&pool, &storage, &fixtures, &score_id, None).await?;
        let geometry = VenueGeometry::load(&pool, &fixtures, &export.scene.venue_id).await?;
        let settings = luma_lib::settings::load_settings(&pool).await?;
        Ok::<_, String>((program, geometry, settings))
    })?;

    // The stage's render controls (`RenderControls::settings` in
    // `visualizer.rs`), from the device settings and the venue row.
    let (mut scene, definitions) = geometry.scene();
    scene.editing = true;
    let render = &mut scene.render;
    render.fov = export.camera.fov_y_deg;
    render.haze.steps = 8;
    render.haze.resolution = luma_render::LIVE_HAZE_RESOLUTION;
    if quality == Quality::Low {
        render.haze.steps = render.haze.steps.min(quality.haze_steps());
        render.haze.resolution = render.haze.resolution.min(quality.haze_resolution());
    }
    render.quality = quality;
    render.show_grid = settings.stage_grid;
    render.show_gizmos = settings.stage_gizmos;
    render.geometry_shadows = true;
    render.look = serde_json::from_str(&settings.stage_look).unwrap_or(Look::STAGE);

    let playhead = export.scene.playhead_s;
    let state = program
        .render(&[playhead], Scope::Composite, &mut Arena::default())
        .pop()
        .ok_or("the score produced no frame")?;
    let mut library = Library::new(meshes_root(Some(&fixtures)));
    let mut frame = build_frame_with(
        &scene,
        &definitions,
        &|id, head| primitive_state(Some(&state), id, head),
        playhead,
        &mut library,
    )
    .map_err(|e| e.to_string())?;
    export.apply(&mut frame);
    if args.iter().any(|a| a == "--list-cones") {
        for cone in &frame.fixture_cones {
            let half = cone.cos_field.clamp(-1.0, 1.0).acos().to_degrees();
            println!(
                "cone at {:.2?} dir {:.2?} half-field {half:.1} deg wash {:.2} intensity {:.2} colour {:.2?} range {:.1}",
                cone.position.to_array(),
                cone.direction.to_array(),
                cone.wash,
                cone.intensity,
                cone.color.to_array(),
                cone.range,
            );
        }
        return Ok(());
    }
    let (width, height) = export.render_size();
    if (frame.haze_density, frame.haze_steps) != (export.haze.density, export.haze.steps)
        || frame.haze_resolution != export.haze.resolution
    {
        eprintln!(
            "note: haze density/steps/resolution {}/{}/{} differ from the export's {}/{}/{}",
            frame.haze_density,
            frame.haze_steps,
            frame.haze_resolution,
            export.haze.density,
            export.haze.steps,
            export.haze.resolution
        );
    }

    let mut renderer = Renderer::new_profiled().map_err(|e| e.to_string())?;
    let mut samples = Vec::with_capacity(measured);
    let mut redrawn = 0;
    let mut pixels = Vec::new();
    for i in 0..warmup + measured {
        frame.time = i as f32 / 60.0;
        let last = i + 1 == warmup + measured;
        let timing = if last {
            renderer.profile_live_into(
                &frame,
                width,
                height,
                luma_render::LIVE_SUBFRAMES,
                &mut pixels,
            )
        } else {
            renderer.profile_live_frame(&frame, width, height, luma_render::LIVE_SUBFRAMES)
        }
        .map_err(|e| e.to_string())?;
        if i >= warmup {
            redrawn = redrawn.max(renderer.shadow_stats().redrawn_maps);
            samples.push(timing);
        }
    }

    println!(
        "{} / score {} at {playhead:.2}s, {width}x{height}, {quality:?}, haze {} steps at {}, \
         {} cones, {} lights per tile, {} shadow maps redrawn at most, {measured} frames",
        export.scene.venue_name,
        score_id,
        frame.haze_steps,
        frame.haze_resolution,
        frame.fixture_cones.len(),
        renderer.light_index_stats().mean_lights_per_tile,
        redrawn,
    );
    println!("{:<28} {:>8} {:>8}", "ms", "median", "p95");
    let row = |name: &str, values: Vec<f64>| {
        let (median, p95) = percentiles(values);
        println!("{name:<28} {median:>8.2} {p95:>8.2}");
    };
    let field = |f: fn(&FrameTimings) -> f64| samples.iter().map(f).collect::<Vec<_>>();
    row("gpu_total", field(|s| s.gpu_total_ms));
    row("  scene", field(|s| s.gpu_scene_ms));
    row("  volumetric (haze)", field(|s| s.gpu_volumetric_ms));
    row("  composite", field(|s| s.gpu_composite_ms));
    row("fog grid", field(|s| s.gpu_fog_grid_ms));
    row("  prepare", field(|s| s.gpu_fog_prepare_ms));
    row("  light", field(|s| s.gpu_fog_light_ms));
    row("  integrate", field(|s| s.gpu_fog_integrate_ms));
    row("cpu encode+submit", field(|s| s.cpu_encode_submit_ms));

    // Named pass brackets (LUMA_PROFILE_DETAIL=1). They include waits on
    // earlier passes and may overlap, so they do not add up to the total.
    let mut order = Vec::new();
    let mut per_pass: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for sample in &samples {
        let mut frame_sums: BTreeMap<&str, f64> = BTreeMap::new();
        for pass in &sample.passes {
            if !order.contains(&pass.name) {
                order.push(pass.name);
            }
            *frame_sums.entry(pass.name).or_default() += pass.end_ms - pass.start_ms;
        }
        for (name, ms) in frame_sums {
            per_pass.entry(name).or_default().push(ms);
        }
    }
    if !order.is_empty() {
        println!("passes (brackets, may overlap)");
    }
    for name in order {
        row(
            &format!("  {name}"),
            per_pass.remove(name).unwrap_or_default(),
        );
    }

    if let Some(out) = flag("png") {
        luma_render::image_out::write(Path::new(out), &pixels, width, height)
            .map_err(|e| e.to_string())?;
        println!("wrote {out}");
    }
    if let Some(base) = flag("compare") {
        let (base_width, base_height, base_pixels) = read_png(Path::new(base))?;
        if (base_width, base_height) != (width, height) {
            return Err(format!(
                "{base} is {base_width}x{base_height}, not {width}x{height}"
            ));
        }
        print_difference(&pixels, &base_pixels);
    }
    Ok(())
}

fn percentiles(mut values: Vec<f64>) -> (f64, f64) {
    if values.is_empty() {
        return (f64::NAN, f64::NAN);
    }
    values.sort_by(f64::total_cmp);
    let at = |q: f64| values[((values.len() - 1) as f64 * q).round() as usize];
    (at(0.5), at(0.95))
}

fn read_png(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    buffer.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => buffer
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        other => return Err(format!("unsupported PNG colour type {other:?}")),
    };
    Ok((info.width, info.height, rgba))
}

/// Per-channel RGB difference: mean, largest, and the share of pixels whose
/// largest channel difference is above 2, 8 and 32 levels.
fn print_difference(a: &[u8], b: &[u8]) {
    let (mut sum, mut max, mut over) = (0_u64, 0_u8, [0_usize; 3]);
    let pixels = a.len() / 4;
    for (p, q) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let worst = (0..3).map(|c| p[c].abs_diff(q[c])).max().unwrap_or(0);
        sum += (0..3).map(|c| u64::from(p[c].abs_diff(q[c]))).sum::<u64>();
        max = max.max(worst);
        for (count, limit) in over.iter_mut().zip([2, 8, 32]) {
            *count += usize::from(worst > limit);
        }
    }
    let share = |n: usize| 100.0 * n as f64 / pixels as f64;
    println!(
        "difference: mean {:.3} levels, max {max}, >2: {:.2}%, >8: {:.2}%, >32: {:.3}%",
        sum as f64 / (pixels * 3) as f64,
        share(over[0]),
        share(over[1]),
        share(over[2]),
    );
}
