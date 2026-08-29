//! Diagnostic probe, not a gate: quantify how far pass-boundary timestamp
//! queries under-report GPU time on Metal/Apple Silicon.
//!
//! Two experiments:
//! 1. `begin_only_timestamps_undercount_fragment_time` — a standalone wgpu
//!    device replicating the renderer's exact pattern (begin-of-pass
//!    timestamps only, delta between consecutive pass starts) against a
//!    deliberately heavy fragment pass, compared with (a) wall time around
//!    submit→wait and (b) a begin+end pair on the same pass.
//! 2. `production_profiler_matches_wall_on_lit_frame` — the renderer's own
//!    profiler machinery (`Renderer::profile_live_frame`) on a heavy lit
//!    frame, gating `gpu_total_ms` against the wall clock. This one is a
//!    regression gate, not a probe: it fails if the timestamps start lying
//!    again.
//!
//! Run: cargo test -p luma-render --release timestamp_lie -- --nocapture

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;

use glam::Vec3;
use luma_render::assets::Library;
use luma_render::build_frame_with;
use luma_render::frame::{FixtureCone, Frame};
use luma_render::scene_desc::{CameraPose, DebugView, Environment, RenderSettings, Scene};
use luma_render::Renderer;

const SIZE: u32 = 2048;

const SHADER: &str = r#"
@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let x = f32(i32(i % 2u) * 4 - 1);
    let y = f32(i32(i / 2u) * 4 - 1);
    return vec4f(x, y, 0.0, 1.0);
}

// Heavy ALU loop. `sin` and a position-derived seed keep the compiler from
// folding the loop away; the accumulated value reaches the output so the
// loop is observable.
@fragment
fn fs_heavy(@builtin(position) p: vec4f) -> @location(0) vec4f {
    var v = p.x * 1e-4 + p.y * 1e-5 + 0.5;
    for (var i = 0u; i < 6000u; i = i + 1u) {
        v = v + sin(v) * 1e-6 + 1e-7;
    }
    return vec4f(fract(v), 0.0, 0.0, 1.0);
}

@fragment
fn fs_trivial(@builtin(position) p: vec4f) -> @location(0) vec4f {
    return vec4f(0.0, 0.0, 0.0, 0.0);
}
"#;

