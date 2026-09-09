//! Four-scale volumetric noise, baked once per device.
//!
//! Broad clouds and fine wisps share one wrapping 3D texture. Combining the
//! octaves at startup avoids four trilinear reads at every camera/light-path
//! sample. Cloudiness, cloud size, wind and deformation still apply at runtime.
//! The bake shader owns the field; the CPU mirror exists only for verification.

/// Texels per edge; the R16Float field occupies 32 MiB.
pub(crate) const SIZE: u32 = 256;

/// Texels per base coordinate unit. The finest octave uses 3.75 lattice cells
/// per unit, leaving just over two texels per cell for reconstruction.
pub(crate) const TEXELS_PER_CELL: u32 = 8;

/// Period of the combined field in base coordinates. Every octave wraps at
/// its own scaled period. Runtime coordinates scale by 2/cloud_size, so the
/// world repeat distance is 16 * cloud_size metres.
pub(crate) const CELLS: u32 = SIZE / TEXELS_PER_CELL;

/// `R16Float` over `R8Snorm` deliberately: quantisation is a *uniform* error and
/// can contour where the reconstruction error, being random, does not. The
/// speed is identical, so this is the safe end of a decision that can be swept
/// downward later if the 32 MiB matters.
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
    /// performed on the GPU rather than a CPU startup loop. No bake work is
    /// repeated while rendering or adjusting haze controls.
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
    fn wrapped_gradient(cell: [f32; 3], cells: f32) -> [f32; 3] {
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
    fn lattice_noise(p: [f32; 3], period: f32) -> f32 {
        let i = p.map(f32::floor);
        let f = [p[0] - i[0], p[1] - i[1], p[2] - i[2]];
        let u = f.map(|v| v * v * (3.0 - 2.0 * v));
        let mut g = [0.0_f32; 8];
        for (n, slot) in g.iter_mut().enumerate() {
            let c = [(n & 1) as f32, ((n >> 1) & 1) as f32, ((n >> 2) & 1) as f32];
            let grad = wrapped_gradient([i[0] + c[0], i[1] + c[1], i[2] + c[2]], period);
            *slot = (0..3).map(|k| grad[k] * (f[k] - c[k])).sum();
        }
        let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let x00 = mix(g[0], g[1], u[0]);
        let x10 = mix(g[2], g[3], u[0]);
        let x01 = mix(g[4], g[5], u[0]);
        let x11 = mix(g[6], g[7], u[0]);
        mix(mix(x00, x10, u[1]), mix(x01, x11, u[1]), u[2])
    }

    fn layered_noise(p: [f32; 3]) -> f32 {
        let octave = |p: [f32; 3], scale: f32, offset: [f32; 3]| {
            lattice_noise(
                std::array::from_fn(|i| p[i] * scale + offset[i]),
                CELLS as f32 * scale,
            )
        };
        0.45 * octave(p, 0.25, [0.0; 3])
            + 0.30 * octave([p[1], p[2], p[0]], 0.75, [11.0, 3.0, 7.0])
            + 0.17 * octave([p[2], p[0], p[1]], 1.75, [5.0, 17.0, 2.0])
            + 0.08 * octave(p, 3.75, [19.0, 6.0, 13.0])
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
            let expected = half::f16::from_f32(layered_noise(centre)).to_f32();
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
            let wrapped = layered_noise([centre(x) + CELLS as f32, centre(y), centre(z)]);
            assert!(
                (at(x, y, z) - half::f16::from_f32(wrapped).to_f32()).abs() < 1.0e-3,
                "field does not wrap at ({x}, {y}, {z})"
            );
        }
    }
}
