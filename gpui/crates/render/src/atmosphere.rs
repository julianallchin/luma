//! A physically based sky: Hillaire, *A Scalable and Production Ready Sky and
//! Atmosphere Rendering Technique* (EGSR 2020).
//!
//! Three tables, in increasing volatility:
//!
//! - **transmittance** (256x64) — how much of a ray survives to the top of the
//!   atmosphere. A function of the medium alone, so it is built once per
//!   device, and built *here on the CPU* rather than in a compute pass. That is
//!   the one place the optical-depth integral is written, and both the sun's
//!   colour (which the CPU has to know, because the directional light is a
//!   uniform) and every GPU pass read the same answer from it.
//! - **multiple scattering** (32x32) — Hillaire §4, indexed by sun zenith and
//!   altitude and therefore also sun-independent. Built once per device.
//! - **sky view** (192x108) — the whole sky for one sun position. Rebuilt only
//!   when the sun moves, which for a venue render is once.
//!
//! Radiance is in units of top-of-atmosphere solar irradiance: 1.0 is "as
//! bright as unattenuated sunlight on a surface facing it". [`SkyFrame::exposure`] is the
//! single scalar that carries that into the range `AgX` expects, and it multiplies
//! the sky, the probe and the sun's own radiance together — so the horizon, the
//! disc and the shadows a rig casts can never disagree about how bright the day
//! is.
//!
//! Not implemented: the aerial-perspective volume. Distant geometry is left to
//! the composite's existing horizon dissolve. See the module note at the end of
//! [`SkyFrame`] for what adding it would take.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use half::f16;
use wgpu::util::DeviceExt;

use crate::environment::{EnvironmentPipelines, CUBE_SIZE};
use crate::scene_desc::SkyParams;

/// Ground sphere, kilometres.
const GROUND_RADIUS_KM: f32 = 6360.0;
/// Top of the atmosphere, kilometres.
const TOP_RADIUS_KM: f32 = 6460.0;
/// Rayleigh scattering at sea level, 1/km, per RGB channel.
const RAYLEIGH_SCATTERING: Vec3 = Vec3::new(5.802e-3, 13.558e-3, 33.1e-3);
/// Rayleigh density scale height, kilometres.
const RAYLEIGH_SCALE_KM: f32 = 8.0;
/// Mie scattering at sea level, 1/km. Grey by construction.
const MIE_SCATTERING: f32 = 3.996e-3;
/// Mie scattering plus absorption at sea level, 1/km.
const MIE_EXTINCTION: f32 = 3.996e-3 + 4.40e-3;
/// Mie density scale height, kilometres.
const MIE_SCALE_KM: f32 = 1.2;
/// Cornette-Shanks asymmetry. Forward-scattering, which is the sun's halo.
const MIE_G: f32 = 0.8;
/// Ozone absorption at the layer's peak, 1/km, per RGB channel. This is what
/// keeps twilight blue instead of grey — a Rayleigh-only sky goes colourless
/// once the sun is near the horizon.
const OZONE_ABSORPTION: Vec3 = Vec3::new(0.650e-3, 1.881e-3, 0.085e-3);
/// Centre of the ozone tent, kilometres.
const OZONE_CENTER_KM: f32 = 25.0;
/// Half-width of the ozone tent, kilometres.
const OZONE_HALF_WIDTH_KM: f32 = 15.0;

/// Ground albedo the multiple-scattering table is baked at.
///
/// That table is shared by every frame, so it cannot track a per-frame albedo.
/// A mid grey is the right compromise: the term it feeds is second-order and
/// already isotropic, and the *visible* ground bounce — the one in the sky-view
/// table — does use the frame's own albedo.
const GROUND_ALBEDO_REFERENCE: f32 = 0.3;

/// Camera altitude the sky is evaluated at, kilometres. A venue camera is tens
/// of metres up at most, which moves nothing in this model.
const VIEW_HEIGHT_KM: f32 = 0.002;

/// Angular radius of the solar disc, radians. Half of 0.535 degrees.
const SUN_ANGULAR_RADIUS: f32 = 0.004_675;

