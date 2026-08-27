//! The volumetric density field, baked once per device.
//!
//! `haze_noise` used to evaluate two octaves of gradient noise per volumetric
//! sample — sixteen hash evaluations, 48 `sin` — which measured 87–95 % of a
//! sample's cost and therefore of the whole volumetric march
//! (`docs/design/haze-noise-field.md` §2). The field is now a wrapping 3D
//! texture read with hardware trilinear filtering, which costs no more than
//! deleting the density term outright.
//!
//! The bake shader is the field's only definition. The CPU mirror below exists
//! solely so the bake can be tested, in the same spirit as the light index's
//! CPU reference builder — and it can exist at all only because the hash is
//! integer, so the field is bit-reproducible off-device.

/// Texels per edge. Time is independent of this (the path is latency-hidden,
/// not capacity-bound), so it buys repeat distance and nothing else, and 256³
/// is the last size whose memory is unremarkable.
pub(crate) const SIZE: u32 = 256;

/// Texels per lattice cell. Trilinear reconstruction error falls 4× per
/// doubling — 34 % of the field's spread at 2, 11 % at 4, 2.9 % at 8 — while
/// the repeat distance halves. 4 is the knee.
pub(crate) const TEXELS_PER_CELL: u32 = 4;

/// Lattice cells per edge, i.e. the field's period in units of `q`. Octave 1
/// rides `q = p * 2`, so this is `CELLS / 2` metres; octave 2 rides `q * 3`,
/// so it repeats three times as often — that is the binding constraint on
/// visible tiling.
pub(crate) const CELLS: u32 = SIZE / TEXELS_PER_CELL;

/// `R16Float` over `R8Snorm` deliberately: quantisation is a *uniform* error and
/// can contour where the reconstruction error, being random, does not. The
/// speed is identical, so this is the safe end of a decision that can be swept
/// downward later if the 16 MB matters.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;

/// Density field plus the sampler that reconstructs it. Owned together because
/// the wrap mode is not a preference — a non-repeating sampler would clamp the
/// field into a smear at the texture edge.
pub(crate) struct HazeField {
    pub(crate) view: wgpu::TextureView,
    pub(crate) sampler: wgpu::Sampler,
}

impl HazeField {
    /// Bakes the field. Runs one compute dispatch and one buffer→texture copy,
    /// ~11 ms at 256³ against ~1 s for the same field on the CPU, which is why
    /// this is a GPU bake and not a startup loop or a build-time asset.
    pub(crate) fn bake(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let packed = bake_packed(device, queue);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("haze-noise-field"),
            size: extent(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("haze-noise-upload"),
        });
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &packed,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    // Two bytes per texel against `COPY_BYTES_PER_ROW_ALIGNMENT`
                    // of 256: this holds from 128 up and *fails validation* at
                    // 64. A smaller field needs padded rows, not a smaller
                    // constant.
                    bytes_per_row: Some(SIZE * 2),
                    rows_per_image: Some(SIZE),
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            extent(),
        );
        queue.submit([encoder.finish()]);

        Self {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("haze-noise-field"),
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::Repeat,
                address_mode_w: wgpu::AddressMode::Repeat,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                // No mip chain: the field is read at one scale per octave, and
                // a chain would only give the filter a way to disagree with
                // the bake.
                ..Default::default()
            }),
        }
    }
}

/// WGSL constants for shaders that touch the field, injected at pipeline
/// creation rather than passed in a uniform — the dimensions are compile-time
/// properties of this module, and the light index injects `NARROW_PHASE` the
/// same way for the same reason.
pub(crate) fn prelude() -> String {
    format!(
        "const FIELD_SIZE: u32 = {SIZE}u;\n\
         const FIELD_TEXELS: f32 = {TEXELS_PER_CELL}.0;\n\
         const FIELD_CELLS: f32 = {CELLS}.0;\n\
         const FIELD_INV_CELLS: f32 = {:?};\n",
        1.0 / CELLS as f32,
    )
}

fn extent() -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: SIZE,
        height: SIZE,
        depth_or_array_layers: SIZE,
    }
}

