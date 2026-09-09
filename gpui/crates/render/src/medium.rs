//! Procedural medium uniforms and a bounded light-path optical-depth cache.
use crate::{frame::Frame, scene_desc::HazeAppearance};
use bytemuck::{Pod, Zeroable};

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
        Self {
            shape: [
                cloudiness,
                cloud_size,
                turbulence,
                if frame.haze_steps > 8 { 16.0 } else { 8.0 },
            ],
            wind: [
                wind_speed * angle.cos(),
                wind_speed * angle.sin(),
                0.0,
                time,
            ],
            min: frame.haze_bounds.min.extend(sigma).to_array(),
            max: frame.haze_bounds.max.extend(0.0).to_array(),
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct CacheUniform {
    pub medium: Uniform,
    pub count: [u32; 4],
}

pub(crate) const BYTES_PER_LIGHT: u64 = 16 * 16 * 33 * 4;
pub(crate) struct Cache {
    pub buffer: wgpu::Buffer,
    capacity: usize,
}
impl Cache {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            buffer: Self::allocate(device, 1),
            capacity: 1,
        }
    }
    fn allocate(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("haze-optical-depth-cache"),
            size: capacity as u64 * BYTES_PER_LIGHT,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        })
    }
    pub fn ensure(&mut self, device: &wgpu::Device, count: usize) {
        if count > self.capacity {
            self.capacity = count.next_power_of_two();
            self.buffer = Self::allocate(device, self.capacity);
        }
    }
}