const TRANSMITTANCE_WIDTH: u32 = 256;
const TRANSMITTANCE_HEIGHT: u32 = 64;
const MULTISCATTER_SIZE: u32 = 32;
/// Sky-view table size.
///
/// Larger than Hillaire's 192x108 for one reason the paper does not have: a
/// venue's ground plane dissolves into the *below-horizon* half of this table
/// across most of the frame, and there a linearly interpolated 108-row table
/// reads as concentric mach bands on the floor. It is still 64k texels rebuilt
/// only when the sun moves.
const SKYVIEW_WIDTH: u32 = 256;
const SKYVIEW_HEIGHT: u32 = 256;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Steps in the CPU optical-depth march. The integrand is a sum of three
/// exponentials over a hundred kilometres; sixty-four midpoints is well past
/// the point where the table stops changing.
const OPTICAL_DEPTH_STEPS: u32 = 64;

/// Everything the shaders need to agree with this file about.
///
/// Prepended to every atmosphere shader — the same mechanism `haze_field`
/// uses — so a coefficient exists once, in Rust, and the GPU cannot hold a
/// stale copy of it.
fn prelude() -> String {
    let v = |name: &str, value: Vec3| {
        format!(
            "const {name}: vec3<f32> = vec3<f32>({:?}, {:?}, {:?});\n",
            value.x, value.y, value.z
        )
    };
    format!(
        "const GROUND_RADIUS_KM: f32 = {GROUND_RADIUS_KM:?};\n\
         const TOP_RADIUS_KM: f32 = {TOP_RADIUS_KM:?};\n\
         const RAYLEIGH_SCALE_KM: f32 = {RAYLEIGH_SCALE_KM:?};\n\
         const MIE_SCATTERING: f32 = {MIE_SCATTERING:?};\n\
         const MIE_EXTINCTION: f32 = {MIE_EXTINCTION:?};\n\
         const MIE_SCALE_KM: f32 = {MIE_SCALE_KM:?};\n\
         const MIE_G: f32 = {MIE_G:?};\n\
         const OZONE_CENTER_KM: f32 = {OZONE_CENTER_KM:?};\n\
         const OZONE_HALF_WIDTH_KM: f32 = {OZONE_HALF_WIDTH_KM:?};\n\
         const GROUND_ALBEDO_REFERENCE: f32 = {GROUND_ALBEDO_REFERENCE:?};\n\
         const MULTISCATTER_SIZE: u32 = {MULTISCATTER_SIZE}u;\n\
         const SKYVIEW_WIDTH: u32 = {SKYVIEW_WIDTH}u;\n\
         const SKYVIEW_HEIGHT: u32 = {SKYVIEW_HEIGHT}u;\n\
         const SKY_CUBE_SIZE: u32 = {CUBE_SIZE}u;\n{}{}",
        v("RAYLEIGH_SCATTERING", RAYLEIGH_SCATTERING),
        v("OZONE_ABSORPTION", OZONE_ABSORPTION),
    )
}

/// The shader prelude the composite pass prepends to read the sky tables.
pub(crate) fn composite_prelude() -> String {
    format!(
        "{}{}{}",
        prelude(),
        include_str!("shaders/atmosphere_common.wgsl"),
        include_str!("shaders/atmosphere_sky.wgsl"),
    )
}

// --- the medium, on the CPU -------------------------------------------------

/// Total extinction at height `h` above the ground, 1/km.
fn extinction(h: f32) -> Vec3 {
    let h = h.max(0.0);
    let rayleigh = (-h / RAYLEIGH_SCALE_KM).exp();
    let mie = (-h / MIE_SCALE_KM).exp();
    let ozone = (1.0 - (h - OZONE_CENTER_KM).abs() / OZONE_HALF_WIDTH_KM).max(0.0);
    RAYLEIGH_SCATTERING * rayleigh + Vec3::splat(MIE_EXTINCTION * mie) + OZONE_ABSORPTION * ozone
}

