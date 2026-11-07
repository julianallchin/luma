//! CPU mirrors of the WGSL uniform and storage structs the passes bind.
//! Their layouts are checked against the shaders by the test below.

use bytemuck::{Pod, Zeroable};

use crate::frame::{Draw, Frame};

use super::CASCADE_COUNT;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct Globals {
    pub(super) view_proj: [[f32; 4]; 4],
    pub(super) light_view_proj: [[[f32; 4]; 4]; CASCADE_COUNT],
    pub(super) camera_pos: [f32; 4],
    pub(super) camera_forward: [f32; 4],
    pub(super) cascade_splits: [f32; 4],
    pub(super) ambient: [f32; 4],
    pub(super) room: [f32; 4],
    pub(super) room_falloff: [f32; 4],
    pub(super) dir_to_light: [f32; 4],
    pub(super) dir_color: [f32; 4],
    pub(super) params: [f32; 4],
    pub(super) medium: crate::medium::Uniform,
    pub(super) outdoor_sun: [f32; 4],
    /// x: fog-grid radial extent, y: 1 when the surface takes outdoor camera
    /// transmittance from the far-field fog grid, 0 when it marches the
    /// medium. See `scene_bindings.wgsl`.
    pub(super) surface_fog: [f32; 4],
    /// xy: output size in pixels, zw: reciprocal.
    pub(super) viewport: [f32; 4],
    /// The ground's floor material; see `floor.wgsl`.
    pub(super) floor: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct Instance {
    pub(super) model: [[f32; 4]; 4],
    pub(super) normal_matrix: [[f32; 4]; 4],
    pub(super) base_color: [f32; 4],
    pub(super) emissive: [f32; 4],
    /// x: 1 for the ground, which casts no sun shadow; y: normal-map scale;
    /// z: AO strength.
    pub(super) flags: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct OverlayInstance {
    pub(super) model: [[f32; 4]; 4],
    pub(super) color: [f32; 4],
}

/// Mirrors `IntervalCacheParams` in `haze_cache.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct IntervalCacheUniform {
    /// x: cache blocks per row, y: hash bucket count minus one, z: nonzero at
    /// native haze resolution, where 2×2 quads may share quadrature.
    pub(super) params: [u32; 4],
    /// y: frame epoch.
    pub(super) flags: [u32; 4],
    pub(super) slots: [[u32; 4]; 512],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct HazeUniform {
    pub(super) inv_view_proj: [[f32; 4]; 4],
    pub(super) camera_pos: [f32; 4],
    pub(super) params: [f32; 4],
    pub(super) tuning: [f32; 4],
    pub(super) transport: [f32; 4],
    pub(super) tiles: [f32; 4],
    pub(super) depth: [f32; 4],
    /// x: shadowed fixture count, y: shadow texel size.
    pub(super) shadow: [f32; 4],
    /// [`QualityBudget::haze`].
    pub(super) quality: [f32; 4],
    pub(super) medium: crate::medium::Uniform,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct SurfaceClusterUniform {
    /// x: surface lighting enabled, y: occupancy debug, z: surface-depth mask.
    /// The culling structure itself is the shared light index.
    pub(super) flags: [f32; 4],
    /// x: shadowed fixture count, y: shadow texel size, z: beam gain.
    pub(super) shadow: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct FixtureShadowMatrix {
    pub(super) view_proj: [[f32; 4]; 4],
    /// x: projection near plane, y: far plane, metres. The shaders linearise
    /// shadow depths with these so occlusion slack is metric — a constant raw
    /// reverse-Z bias is centimetres near the light but metres at range, and
    /// read as beams spilling through occluders.
    pub(super) params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct CompositeUniform {
    pub(super) inv_view_proj: [[f32; 4]; 4],
    pub(super) params: [f32; 4],
    pub(super) depth: [f32; 4],
    pub(super) background: [f32; 4],
    pub(super) medium: crate::medium::Uniform,
    pub(super) camera_pos: [f32; 4],
    pub(super) outdoor_sun: [f32; 4],
    /// x: display headroom, the brightest output as a multiple of SDR white.
    /// Read only by the [`Channels::Hdr`] pipeline. y: 1 when the sun
    /// shaft texture (`sun_shafts.rs`) holds this frame. zw unused.
    pub(super) display: [f32; 4],
}

pub(super) fn instance_of(frame: &Frame, draw: &Draw) -> Instance {
    Instance {
        model: draw.model.to_cols_array_2d(),
        normal_matrix: draw.model.inverse().transpose().to_cols_array_2d(),
        base_color: draw
            .material
            .base_color
            .extend(draw.material.metallic)
            .to_array(),
        emissive: draw
            .material
            .emissive
            .extend(draw.material.roughness)
            .to_array(),
        flags: [
            f32::from(u8::from(crate::frame::is_ground(
                &frame.meshes[draw.mesh].key,
            ))),
            if draw.textures.normal.is_some() {
                draw.material.normal_scale
            } else {
                0.0
            },
            draw.material.occlusion_strength,
            0.0,
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CompositeUniform, FixtureShadowMatrix, Globals, HazeUniform, SurfaceClusterUniform,
    };
    use crate::light_index::{LightCore, LightRest};

    #[test]
    fn volumetric_cpu_layouts_match_wgsl_storage_and_uniform_strides() {
        use wgpu::naga;
        // The size naga lays `name` out at in `source`, the module the GPU
        // compiles, so a field added on one side only fails here.
        fn wgsl_size(source: &str, name: &str) -> usize {
            let module = naga::front::wgsl::parse_str(source).expect("WGSL parses");
            let (_, ty) = module
                .types
                .iter()
                .find(|(_, ty)| ty.name.as_deref() == Some(name))
                .unwrap_or_else(|| panic!("no WGSL struct {name}"));
            ty.inner.size(module.to_ctx()) as usize
        }
        let scene = crate::gpu::wgsl::scene_wgsl();
        // The head of the haze module, in the order `Gpu::new` builds it.
        let transport = format!(
            "{}{}{}{}{}",
            crate::haze_field::prelude(),
            include_str!("../shaders/fixture_light.wgsl"),
            crate::light_index::prelude(),
            include_str!("../shaders/visibility.wgsl")
                .replace("@group(3) @binding(11)", "@group(0) @binding(8)"),
            crate::gpu::wgsl::beam_transport_wgsl()
        );
        let composite = crate::gpu::wgsl::composite_wgsl();
        let compact = format!(
            "{transport}{}{}{}",
            include_str!("../shaders/haze.wgsl"),
            include_str!("../shaders/haze_cache.wgsl"),
            include_str!("../shaders/haze_compact.wgsl")
        );
        for (cpu, source, name) in [
            (std::mem::size_of::<Globals>(), &scene, "Globals"),
            (std::mem::size_of::<LightCore>(), &scene, "FixtureLightCore"),
            (std::mem::size_of::<LightRest>(), &scene, "FixtureLightRest"),
            (
                std::mem::size_of::<FixtureShadowMatrix>(),
                &scene,
                "FixtureShadowMatrix",
            ),
            (
                std::mem::size_of::<SurfaceClusterUniform>(),
                &scene,
                "SurfaceClusterParams",
            ),
            (std::mem::size_of::<LightCore>(), &transport, "LightCore"),
            (std::mem::size_of::<LightRest>(), &transport, "LightRest"),
            (std::mem::size_of::<HazeUniform>(), &transport, "Haze"),
            (
                std::mem::size_of::<CompositeUniform>(),
                &composite,
                "Composite",
            ),
            (
                std::mem::size_of::<crate::haze_compact::Uniform>(),
                &compact,
                "CompactParams",
            ),
        ] {
            assert_eq!(cpu, wgsl_size(source, name), "{name}");
        }
    }
}