/// Standalone reproduction of the renderer's timestamp pattern.
///
/// Query layout mirrors `Renderer::submit_readback`:
///   q0 = beginning_of_pass of the heavy pass   (renderer: first scene pass)
///   q1 = beginning_of_pass of a trivial pass   (renderer: the profile fence)
/// plus what the renderer does NOT record:
///   q2 = end_of_pass of the heavy pass
/// "luma-style" time for the heavy pass is q1 - q0; the same-pass bracket is
/// q2 - q0; ground truth is the wall clock around submit → queue drained.
#[test]
fn begin_only_timestamps_undercount_fragment_time() {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
        apply_limit_buckets: false,
    }))
    .expect("adapter");
    assert!(
        adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY),
        "adapter lacks TIMESTAMP_QUERY; probe is meaningless here"
    );
    let inside_encoders = adapter
        .features()
        .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);
    let inside_passes = adapter
        .features()
        .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("timestamp-lie"),
        required_features: wgpu::Features::TIMESTAMP_QUERY,
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .expect("device");
    let info = adapter.get_info();
    println!(
        "adapter: {} ({:?}); INSIDE_ENCODERS={inside_encoders} INSIDE_PASSES={inside_passes}",
        info.name, info.backend
    );

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("timestamp-lie"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor::default());
    let pipeline = |entry: &str| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(entry),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
            }),
            multiview_mask: None,
            cache: None,
        })
    };
    let heavy = pipeline("fs_heavy");
    let trivial = pipeline("fs_trivial");

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());

    let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some("timestamps"),
        ty: wgpu::QueryType::Timestamp,
        count: 4,
    });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("resolve"),
        size: 32,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: 32,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    // Warm once (pipeline compilation, first-submit costs) before measuring.
    for measured in [false, true] {
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("heavy"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: Some(wgpu::RenderPassTimestampWrites {
                    query_set: &queries,
                    beginning_of_pass_write_index: Some(0),
                    end_of_pass_write_index: Some(2),
                }),
                ..Default::default()
            });
            pass.set_pipeline(&heavy);
            pass.draw(0..3, 0..1);
        }
        {
            // The renderer's "fence" pass: its *beginning* timestamp is the
            // end marker for everything before it.
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fence"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: Some(wgpu::RenderPassTimestampWrites {
                    query_set: &queries,
                    beginning_of_pass_write_index: Some(1),
                    end_of_pass_write_index: Some(3),
                }),
                ..Default::default()
            });
            pass.set_pipeline(&trivial);
            pass.draw(0..0, 0..1);
        }
        encoder.resolve_query_set(&queries, 0..4, &resolve, 0);
        encoder.copy_buffer_to_buffer(&resolve, 0, &readback, 0, 32);
        let started = Instant::now();
        queue.submit([encoder.finish()]);
        let (done_tx, done_rx) = mpsc::channel();
        queue.on_submitted_work_done(move || {
            let _ = done_tx.send(Instant::now());
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let done_at = done_rx.recv().expect("work-done callback");
        let wall_ms = (done_at - started).as_secs_f64() * 1e3;

        let (map_tx, map_rx) = mpsc::channel();
        readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = map_tx.send(r);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        map_rx.recv().expect("map callback").expect("map ok");
        let mut ts = [0u64; 4];
        {
            let data = readback.slice(..).get_mapped_range().expect("mapped");
            for (t, chunk) in ts.iter_mut().zip(data.chunks_exact(8)) {
                *t = u64::from_ne_bytes(chunk.try_into().unwrap());
            }
        }
        readback.unmap();
        if !measured {
            continue;
        }

        let period = f64::from(queue.get_timestamp_period());
        let ms = |delta: u64| delta as f64 * period / 1e6;
        println!("raw timestamps: {ts:?} (period {period} ns/tick)");
        let luma_style = ms(ts[1].saturating_sub(ts[0]));
        let same_pass = ms(ts[2].saturating_sub(ts[0]));
        let fence_span = ms(ts[3].saturating_sub(ts[1]));
        println!("wall submit->done:                {wall_ms:8.3} ms");
        println!("begin(heavy)->begin(fence) [luma]:{luma_style:8.3} ms");
        println!("begin(heavy)->end(heavy) [honest]:{same_pass:8.3} ms");
        println!("begin(fence)->end(fence):         {fence_span:8.3} ms");
        println!(
            "under-report factor (wall / luma-style): {:.1}x",
            wall_ms / luma_style.max(1e-6)
        );
        assert!(ts[0] > 0, "start timestamp never sampled");
        assert!(
            wall_ms > 4.0,
            "workload too light to distinguish scheduling from execution ({wall_ms:.3} ms)"
        );
    }
}

// --- production machinery -------------------------------------------------
// Scene construction lifted from tests/stall_probe.rs: a lit stage with haze,
// the workload whose live numbers prompted this probe.

fn scene() -> Scene {
    let mut render = RenderSettings::dark_stage(48.0, 0.5);
    render.environment = Environment::DARK;
    render.sun = None;
    render.show_grid = false;
    render.haze.enabled = true;
    render.haze.steps = 8;
    render.haze.density = 0.65;
    render.debug_view = DebugView::Pbr;
    Scene {
        id: "timestamp-lie".into(),
        times: vec![0.0],
        camera: CameraPose {
            position: [4.5, 3.0, 5.0],
            target: [0.0, 0.8, 0.0],
        },
        editing: false,
        render,
        selected_fixture_ids: Vec::new(),
        editor: Default::default(),
        fixtures: Vec::new(),
        pieces: Vec::new(),
        state: BTreeMap::new(),
    }
}