/// Distance from `(r, mu)` to the sphere of radius `radius`, or `None` if the
/// ray misses it. `mu` is the cosine between the ray and the local up.
fn ray_sphere_distance(r: f32, mu: f32, radius: f32) -> Option<f32> {
    let discriminant = r * r * (mu * mu - 1.0) + radius * radius;
    if discriminant < 0.0 {
        return None;
    }
    let sq = discriminant.sqrt();
    let near = -r * mu - sq;
    let far = -r * mu + sq;
    if far < 0.0 {
        None
    } else if near < 0.0 {
        Some(far)
    } else {
        Some(near)
    }
}

/// Fraction of light surviving from `(r, mu)` to the top of the atmosphere.
///
/// Zero when the ray meets the ground first — which is exactly the sun setting.
fn transmittance(r: f32, mu: f32) -> Vec3 {
    if ray_sphere_distance(r, mu, GROUND_RADIUS_KM).is_some() {
        return Vec3::ZERO;
    }
    let Some(length_km) = ray_sphere_distance(r, mu, TOP_RADIUS_KM) else {
        return Vec3::ONE;
    };
    let dt = length_km / OPTICAL_DEPTH_STEPS as f32;
    let mut depth = Vec3::ZERO;
    for step in 0..OPTICAL_DEPTH_STEPS {
        let t = (step as f32 + 0.5) * dt;
        // Law of cosines along the ray, in the plane it shares with the centre.
        let sample_r = (r * r + t * t + 2.0 * r * t * mu).max(0.0).sqrt();
        depth += extinction(sample_r - GROUND_RADIUS_KM) * dt;
    }
    (-depth).exp()
}

// --- what a frame gets ------------------------------------------------------

/// The sun and the sky, resolved for one frame.
///
/// The sun here **is** the frame's directional light: [`crate::frame`] builds
/// `Frame::directional` from [`Self::sun_direction`] and [`Self::sun_radiance`]
/// rather than from an authored `sun`, so the disc in the picture, the colour
/// of the horizon and the direction of every cast shadow are one fact.
///
/// # Aerial perspective
/// Not modelled. Adding it is Hillaire §5.2: a 32x32x32 froxel volume holding
/// in-scattered radiance and transmittance out to a few kilometres, filled by
/// one compute pass per frame (it depends on the camera, unlike everything
/// here), then sampled in the composite by the same view ray it already
/// reconstructs — replacing the `horizon_dissolve` mix with a physical one. The
/// volume is the only piece of state a moving camera would invalidate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyFrame {
    /// Unit world direction from the ground toward the sun.
    pub sun_direction: Vec3,
    /// Sun irradiance on a surface facing it, after atmospheric extinction and
    /// after [`SkyFrame::exposure`]. This is what reddens at dusk.
    pub sun_radiance: Vec3,
    /// Display exposure applied to the sky, the probe and the sun alike.
    pub exposure: f32,
    /// Ground albedo the sky-view table bounces sunlight off.
    pub ground_albedo: f32,
}

/// Display exposure for a sun elevation.
///
/// Dusk to noon is roughly six stops of sky luminance and far more of ground
/// illuminance, so one fixed exposure cannot hold both ends. This is a fitted
/// curve rather than a physical camera model: it is anchored at four measured
/// elevations and interpolated in log2, which is the axis the eye reads
/// brightness on. Anchors were chosen against renders of a rig — the criterion
/// is that the horizon stays under the `AgX` shoulder while a black-steel truss
/// still silhouettes against it.
#[must_use]
fn default_exposure(elevation_deg: f32) -> f32 {
    /// (elevation in degrees, exposure), ascending in elevation.
    const ANCHORS: [(f32, f32); 5] = [
        (-6.0, 34.0),
        (0.0, 16.0),
        (4.0, 10.0),
        (20.0, 4.5),
        (90.0, 3.0),
    ];
    let e = elevation_deg.clamp(ANCHORS[0].0, ANCHORS[ANCHORS.len() - 1].0);
    for pair in ANCHORS.windows(2) {
        let (lo, hi) = (pair[0], pair[1]);
        if e <= hi.0 {
            let t = (e - lo.0) / (hi.0 - lo.0);
            return (lo.1.log2() * (1.0 - t) + hi.1.log2() * t).exp2();
        }
    }
    ANCHORS[ANCHORS.len() - 1].1
}

