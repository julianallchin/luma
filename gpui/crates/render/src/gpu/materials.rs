//! Material maps: cache keys by source and colour-space role, mip-chained
//! uploads, and the per-draw material bind group.

use crate::assets::Image;
use crate::frame::{Draw, Frame};

use super::descriptors::binding;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum TextureEncoding {
    Srgb,
    Linear,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct TextureKey {
    pub(super) source: String,
    pub(super) encoding: TextureEncoding,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct MaterialKey {
    pub(super) base_color: Option<TextureKey>,
    pub(super) normal: Option<TextureKey>,
    pub(super) metallic_roughness: Option<TextureKey>,
    pub(super) occlusion: Option<TextureKey>,
    pub(super) emissive: Option<TextureKey>,
    /// The ground's floor material, which samples anisotropically: it is
    /// seen at grazing angles out to the horizon.
    pub(super) floor: bool,
}

impl MaterialKey {
    pub(super) fn of(draw: &Draw, frame: &Frame) -> Self {
        let key = |index: Option<usize>, encoding| {
            index.map(|index| TextureKey {
                source: frame.images[index].key.clone(),
                encoding,
            })
        };
        Self {
            base_color: key(draw.textures.base_color, TextureEncoding::Srgb),
            normal: key(draw.textures.normal, TextureEncoding::Linear),
            metallic_roughness: key(draw.textures.metallic_roughness, TextureEncoding::Linear),
            occlusion: key(draw.textures.occlusion, TextureEncoding::Linear),
            emissive: key(draw.textures.emissive, TextureEncoding::Srgb),
            floor: frame.floor.is_some() && crate::frame::is_ground(&frame.meshes[draw.mesh].key),
        }
    }

    pub(super) fn textures(&self) -> impl Iterator<Item = &TextureKey> {
        [
            self.base_color.as_ref(),
            self.normal.as_ref(),
            self.metallic_roughness.as_ref(),
            self.occlusion.as_ref(),
            self.emissive.as_ref(),
        ]
        .into_iter()
        .flatten()
    }
}

pub(super) struct MaterialDefaults {
    pub(super) base_color: wgpu::TextureView,
    pub(super) normal: wgpu::TextureView,
    pub(super) metallic_roughness: wgpu::TextureView,
    pub(super) occlusion: wgpu::TextureView,
    pub(super) emissive: wgpu::TextureView,
}

/// Upload one immutable material map with a full mip chain. Color maps use an
/// sRGB view, while normal/metallic-roughness/AO maps remain linear data.
pub(super) fn upload_texture_view(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &Image,
    encoding: TextureEncoding,
) -> wgpu::TextureView {
    let levels = 32 - image.width.max(image.height).leading_zeros();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("material-map"),
        size: wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: match encoding {
            TextureEncoding::Srgb => wgpu::TextureFormat::Rgba8UnormSrgb,
            TextureEncoding::Linear => wgpu::TextureFormat::Rgba8Unorm,
        },
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let mut level = (image.width, image.height, image.rgba.to_vec());
    for mip in 0..levels {
        let (w, h, ref pixels) = level;
        // Rows go up 256-byte aligned: that is the copy alignment every backend
        // agrees on, and a tightly packed odd width otherwise lands skewed.
        let row = (w * 4).div_ceil(256) * 256;
        let mut padded = vec![0u8; (row * h) as usize];
        for y in 0..h as usize {
            let src = y * (w * 4) as usize;
            let dst = y * row as usize;
            padded[dst..dst + (w * 4) as usize]
                .copy_from_slice(&pixels[src..src + (w * 4) as usize]);
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: mip,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &padded,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        if mip + 1 < levels {
            level = downsample(w, h, pixels, encoding);
        }
    }

    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

pub(super) fn material_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    base_color: &wgpu::TextureView,
    normal: &wgpu::TextureView,
    metallic_roughness: &wgpu::TextureView,
    occlusion: &wgpu::TextureView,
    emissive: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("material"),
        layout,
        entries: &[
            binding(0, wgpu::BindingResource::TextureView(base_color)),
            binding(1, wgpu::BindingResource::TextureView(normal)),
            binding(2, wgpu::BindingResource::TextureView(metallic_roughness)),
            binding(3, wgpu::BindingResource::TextureView(occlusion)),
            binding(4, wgpu::BindingResource::TextureView(emissive)),
            binding(5, wgpu::BindingResource::Sampler(sampler)),
        ],
    })
}

/// One 2x2 box-filter step. Odd dimensions collapse to 1 and then repeat the
/// row/column, which is how GL's mip chain treats them.
fn downsample(w: u32, h: u32, pixels: &[u8], encoding: TextureEncoding) -> (u32, u32, Vec<u8>) {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = Vec::with_capacity((nw * nh * 4) as usize);
    for y in 0..nh {
        for x in 0..nw {
            let (x0, y0) = (x * 2, y * 2);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            for c in 0..4 {
                let at = |px: u32, py: u32| pixels[((py * w + px) * 4 + c) as usize];
                if encoding == TextureEncoding::Srgb && c < 3 {
                    let linear =
                        |value: u8| crate::colour::srgb_to_linear(f32::from(value) / 255.0);
                    let mean = (linear(at(x0, y0))
                        + linear(at(x1, y0))
                        + linear(at(x0, y1))
                        + linear(at(x1, y1)))
                        * 0.25;
                    let encoded = crate::colour::linear_to_srgb(mean);
                    out.push((encoded * 255.0).round().clamp(0.0, 255.0) as u8);
                } else {
                    let mean = u32::from(at(x0, y0))
                        + u32::from(at(x1, y0))
                        + u32::from(at(x0, y1))
                        + u32::from(at(x1, y1));
                    out.push(((mean + 2) / 4) as u8);
                }
            }
        }
    }
    (nw, nh, out)
}

#[cfg(test)]
mod tests {
    use super::{downsample, TextureEncoding};

    #[test]
    fn material_mips_filter_color_in_linear_light_and_data_as_bytes() {
        let pixels = [
            0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ];
        let (_, _, color) = downsample(2, 2, &pixels, TextureEncoding::Srgb);
        let (_, _, data) = downsample(2, 2, &pixels, TextureEncoding::Linear);
        assert_eq!(color, [188, 188, 188, 255]);
        assert_eq!(data, [128, 128, 128, 255]);
    }
}
