//! Assembling WGSL modules from the shared shader preludes, compiling them,
//! and baking pipeline overrides into constants.

use std::borrow::Cow;

/// The shared bind-group declarations every scene-geometry shader opens with;
/// see `scene_bindings.wgsl`.
pub(super) fn scene_bindings_wgsl() -> String {
    format!(
        "{}{}{}{}{}{}{}{}{}",
        crate::haze_field::prelude(),
        include_str!("../shaders/medium.wgsl"),
        include_str!("../shaders/scene_bindings.wgsl"),
        include_str!("../shaders/haze_glow_sample.wgsl"),
        crate::atmosphere::surface_prelude(),
        include_str!("../shaders/horizon.wgsl"),
        include_str!("../shaders/haze_daylight.wgsl"),
        include_str!("../shaders/shaft_upsample.wgsl"),
        include_str!("../shaders/outdoor_surface.wgsl"),
    )
}

/// The volumetric transport (ray reconstruction, per-light integral and
/// group-0 layout) that both volumetric passes prepend.
pub(super) fn beam_transport_wgsl() -> String {
    format!(
        "{}{}const CACHE_K: u32 = {}u;\n{}",
        include_str!("../shaders/medium.wgsl"),
        crate::fog_grid::prelude(),
        crate::interval_cache::CACHE_K,
        include_str!("../shaders/beam_transport.wgsl")
    )
}

/// The composite module's source.
pub(super) fn composite_wgsl() -> String {
    format!(
        "{}{}{}{}{}{}{}{}",
        crate::haze_field::prelude(),
        include_str!("../shaders/medium.wgsl"),
        crate::atmosphere::composite_prelude(),
        include_str!("../shaders/haze_daylight.wgsl"),
        include_str!("../shaders/shaft_upsample.wgsl"),
        include_str!("../shaders/tone.wgsl"),
        include_str!("../shaders/haze_glow_sample.wgsl"),
        include_str!("../shaders/composite.wgsl")
    )
}

/// The scene module's source.
///
/// The local reflection probes (`probes.rs`) are in it: the scene pass
/// samples them, and their relight is this module with its entry points
/// appended, so a probe texel is lit by the functions that light the surface
/// it stands for. The light-index prelude is authored against group 1 (the
/// haze pass's slot); the surface pass carries the same bindings inside its
/// group 3, so its copy is rebound by this one documented replace.
pub(super) fn scene_wgsl() -> String {
    format!(
        "{}{}{}{}{}{}{}{}{}",
        scene_bindings_wgsl(),
        crate::probes::prelude(),
        include_str!("../shaders/probe_sample.wgsl"),
        crate::light_index::prelude().replace("@group(1)", "@group(3)"),
        include_str!("../shaders/fixture_light.wgsl"),
        include_str!("../shaders/visibility.wgsl"),
        include_str!("../shaders/scene.wgsl"),
        include_str!("../shaders/floor.wgsl"),
        include_str!("../shaders/probe_debug.wgsl"),
    )
}

pub(crate) fn shader(device: &wgpu::Device, label: &str, src: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(src)),
    })
}

pub(crate) fn specialize_wgsl_overrides(source: &str, values: &[(&str, f64)]) -> String {
    source
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let Some(declaration) = trimmed.strip_prefix("override ") else {
                return line.to_owned();
            };
            let (name, remainder) = declaration.split_once(':').expect("WGSL override type");
            let Some(value) = values
                .iter()
                .find_map(|(key, value)| (*key == name).then_some(*value))
            else {
                return line.replacen("override", "const", 1);
            };
            let (kind, _) = remainder
                .split_once('=')
                .expect("WGSL override initializer");
            let literal = match kind.trim() {
                "bool" => match value {
                    0.0 => "false".to_owned(),
                    1.0 => "true".to_owned(),
                    _ => panic!("boolean WGSL override {name} must be zero or one"),
                },
                "u32" => format!("{}u", value as u32),
                "i32" => format!("{}i", value as i32),
                "f32" => {
                    assert!(value.is_finite(), "WGSL override {name} must be finite");
                    let mut literal = value.to_string();
                    if !literal.contains(['.', 'e', 'E']) {
                        literal.push_str(".0");
                    }
                    literal
                }
                other => panic!("unsupported WGSL override type {other}"),
            };
            let indent = &line[..line.len() - trimmed.len()];
            format!("{indent}const {name}: {} = {literal};", kind.trim())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::specialize_wgsl_overrides;

    #[test]
    fn wgsl_override_baking_preserves_defaults_and_supplied_types() {
        let source = "override A: bool = false;\noverride B: u32 = 7u;\noverride C: f32 = 2.5;\noverride D: i32 = -3;";
        let baked =
            specialize_wgsl_overrides(source, &[("A", 1.0), ("B", 11.0), ("C", 4.0), ("D", -9.0)]);
        assert_eq!(
            baked,
            "const A: bool = true;\nconst B: u32 = 11u;\nconst C: f32 = 4.0;\nconst D: i32 = -9i;"
        );
        assert_eq!(
            specialize_wgsl_overrides(source, &[]),
            "const A: bool = false;\nconst B: u32 = 7u;\nconst C: f32 = 2.5;\nconst D: i32 = -3;"
        );
    }

    #[test]
    fn haze_jitter_advances_beyond_eight_frames() {
        // Execute the renderer's actual WGSL, not a CPU reimplementation.
        // The old toroidal tile walk repeated exactly every eight frames and
        // prevented both temporal history and offline references converging.
        let transport = include_str!("../shaders/beam_transport.wgsl");
        let start = transport.find("const BLUE_NOISE_RANK =").unwrap();
        let end = transport.find("/// One fragment's camera ray").unwrap();
        let source = format!(
            "{}\n{}",
            &transport[start..end],
            r"
            @group(0) @binding(0) var<storage, read_write> samples: array<vec2<f32>>;
            @compute @workgroup_size(64)
            fn probe(@builtin(global_invocation_id) id: vec3<u32>) {
                let pixel = vec2<f32>(23.5, 45.5);
                samples[id.x] = vec2<f32>(blue_noise(pixel, id.x), blue_noise(pixel, id.x + 8u));
            }"
        );
        let gpu = crate::gpu::Gpu::shared().unwrap();
        let device = &gpu.device;
        let module = super::shader(device, "haze-jitter-probe", &source);
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("haze-jitter-probe"),
            layout: None,
            module: &module,
            entry_point: Some("probe"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let buffer = |usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("haze-jitter-samples"),
                size: 64 * 8,
                usage,
                mapped_at_creation: false,
            })
        };
        let samples = buffer(wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC);
        let readback = buffer(wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ);
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("haze-jitter-probe"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[crate::gpu::binding(0, samples.as_entire_binding())],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&samples, 0, &readback, 0, 64 * 8);
        gpu.queue.submit([encoder.finish()]);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, |result| result.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let view = readback.slice(..).get_mapped_range().unwrap();
        let values: &[[f32; 2]] = bytemuck::cast_slice(&view);
        assert!(values
            .iter()
            .all(|v| v.iter().all(|x| (0.0..1.0).contains(x))));
        assert!(
            values.iter().all(|v| (v[0] - v[1]).abs() > 0.01),
            "the shadow integration repeats its samples after eight frames"
        );
    }
}