/// Resolve authored sky parameters into the sun and exposure a frame carries.
#[must_use]
pub(crate) fn resolve(params: &SkyParams) -> SkyFrame {
    let elevation = params.sun_elevation_deg.to_radians();
    let azimuth = params.sun_azimuth_deg.to_radians();
    let (sin_e, cos_e) = elevation.sin_cos();
    let (sin_a, cos_a) = azimuth.sin_cos();
    let exposure = params
        .exposure
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or_else(|| default_exposure(params.sun_elevation_deg));
    SkyFrame {
        sun_direction: Vec3::new(cos_e * cos_a, cos_e * sin_a, sin_e),
        sun_radiance: transmittance(GROUND_RADIUS_KM + VIEW_HEIGHT_KM, sin_e) * exposure,
        exposure,
        ground_albedo: params.ground_albedo.clamp(0.0, 1.0),
    }
}

// --- GPU --------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Pod, Zeroable)]
struct SkyUniform {
    sun: [f32; 4],
    params: [f32; 4],
}

impl SkyUniform {
    fn of(sky: Option<&SkyFrame>) -> Self {
        sky.map_or(
            Self {
                sun: [0.0, 0.0, 1.0, 0.0],
                params: [1.0, 0.0, VIEW_HEIGHT_KM, 1.0],
            },
            |sky| Self {
                sun: sky.sun_direction.extend(1.0).to_array(),
                params: [
                    sky.exposure,
                    sky.ground_albedo,
                    VIEW_HEIGHT_KM,
                    SUN_ANGULAR_RADIUS.cos(),
                ],
            },
        )
    }
}

/// The scene-independent half: the two static tables, the pipelines that fill
/// the volatile one, and the layout the composite binds group 2 with.
pub(crate) struct AtmospherePipelines {
    transmittance: wgpu::TextureView,
    multiscatter: wgpu::TextureView,
    skyview_layout: wgpu::BindGroupLayout,
    cube_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    skyview_pipeline: wgpu::ComputePipeline,
    cube_pipeline: wgpu::ComputePipeline,
    sampler: wgpu::Sampler,
    /// Group 2 for a frame with no sky: the tables replaced by a black texel
    /// and `sun.w` zero, so the composite keeps one pipeline and one
    /// bind-group layout. Built once — an indoor venue renders this every
    /// frame, and a uniform buffer per frame for a value that never moves is
    /// the garbage `EnvironmentCache` already learned not to make.
    off: wgpu::BindGroup,
}

impl AtmospherePipelines {
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atmosphere"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let transmittance = transmittance_table(device, queue);
        let prelude = prelude();
        let common = include_str!("shaders/atmosphere_common.wgsl");

        let multiscatter_layout = device.create_bind_group_layout(&layout(
            "atmosphere-multiscatter",
            &[sampled_2d(0), sampler_entry(1), storage_2d(2)],
        ));
        let multiscatter_pipeline = compute(
            device,
            "atmosphere-multiscatter",
            &multiscatter_layout,
            &format!(
                "{prelude}{common}{}",
                include_str!("shaders/atmosphere_multiscatter.wgsl")
            ),
        );
        let multiscatter = multiscatter_table(
            device,
            queue,
            &multiscatter_layout,
            &multiscatter_pipeline,
            &transmittance,
            &sampler,
        );

        let skyview_layout = device.create_bind_group_layout(&layout(
            "atmosphere-skyview",
            &[
                sampled_2d(0),
                sampler_entry(1),
                sampled_2d(2),
                storage_2d(3),
                uniform_entry(4, wgpu::ShaderStages::COMPUTE),
            ],
        ));
        let skyview_pipeline = compute(
            device,
            "atmosphere-skyview",
            &skyview_layout,
            &format!(
                "{prelude}{common}{}",
                include_str!("shaders/atmosphere_skyview.wgsl")
            ),
        );