/// The compute half, separated so the test can read the baked texels back
/// without the shipping path paying for a readback.
fn bake_packed(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::Buffer {
    let texels = u64::from(SIZE) * u64::from(SIZE) * u64::from(SIZE);
    let packed = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("haze-noise-bake"),
        size: texels * 2,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("haze-noise-bake"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                "{}{}",
                prelude(),
                include_str!("shaders/haze_noise_bake.wgsl")
            )
            .into(),
        ),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("haze-noise-bake"),
        layout: None,
        module: &module,
        entry_point: Some("bake_field"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("haze-noise-bake"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: packed.as_entire_binding(),
        }],
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("haze-noise-bake"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("haze-noise-bake"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        // 2D: x over one slice's texel pairs, y over slices. A 1D dispatch
        // would exceed the 65535-workgroups-per-dimension limit at 256³.
        pass.dispatch_workgroups((SIZE * SIZE / 2).div_ceil(64), SIZE, 1);
    }
    queue.submit([encoder.finish()]);
    packed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact mirror of the bake shader's hash. Its existence is the argument
    /// for the hash being integer: a `sin`-based one cannot be reproduced
    /// off-device, so it could not be checked from here at all.
    fn wrapped_gradient(cell: [f32; 3]) -> [f32; 3] {
        let cells = CELLS as f32;
        let wrap = |v: f32| (v - (v / cells).floor() * cells) as u32;
        let h = [
            wrap(cell[0]).wrapping_mul(1_597_334_673),
            wrap(cell[1]).wrapping_mul(3_812_015_801),
            wrap(cell[2]).wrapping_mul(2_798_796_415),
        ];
        let m = h[0] ^ h[1] ^ h[2];
        let mut s = [
            m,
            m.wrapping_mul(1_597_334_677),
            m.wrapping_mul(3_812_015_801),
        ];
        let mut out = [0.0_f32; 3];
        for (value, slot) in s.iter_mut().zip(&mut out) {
            *value ^= *value >> 15;
            *value = value.wrapping_mul(2_246_822_519);
            *value ^= *value >> 13;
            *slot = -1.0 + 2.0 * ((*value >> 9) as f32 / 8_388_608.0);
        }
        out
    }

    // `i`, `f`, `u`, `g` deliberately: this mirrors the bake shader
    // line for line, and renaming them here is how the two drift apart.
    #[allow(clippy::many_single_char_names)]
    fn lattice_noise(p: [f32; 3]) -> f32 {
        let i = p.map(f32::floor);
        let f = [p[0] - i[0], p[1] - i[1], p[2] - i[2]];
        let u = f.map(|v| v * v * (3.0 - 2.0 * v));
        let mut g = [0.0_f32; 8];
        for (n, slot) in g.iter_mut().enumerate() {
            let c = [(n & 1) as f32, ((n >> 1) & 1) as f32, ((n >> 2) & 1) as f32];
            let grad = wrapped_gradient([i[0] + c[0], i[1] + c[1], i[2] + c[2]]);
            *slot = (0..3).map(|k| grad[k] * (f[k] - c[k])).sum();
        }
        let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let x00 = mix(g[0], g[1], u[0]);
        let x10 = mix(g[2], g[3], u[0]);
        let x01 = mix(g[4], g[5], u[0]);
        let x11 = mix(g[6], g[7], u[0]);
        mix(mix(x00, x10, u[1]), mix(x01, x11, u[1]), u[2])
    }

    /// The bake must reproduce an independent implementation of the field, and
    /// the field must be periodic. Both are silent when wrong — a bad wrap is
    /// a plane of discontinuity in the haze that no aggregate metric flags, and
    /// a bad texel-centre convention is a half-texel shift nobody would name.
    #[test]
    fn bake_matches_the_cpu_field_and_wraps() {
        // Its own device rather than the shared one: this tests a bake, not a
        // renderer, and nothing here needs the process-wide pipelines.
        let instance = wgpu::Instance::default();
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            eprintln!("no GPU device; skipping");
            return;
        };
        let packed = bake_packed(&device, &queue);
        let texels = (SIZE as usize).pow(3);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("haze-noise-readback"),
            size: (texels * 2) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(&packed, 0, &readback, 0, (texels * 2) as u64);
        queue.submit([encoder.finish()]);
        readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .expect("poll");
        let mapped = readback.slice(..).get_mapped_range().expect("map");
        let baked: Vec<half::f16> = bytemuck::cast_slice::<u8, half::f16>(&mapped).to_vec();
        drop(mapped);

        // A stride that shares no factor with the edge length walks all three
        // axes rather than sampling one plane.
        let stride = 7919;
        let mut worst = 0.0_f32;
        for index in (0..texels).step_by(stride) {
            let texel = [
                (index as u32 % SIZE) as f32,
                ((index as u32 / SIZE) % SIZE) as f32,
                (index as u32 / (SIZE * SIZE)) as f32,
            ];
            let centre = texel.map(|v| (v + 0.5) / TEXELS_PER_CELL as f32);
            let expected = half::f16::from_f32(lattice_noise(centre)).to_f32();
            worst = worst.max((baked[index].to_f32() - expected).abs());
        }
        assert!(
            worst < 1.0e-3,
            "baked field disagrees with the CPU reference by {worst}"
        );

        // Periodicity, read straight off the baked texels: the field one
        // period along any axis is the same field.
        let period = (CELLS * TEXELS_PER_CELL) as usize;
        assert_eq!(
            period, SIZE as usize,
            "the texture holds exactly one period"
        );
        for (x, y, z) in [(0_usize, 5_usize, 9_usize), (3, 0, 17), (11, 23, 0)] {
            let at = |x: usize, y: usize, z: usize| {
                baked[(z * SIZE as usize + y) * SIZE as usize + x].to_f32()
            };
            let centre = |v: usize| (v as f32 + 0.5) / TEXELS_PER_CELL as f32;
            // The wrap is a property of the field, so compare the texel at the
            // low edge against the CPU field evaluated one period further on.
            let wrapped = lattice_noise([centre(x) + CELLS as f32, centre(y), centre(z)]);
            assert!(
                (at(x, y, z) - half::f16::from_f32(wrapped).to_f32()).abs() < 1.0e-3,
                "field does not wrap at ({x}, {y}, {z})"
            );
        }
    }
}