fn lit_frame(lights: usize) -> Frame {
    let meshes = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../resources/meshes");
    let mut library = Library::new(meshes);
    let scene = scene();
    let mut frame = build_frame_with(&scene, &BTreeMap::new(), &|_, _| None, 0.0, &mut library)
        .expect("frame builds");
    frame.fixture_cones = (0..lights)
        .map(|i| {
            let angle = i as f32 * 0.7;
            FixtureCone {
                position: Vec3::new((i % 8) as f32 - 4.0, 4.0, (i / 8) as f32 - 2.0),
                range: 8.0,
                direction: Vec3::new(angle.sin() * 0.3, -1.0, angle.cos() * 0.3).normalize(),
                cos_beam: 0.975,
                color: Vec3::new(1.0, 0.3, 0.2),
                intensity: 0.4,
                cos_field: 0.93,
                wash: 0.0,
                gobo: 0,
                gobo_rotation: 0.0,
            }
        })
        .collect();
    frame.haze_density = 0.65;
    frame
}

/// Gate: the profiler's `gpu_total_ms` must track the wall clock on a frame
/// heavy enough that scheduling spacing and shader time are far apart. The
/// begin-to-begin scheme this replaced reported 0.30 ms for a 42 ms frame.
#[test]
fn production_profiler_matches_wall_on_lit_frame() {
    // Both sizes, because which samples a too-early query resolve drops turns
    // out to depend on the viewport: 1920x1080 loses the composite pass's end
    // every time, 2558x1357 never does.
    for (width, height) in [(1920, 1080), (2558, 1357)] {
        let mut renderer = Renderer::new_profiled().expect("profiled renderer");
        let frame = lit_frame(48);
        // Warm: pipeline + upload costs land on the first frame.
        renderer
            .profile_live_frame(&frame, width, height, 1)
            .expect("warm frame");
        for run in 0..3 {
            let started = Instant::now();
            let timings = renderer
                .profile_live_frame(&frame, width, height, 1)
                .expect("profiled frame");
            let wall_ms = started.elapsed().as_secs_f64() * 1e3;
            let parts = timings.gpu_scene_ms + timings.gpu_volumetric_ms + timings.gpu_composite_ms;
            println!(
                "{width}x{height} run {run}: wall {wall_ms:7.2} ms | gpu_total {:6.3} ms \
             (scene {:6.3} + volumetric {:6.3} + composite {:6.3}) | index {:6.3} \
             | cpu encode+submit {:6.3} ms",
                timings.gpu_total_ms,
                timings.gpu_scene_ms,
                timings.gpu_volumetric_ms,
                timings.gpu_composite_ms,
                timings.gpu_index_ms,
                timings.cpu_encode_submit_ms,
            );
            // The GPU cannot have taken longer than the wall clock around it, nor
            // can it plausibly have taken a small fraction of it: the wall adds
            // only encode and image readback.
            let gpu_bound = wall_ms - timings.cpu_encode_submit_ms;
            assert!(
                timings.gpu_total_ms <= wall_ms,
                "gpu_total {:.3} ms exceeds wall {wall_ms:.3} ms",
                timings.gpu_total_ms
            );
            assert!(
                timings.gpu_total_ms > gpu_bound * 0.5,
                "gpu_total {:.3} ms is under half of the {gpu_bound:.3} ms the frame had to fill; \
             timestamps are under-reporting again",
                timings.gpu_total_ms
            );
            // The regions are cut at consecutive fragment completions, so they
            // partition the total rather than merely fitting inside it.
            assert!(
                (parts - timings.gpu_total_ms).abs() < 1e-6,
                "regions {parts:.6} ms do not partition gpu_total {:.6} ms",
                timings.gpu_total_ms
            );
            // Haze dominates this frame; that is the cost the old scheme hid.
            assert!(
                timings.gpu_volumetric_ms > timings.gpu_composite_ms,
                "volumetric {:.3} ms should dominate composite {:.3} ms on a hazed lit frame",
                timings.gpu_volumetric_ms,
                timings.gpu_composite_ms,
            );
        }
    }
}