        let cube_layout = device.create_bind_group_layout(&layout(
            "atmosphere-cube",
            &[
                sampled_2d(0),
                sampler_entry(1),
                storage_2d_array(2),
                uniform_entry(3, wgpu::ShaderStages::COMPUTE),
            ],
        ));
        let cube_pipeline = compute(
            device,
            "atmosphere-cube",
            &cube_layout,
            &format!(
                "{prelude}{common}{}",
                include_str!("shaders/atmosphere_cube.wgsl")
            ),
        );

        let composite_layout = device.create_bind_group_layout(&layout(
            "atmosphere-composite",
            &[
                sampled_2d(0),
                sampled_2d(1),
                sampler_entry(2),
                uniform_entry(3, wgpu::ShaderStages::FRAGMENT),
            ],
        ));

        let placeholder = placeholder_texture(device, queue);
        let off = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atmosphere-composite-off"),
            layout: &composite_layout,
            entries: &[
                binding(0, wgpu::BindingResource::TextureView(&placeholder)),
                binding(1, wgpu::BindingResource::TextureView(&placeholder)),
                binding(2, wgpu::BindingResource::Sampler(&sampler)),
                binding(
                    3,
                    buffer(device, SkyUniform::of(None), "atmosphere-sky-off").as_entire_binding(),
                ),
            ],
        });
        Self {
            transmittance,
            multiscatter,
            skyview_layout,
            cube_layout,
            composite_layout,
            skyview_pipeline,
            cube_pipeline,
            sampler,
            off,
        }
    }

    pub(crate) fn composite_layout(&self) -> &wgpu::BindGroupLayout {
        &self.composite_layout
    }
}

/// What one renderer has resident for the sun it last drew.
#[derive(Default)]
pub(crate) struct AtmosphereCache {
    resident: Option<Resident>,
}

struct Resident {
    sky: SkyFrame,
    composite: wgpu::BindGroup,
    probe: (wgpu::TextureView, wgpu::TextureView),
}

