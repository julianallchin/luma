//! Procedural medium uniforms and a bounded light-path optical-depth cache.
use crate::{frame::Frame, scene_desc::HazeAppearance};
use bytemuck::{Pod, Zeroable};

/// Outdoor haze falls by e every fifty metres of altitude, without an XY edge.
const OUTDOOR_SCALE_HEIGHT_M: f32 = 50.0;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct Uniform {
    pub shape: [f32; 4],
    pub wind: [f32; 4],
    pub min: [f32; 4],
    pub max: [f32; 4],
}
impl Uniform {
    pub fn new(frame: &Frame, sigma: f32, time: f32) -> Self {
        let HazeAppearance {
            cloudiness,
            cloud_size,
            turbulence,
            wind_speed,
            wind_direction,
        } = frame.haze_appearance.sanitized();
        let angle = wind_direction.to_radians();
        // Outdoors these bounds schedule fixture lighting only. The density
        // field is global; a rig's dimensions must never cut a hole in it.
        let (bounds, scale_height) = if frame.sky.is_some() {
            let bounds =
                luma_scene::Aabb::from_points(frame.fixture_cones.iter().flat_map(|light| {
                    let radius = glam::Vec3::splat(light.range);
                    [light.position - radius, light.position + radius]
                }));
            let bounds = if bounds.is_empty() {
                luma_scene::Aabb::new(glam::Vec3::ZERO, glam::Vec3::ZERO)
            } else {
                bounds
            };
            (bounds, OUTDOOR_SCALE_HEIGHT_M)
        } else {
            (frame.haze_bounds, 0.0)
        };
        Self {
            shape: [cloudiness, cloud_size, turbulence, 16.0],
            wind: [
                wind_speed * angle.cos(),
                wind_speed * angle.sin(),
                0.0,
                time,
            ],
            min: bounds.min.extend(sigma).to_array(),
            max: bounds.max.extend(scale_height).to_array(),
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct CacheUniform {
    pub medium: Uniform,
    pub count: [u32; 4],
}

pub(crate) struct Cache {
    pub view: wgpu::TextureView,
    capacity: usize,
}
impl Cache {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            view: Self::allocate(device, 1),
            capacity: 1,
        }
    }
    fn allocate(device: &wgpu::Device, capacity: usize) -> wgpu::TextureView {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("haze-optical-depth-cache"),
                size: wgpu::Extent3d {
                    width: 16 * 16,
                    height: 16 * (capacity as u32).div_ceil(16),
                    depth_or_array_layers: 33,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D3,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    }
    pub fn ensure(&mut self, device: &wgpu::Device, count: usize) {
        if count > self.capacity {
            self.capacity = count.next_power_of_two();
            self.view = Self::allocate(device, self.capacity);
        }
    }
}

#[cfg(test)]
mod tests;
