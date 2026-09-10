use super::*;
use crate::{
    atmosphere::{extinction, resolve, GROUND_RADIUS_KM},
    device::DeviceContext,
    scene_desc::SkyParams,
};
use glam::Vec3;

fn read(context: &DeviceContext, view: &wgpu::TextureView) -> Vec<Vec3> {
    let texture = view.texture();
    let size = texture.size();
    let stride = (size.width * 8).div_ceil(256) * 256;
    let buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aerial-test-readback"),
        size: u64::from(stride * size.height * size.depth_or_array_layers),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = context
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(size.height),
            },
        },
        size,
    );
    context.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).unwrap();
        });
    context
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    rx.recv().unwrap().unwrap();
    let data = buffer.slice(..).get_mapped_range().unwrap();
    data.chunks_exact(stride as usize)
        .flat_map(|row| row[..size.width as usize * 8].chunks_exact(8))
        .map(|pixel| {
            Vec3::from_array(std::array::from_fn(|c| {
                f16::from_bits(u16::from_le_bytes([pixel[c * 2], pixel[c * 2 + 1]])).to_f32()
            }))
        })
        .collect()
}

fn prepare(
    context: &DeviceContext,
    pipelines: &AtmospherePipelines,
    cache: &mut Cache,
    sky: Option<&SkyFrame>,
    height: f32,
) -> Textures {
    let mut encoder = context
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    let textures = cache.prepare(pipelines, &context.device, &mut encoder, sky, height);
    context.queue.submit([encoder.finish()]);
    textures
}

#[test]
fn finite_paths_conserve_transmission_and_match_optical_depth() {
    let context = DeviceContext::shared().unwrap();
    let pipelines = AtmospherePipelines::new(&context.device, &context.queue);
    let sky = resolve(&SkyParams::DUSK);
    let textures = prepare(&context, &pipelines, &mut Cache::default(), Some(&sky), 2.0);
    let light = read(&context, &textures.radiance);
    let transmission = read(&context, &textures.transmittance);
    let slice = (SIZE[0] * SIZE[1]) as usize;
    for i in 0..light.len() {
        assert!(light[i].is_finite() && light[i].min_element() >= 0.0);
        assert!(transmission[i].is_finite());
        assert!(transmission[i].min_element() >= 0.0 && transmission[i].max_element() <= 1.0);
        if i < slice {
            assert_eq!(light[i], Vec3::ZERO, "zero distance adds no light");
            assert_eq!(transmission[i], Vec3::ONE, "zero distance loses no light");
        } else {
            assert!(light[i].cmpge(light[i - slice]).all(), "radiance at {i}");
            assert!(
                transmission[i].cmple(transmission[i - slice]).all(),
                "transmission at {i}"
            );
        }
    }

    // An independent uniform-step, double-precision path integral. Check up,
    // grazing and downward rays across kilometres, not just LUT self-consistency.
    for y in [0, 16, 31, 32, 48, 63] {
        let vertical = 1.0 - 2.0 * f64::from(y) / 63.0;
        let mu = (vertical * vertical.abs() * std::f64::consts::FRAC_PI_2).sin();
        for z in [1, 30, 45, 63] {
            let km = f64::from(DISTANCE_SCALE_M)
                * ((f64::from(z) / 63.0
                    * (1.0 + f64::from(MAX_DISTANCE_M / DISTANCE_SCALE_M)).ln())
                .exp()
                    - 1.0)
                * 0.001;
            let dt = km / 4096.0;
            let radius = f64::from(GROUND_RADIUS_KM) + 0.002;
            let mut depth = glam::DVec3::ZERO;
            for step in 0..4096 {
                let t = (f64::from(step) + 0.5) * dt;
                let height = ((radius * radius + t * t + 2.0 * radius * t * mu).sqrt()
                    - f64::from(GROUND_RADIUS_KM))
                .max(0.001);
                depth += extinction(height as f32).as_dvec3() * dt;
            }
            let expected = (-depth).exp().as_vec3();
            let actual = transmission[(z * 64 * 64 + y * 64) as usize];
            assert!(
                (actual - expected).abs().max_element() < 0.006,
                "zenith row {y}, distance {km:.3} km: {actual:?} vs {expected:?}"
            );
        }
    }
    let near = transmission[slice + 31 * 64];
    let far = transmission[45 * slice + 31 * 64];
    assert!(
        near.min_element() > 0.999,
        "nearby rigging stays clear: {near:?}"
    );
    assert!(
        far.x > far.y && far.y > far.z,
        "air removes more blue: {far:?}"
    );
    assert!(light[45 * slice + 31 * 64].max_element() > 0.01);
}

#[test]
fn altitude_and_sun_refresh_resident_transport_while_indoors_is_identity() {
    let context = DeviceContext::shared().unwrap();
    let pipelines = AtmospherePipelines::new(&context.device, &context.queue);
    let mut cache = Cache::default();
    let mut sky = resolve(&SkyParams::DUSK);
    let low = prepare(&context, &pipelines, &mut cache, Some(&sky), 2.0);
    let low_transmission = read(&context, &low.transmittance);
    let same = prepare(&context, &pipelines, &mut cache, Some(&sky), 2.0);
    assert_eq!(
        low.sky, same.sky,
        "unchanged atmosphere reuses its computation"
    );
    let high = prepare(&context, &pipelines, &mut cache, Some(&sky), 2000.0);
    assert_eq!(
        low.radiance, high.radiance,
        "camera motion reuses GPU storage"
    );
    assert_ne!(
        low.sky, high.sky,
        "camera altitude invalidates the integral"
    );
    let high_transmission = read(&context, &high.transmittance);
    let horizontal = 45 * 64 * 64 + 31 * 64;
    assert!(high_transmission[horizontal]
        .cmpgt(low_transmission[horizontal])
        .all());
    let high_light = read(&context, &high.radiance);
    sky.sun_direction = Vec3::Z;
    let noon = prepare(&context, &pipelines, &mut cache, Some(&sky), 2000.0);
    assert_ne!(
        high.sky, noon.sky,
        "the solar direction invalidates the integral"
    );
    let noon_light = read(&context, &noon.radiance);
    assert!((noon_light[horizontal] - high_light[horizontal]).length() > 0.01);
    let off = prepare(&context, &pipelines, &mut cache, None, 2.0);
    assert_eq!(read(&context, &off.radiance), vec![Vec3::ZERO]);
    assert_eq!(read(&context, &off.transmittance), vec![Vec3::ONE]);
}