impl AtmosphereCache {
    /// Bring the sky-view table and the environment probe up to date with
    /// `sky`, returning the composite's group-2 bindings and, when there is a
    /// sky, the irradiance and specular cubes the scene pass should use.
    ///
    /// Rebuilds nothing when the sun has not moved: the sky-view table is a
    /// function of `sky` alone, and a venue render moves the camera far more
    /// often than the sun.
    pub(crate) fn prepare(
        &mut self,
        pipelines: &AtmospherePipelines,
        environment: &EnvironmentPipelines,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        sky: Option<&SkyFrame>,
    ) -> (
        wgpu::BindGroup,
        Option<(wgpu::TextureView, wgpu::TextureView)>,
    ) {
        let Some(sky) = sky else {
            self.resident = None;
            return (pipelines.off.clone(), None);
        };
        if let Some(resident) = &self.resident {
            if resident.sky == *sky {
                return (resident.composite.clone(), Some(resident.probe.clone()));
            }
        }

        let uniform = buffer(device, SkyUniform::of(Some(sky)), "atmosphere-sky");
        let skyview_texture = storage_texture(
            device,
            SKYVIEW_WIDTH,
            SKYVIEW_HEIGHT,
            1,
            "atmosphere-skyview",
        );
        let skyview = skyview_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atmosphere-skyview"),
            layout: &pipelines.skyview_layout,
            entries: &[
                binding(
                    0,
                    wgpu::BindingResource::TextureView(&pipelines.transmittance),
                ),
                binding(1, wgpu::BindingResource::Sampler(&pipelines.sampler)),
                binding(
                    2,
                    wgpu::BindingResource::TextureView(&pipelines.multiscatter),
                ),
                binding(3, wgpu::BindingResource::TextureView(&skyview)),
                binding(4, uniform.as_entire_binding()),
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("atmosphere-skyview"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipelines.skyview_pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(SKYVIEW_WIDTH.div_ceil(8), SKYVIEW_HEIGHT.div_ceil(8), 1);
        }

        // The probe: the same sky, projected onto the cube the environment
        // preprocessing already knows how to convolve. That is the whole of the
        // ambient term — no new shading path, and the scene pass cannot tell a
        // sky from an authored HDR.
        let raw = crate::environment::cube_texture(device, CUBE_SIZE, 1, "atmosphere-cube");
        let raw_target = crate::environment::storage_mip(&raw, 0, "atmosphere-cube-target");
        let raw_sample = raw.create_view(&wgpu::TextureViewDescriptor {
            label: Some("atmosphere-cube"),
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        });
        let cube_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atmosphere-cube"),
            layout: &pipelines.cube_layout,
            entries: &[
                binding(0, wgpu::BindingResource::TextureView(&skyview)),
                binding(1, wgpu::BindingResource::Sampler(&pipelines.sampler)),
                binding(2, wgpu::BindingResource::TextureView(&raw_target)),
                binding(3, uniform.as_entire_binding()),
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("atmosphere-cube"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipelines.cube_pipeline);
            pass.set_bind_group(0, &cube_bind, &[]);
            pass.dispatch_workgroups(CUBE_SIZE.div_ceil(8), CUBE_SIZE.div_ceil(8), 6);
        }
        let probe = environment.convolve(device, encoder, &raw_sample, "atmosphere");

        let composite = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atmosphere-composite"),
            layout: &pipelines.composite_layout,
            entries: &[
                binding(
                    0,
                    wgpu::BindingResource::TextureView(&pipelines.transmittance),
                ),
                binding(1, wgpu::BindingResource::TextureView(&skyview)),
                binding(2, wgpu::BindingResource::Sampler(&pipelines.sampler)),
                binding(3, uniform.as_entire_binding()),
            ],
        });
        self.resident = Some(Resident {
            sky: *sky,
            composite: composite.clone(),
            probe: probe.clone(),
        });
        (composite, Some(probe))
    }
}

/// The transmittance table, marched on the CPU and uploaded.
///
/// Bruneton's (r, mu) parameterisation, inverted: `u` places the ray's length
/// between the shortest and longest one leaving this altitude, `v` is the
/// altitude. The forward mapping is `transmittance_uv` in
/// `atmosphere_common.wgsl`, and the two are each other's inverse.
fn transmittance_table(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let h = (TOP_RADIUS_KM * TOP_RADIUS_KM - GROUND_RADIUS_KM * GROUND_RADIUS_KM).sqrt();
    let row = (TRANSMITTANCE_WIDTH * 8).div_ceil(256) * 256;
    let mut pixels = vec![0_u8; (row * TRANSMITTANCE_HEIGHT) as usize];
    for y in 0..TRANSMITTANCE_HEIGHT {
        let x_r = (y as f32 + 0.5) / TRANSMITTANCE_HEIGHT as f32;
        let rho = h * x_r;
        let r = (rho * rho + GROUND_RADIUS_KM * GROUND_RADIUS_KM).sqrt();
        for x in 0..TRANSMITTANCE_WIDTH {
            let x_mu = (x as f32 + 0.5) / TRANSMITTANCE_WIDTH as f32;
            let d_min = TOP_RADIUS_KM - r;
            let d_max = rho + h;
            let d = d_min + x_mu * (d_max - d_min);
            let mu = if d <= 0.0 {
                1.0
            } else {
                ((h * h - rho * rho - d * d) / (2.0 * r * d)).clamp(-1.0, 1.0)
            };
            let t = transmittance(r, mu);
            let rgba = [
                f16::from_f32(t.x),
                f16::from_f32(t.y),
                f16::from_f32(t.z),
                f16::ONE,
            ];
            let offset = (y * row + x * 8) as usize;
            pixels[offset..offset + 8].copy_from_slice(bytemuck::cast_slice(&rgba));
        }
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("atmosphere-transmittance"),
        size: wgpu::Extent3d {
            width: TRANSMITTANCE_WIDTH,
            height: TRANSMITTANCE_HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(row),
            rows_per_image: Some(TRANSMITTANCE_HEIGHT),
        },
        wgpu::Extent3d {
            width: TRANSMITTANCE_WIDTH,
            height: TRANSMITTANCE_HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

fn multiscatter_table(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    pipeline: &wgpu::ComputePipeline,
    transmittance: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::TextureView {
    let texture = storage_texture(
        device,
        MULTISCATTER_SIZE,
        MULTISCATTER_SIZE,
        1,
        "atmosphere-multiscatter",
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("atmosphere-multiscatter"),
        layout,
        entries: &[
            binding(0, wgpu::BindingResource::TextureView(transmittance)),
            binding(1, wgpu::BindingResource::Sampler(sampler)),
            binding(2, wgpu::BindingResource::TextureView(&view)),
        ],
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("atmosphere-multiscatter"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("atmosphere-multiscatter"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[]);
        let groups = MULTISCATTER_SIZE.div_ceil(8);
        pass.dispatch_workgroups(groups, groups, 1);
    }
    queue.submit(Some(encoder.finish()));
    view
}

fn placeholder_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("atmosphere-placeholder"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[0; 8],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

fn storage_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    layers: u32,
    label: &str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn buffer<T: Pod>(device: &wgpu::Device, value: T, label: &str) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::cast_slice(&[value]),
        usage: wgpu::BufferUsages::UNIFORM,
    })
}

fn compute(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::BindGroupLayout,
    source: &str,
) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some("main"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    })
}

fn layout<'a>(
    label: &'a str,
    entries: &'a [wgpu::BindGroupLayoutEntry],
) -> wgpu::BindGroupLayoutDescriptor<'a> {
    wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries,
    }
}

