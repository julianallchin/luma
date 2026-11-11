//! Replay one snapshot of a score through the renderer alone and report the
//! GPU time of each pass.
//!
//! `luma-cli profile SCORE OUT.json 1 1 1 snapshot TIMES.json` writes the
//! venue, its fixtures and the score's state at each time. This example
//! builds the frame from that file, so it needs no library and no backend,
//! and a renderer change rebuilds only this crate.
//!
//! ```sh
//! LUMA_PROFILE_DETAIL=1 cargo run --release --manifest-path gpui/Cargo.toml \
//!     -p luma-render --example profile_scene -- SNAPSHOT.json [--scene=0] \
//!     [--quality=high|medium|low] [--size=1402x714] \
//!     [--eye=x,y,z --target=x,y,z] [--fov=50] [--haze=DENSITY] \
//!     [--look=JSON] [--cones=N] [--warmup=120] [--frames=300] [--png=OUT.png] \
//!     [--paced=HZ] [--seconds=4]
//! ```
//!
//! `--eye` and `--target` are the renderer's world (Z up, metres), as the
//! stage's hitch report and "Export camera" write them; without them the
//! snapshot's camera is used. `--look` is the stage's stored look
//! (`settings.stage_look`). `--cones=N` keeps N of the lit cones, evenly
//! spaced, to split a frame's cost into its fixed and per-light parts.
//! `LUMA_PROFILE_DETAIL=1` adds a line for each named pass.
//!
//! The haze clock runs at 60 frames a second from zero, so two runs with the
//! same arguments draw the same frames. Frames are drawn back to back, which
//! keeps the GPU at its full clock: a live window paces them and reads higher.
//!
//! `--paced=HZ` drives the frame the way the stage does instead: a display
//! tick every 1/HZ seconds takes the newest finished frame and submits the
//! next through [`AsyncViewport`], for `--seconds`. It reports the delivered
//! rate and spacing, which is what a viewer sees, without a window or its
//! compositor.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use std::time::{Duration, Instant};

use glam::Vec3;
use luma_render::frame::Frame;
use luma_render::scene_desc::{Look, Quality};
use luma_render::viewport::AsyncViewport;
use luma_render::{assets::Library, build_frame, Catalogue, FrameTimings, MetricSummary, Renderer};

fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        !cfg!(debug_assertions),
        "GPU timings need a --release build"
    );
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .find_map(|a| a.strip_prefix(&format!("--{name}=")))
    };
    let count = |name: &str, default: usize| -> anyhow::Result<usize> {
        Ok(flag(name).map_or(Ok(default), str::parse)?)
    };
    let vec3 = |name: &str| -> anyhow::Result<Option<Vec3>> {
        let Some(text) = flag(name) else {
            return Ok(None);
        };
        let parts: Vec<f32> = text.split(',').map(str::parse).collect::<Result<_, _>>()?;
        anyhow::ensure!(parts.len() == 3, "--{name} is x,y,z");
        Ok(Some(Vec3::new(parts[0], parts[1], parts[2])))
    };
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .ok_or_else(|| anyhow::anyhow!("usage: profile_scene SNAPSHOT.json [--scene=N] [--quality=…] [--size=WxH] [--eye=… --target=…] [--cones=N] [--png=OUT]"))?;
    let mut catalogue = Catalogue::load(Path::new(path))?;
    build_library_fixtures(&mut catalogue)?;

    let index = count("scene", 0)?;
    let mut scene = catalogue
        .scenes
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("the snapshot has {} scenes", catalogue.scenes.len()))?
        .clone();
    let quality = flag("quality").map_or(Ok(Quality::High), |name| {
        Quality::parse(name).ok_or_else(|| anyhow::anyhow!("unknown quality {name:?}"))
    })?;
    let (width, height) = match flag("size") {
        Some(size) => {
            let (w, h) = size
                .split_once('x')
                .ok_or_else(|| anyhow::anyhow!("--size is WIDTHxHEIGHT"))?;
            (w.parse()?, h.parse()?)
        }
        None => catalogue.frame_size(),
    };
    let (warmup, measured) = (count("warmup", 120)?, count("frames", 300)?.max(1));

    // The stage's render controls (`RenderControls::settings`).
    scene.editing = true;
    let render = &mut scene.render;
    if let Some(fov) = flag("fov") {
        render.fov = fov.parse()?;
    }
    (render.haze.steps, render.haze.resolution) =
        quality.limit_haze(8, luma_render::LIVE_HAZE_RESOLUTION);
    if let Some(density) = flag("haze") {
        render.haze.density = density.parse()?;
    }
    render.quality = quality;
    render.show_grid = false;
    render.show_gizmos = false;
    render.geometry_shadows = true;
    if let Some(look) = flag("look") {
        render.look = Look::stored(look);
    }
    let fov = render.fov;

    let time = *scene
        .times
        .first()
        .ok_or_else(|| anyhow::anyhow!("the scene has no time"))?;
    let mut library = Library::new(luma_scene::meshes_root());
    let camera = match (vec3("eye")?, vec3("target")?) {
        (Some(eye), Some(target)) => Some(luma_render::frame::Camera {
            eye,
            target,
            fov_y_deg: fov,
        }),
        _ => None,
    };
    let keep: Option<usize> = flag("cones").map(str::parse).transpose()?;
    let mut lit = 0;
    let mut build = |clock: f32| -> anyhow::Result<Frame> {
        let mut frame = build_frame(&scene, &catalogue.definitions, time, &mut library)?;
        if let Some(camera) = camera {
            frame.camera = camera;
        }
        lit = frame.fixture_cones.len();
        if let Some(keep) = keep.filter(|keep| *keep < lit) {
            let all = std::mem::take(&mut frame.fixture_cones);
            frame.fixture_cones = (0..keep).map(|i| all[i * lit / keep]).collect();
        }
        frame.time = clock;
        Ok(frame)
    };

    if let Some(hz) = flag("paced") {
        let hz: f64 = hz.parse()?;
        let seconds = flag("seconds").map_or(Ok(4.0), str::parse::<f64>)?;
        return paced(&mut build, (width, height), hz, seconds);
    }

    let mut frame = build(0.0)?;
    let frame_id = scene.id.clone();
    let mut renderer = Renderer::new_profiled()?;
    let mut samples = Vec::with_capacity(measured);
    let mut pixels = Vec::new();
    for i in 0..warmup + measured {
        frame.time = i as f32 / 60.0;
        let timing = if i + 1 == warmup + measured {
            renderer.profile_live_into(
                &frame,
                width,
                height,
                luma_render::LIVE_SUBFRAMES,
                &mut pixels,
            )
        } else {
            renderer.profile_live_frame(&frame, width, height, luma_render::LIVE_SUBFRAMES)
        }?;
        if i >= warmup {
            samples.push(timing);
        }
    }

    println!(
        "{} at {time:.2}s, {width}x{height}, {quality:?}, haze {} at {} steps and {} of the output, \
         {} of {lit} cones, {:.1} lights per tile, {measured} frames",
        frame_id,
        frame.haze_density,
        frame.haze_steps,
        frame.haze_resolution,
        frame.fixture_cones.len(),
        renderer.light_index_stats().mean_lights_per_tile,
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
    row("cpu encode+submit", field(|s| s.cpu_encode_submit_ms));

    // Named pass brackets (LUMA_PROFILE_DETAIL=1). They include waits on
    // earlier passes and may overlap, so they do not add up to the total.
    let mut order = Vec::new();
    let mut per_pass: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for sample in &samples {
        let mut sums: BTreeMap<&str, f64> = BTreeMap::new();
        for pass in &sample.passes {
            if !order.contains(&pass.name) {
                order.push(pass.name);
            }
            *sums.entry(pass.name).or_default() += pass.end_ms - pass.start_ms;
        }
        for (name, ms) in sums {
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
        luma_render::image_out::write(Path::new(out), &pixels, width, height)?;
        println!("wrote {out}");
    }
    Ok(())
}

/// Drive `build`'s frame as the stage does: each display tick takes the
/// newest finished frame and submits the next.
fn paced(
    build: &mut dyn FnMut(f32) -> anyhow::Result<Frame>,
    (width, height): (u32, u32),
    hz: f64,
    seconds: f64,
) -> anyhow::Result<()> {
    let mut viewport = AsyncViewport::new();
    viewport.set_subframes(luma_render::LIVE_SUBFRAMES);
    let tick = Duration::from_secs_f64(1.0 / hz);
    let start = Instant::now();
    // The first second warms the pipelines and the clock governor.
    let warm = Duration::from_secs(1);
    let run = warm + Duration::from_secs_f64(seconds);
    let (mut ticks, mut delivered, mut finished_at_warm) = (0_u64, 0_u64, None);
    let mut last: Option<Instant> = None;
    let (mut intervals, mut draws, mut signalled, mut gpu) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut profiled = None;
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    loop {
        let now = start.elapsed();
        if now >= run {
            break;
        }
        let measuring = now >= warm;
        if measuring && finished_at_warm.is_none() {
            finished_at_warm = Some(viewport.finished().0);
        }
        if let Some(result) = viewport.take_latest() {
            let presented = result?;
            let at = Instant::now();
            let previous = last.replace(at);
            if measuring {
                delivered += 1;
                if let Some(previous) = previous {
                    intervals.push(ms(at - previous));
                }
                draws.push(ms(presented.draw_time));
                signalled.extend(presented.until_signalled.map(ms));
                if presented.timings_serial != profiled {
                    profiled = presented.timings_serial;
                    gpu.extend(presented.timings.map(|t| t.gpu_total_ms));
                }
            }
        }
        viewport.submit(build(now.as_secs_f32())?, width, height);
        ticks += 1;
        // Sleep to the next tick boundary, as a display link would wake.
        let next = tick * ticks as u32;
        if let Some(wait) = next.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
    }
    let rendered = viewport.finished().0 - finished_at_warm.unwrap_or(0);
    let line = |name: &str, values: Vec<f64>| {
        let summary = MetricSummary::of(values);
        let (p50, p95) = summary.map_or((f64::NAN, f64::NAN), |s| (s.p50_ms, s.p95_ms));
        println!("{name:<28} {p50:>8.2} {p95:>8.2}");
    };
    println!(
        "paced at {hz} Hz for {seconds} s, {width}x{height}: {:.1} delivered and {:.1} rendered a second",
        delivered as f64 / seconds,
        rendered as f64 / seconds,
    );
    println!("{:<28} {:>8} {:>8}", "ms", "median", "p95");
    line("interval", intervals);
    line("draw (submit to seen)", draws);
    line("  until signalled", signalled);
    line("gpu_total (1 in 16)", gpu);
    Ok(())
}

/// A library fixture's build is never stored with its definition: build each
/// `lib:<id>` definition from its spec in the repository's fixture library.
fn build_library_fixtures(catalogue: &mut Catalogue) -> anyhow::Result<()> {
    let libraries = luma_scene::repo_root().join("resources/fixture-library");
    let newest = std::fs::read_dir(&libraries)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .max();
    for (path, definition) in &mut catalogue.definitions {
        let Some(id) = path.strip_prefix("lib:") else {
            continue;
        };
        let root = newest
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no fixture library in {}", libraries.display()))?;
        let spec = root.join("fixtures").join(format!("{id}.json"));
        let text = std::fs::read_to_string(&spec)
            .map_err(|e| anyhow::anyhow!("{}: {e}", spec.display()))?;
        let fixture = luma_fixture::Fixture::from_json(&text)
            .map_err(|e| anyhow::anyhow!("{}: {e}", spec.display()))?;
        let built = luma_fixture::build(&fixture)
            .map_err(|e| anyhow::anyhow!("{}: {e}", spec.display()))?;
        definition.built = Some(Arc::new(built));
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