fn sampled_2d(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT | wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn storage_2d(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: FORMAT,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

fn storage_2d_array(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: FORMAT,
            view_dimension: wgpu::TextureViewDimension::D2Array,
        },
        count: None,
    }
}

fn binding(index: u32, resource: wgpu::BindingResource) -> wgpu::BindGroupEntry {
    wgpu::BindGroupEntry {
        binding: index,
        resource,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transmittance_reddens_as_the_sun_sets() {
        let r = GROUND_RADIUS_KM + VIEW_HEIGHT_KM;
        let noon = transmittance(r, 1.0);
        let dusk = transmittance(r, 4.0_f32.to_radians().sin());
        // Overhead the sky barely tints; at four degrees the blue is gone and
        // the red is most of what is left.
        assert!(noon.x > 0.9 && noon.z > 0.6, "{noon:?}");
        assert!(dusk.x > dusk.y && dusk.y > dusk.z, "{dusk:?}");
        assert!(dusk.x < noon.x * 0.5, "{dusk:?} vs {noon:?}");
    }

    #[test]
    fn the_sun_below_the_horizon_delivers_nothing() {
        let r = GROUND_RADIUS_KM + VIEW_HEIGHT_KM;
        assert_eq!(transmittance(r, (-2.0_f32).to_radians().sin()), Vec3::ZERO);
    }

    #[test]
    fn exposure_falls_monotonically_with_the_sun() {
        let mut previous = f32::INFINITY;
        for elevation in [-6.0, -4.0, 0.5, 4.0, 12.0, 30.0, 60.0, 90.0] {
            let exposure = default_exposure(elevation);
            assert!(exposure < previous, "{elevation} gave {exposure}");
            previous = exposure;
        }
    }

    #[test]
    fn the_default_sun_stands_behind_the_stage() {
        let sky = resolve(&SkyParams::DUSK);
        // Upstage is world -Y, and the offset keeps it off the centre line.
        assert!(sky.sun_direction.y < -0.5, "{:?}", sky.sun_direction);
        assert!(sky.sun_direction.x.abs() > 0.1, "{:?}", sky.sun_direction);
        assert!(sky.sun_direction.z > 0.0, "{:?}", sky.sun_direction);
    }
}
