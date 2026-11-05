//! The camera's projection and the sun's shadow cascades fitted to its view,
//! with the cache identities that decide when a cascade is redrawn.

use glam::{Mat4, Vec3};

use crate::frame::{Draw, Frame};
use crate::shadow::{shadow_matrix_bits, ShadowCacheKey};

use super::{CAMERA_FAR, CAMERA_NEAR, CASCADE_COUNT, CASCADE_SPLITS, SHADOW_SIZE};

/// The camera exactly as the scene pass sees it.
pub(crate) struct CameraMatrices {
    /// The far plane: bounded under a sky, where the atmosphere ends.
    pub far: f32,
    pub view: Mat4,
    /// Finite reverse-Z: near maps to one, `far` to zero.
    pub proj: Mat4,
}

/// The frame's camera matrices at `aspect`. One function, so a debug export
/// of the camera cannot drift from what the renderer drew.
pub(crate) fn camera_matrices(frame: &Frame, aspect: f32) -> CameraMatrices {
    let far = if frame.sky.is_some() {
        crate::atmosphere::MAX_DISTANCE_M
    } else {
        CAMERA_FAR
    };
    CameraMatrices {
        far,
        // Passing the bounds in reverse order produces a finite reverse-Z
        // projection: near maps to one and the bounded far plane maps to zero.
        proj: Mat4::perspective_rh(
            frame.camera.fov_y_deg.to_radians(),
            aspect,
            far,
            CAMERA_NEAR,
        ),
        view: Mat4::look_at_rh(frame.camera.eye, frame.camera.target, Vec3::Z),
    }
}

/// The sun's shadow casters: every opaque draw but the ground, with its world
/// bounding sphere. `mesh_bounds` are the meshes' local spheres, by index.
pub(crate) fn sun_casters<'a>(
    frame: &'a Frame,
    mesh_bounds: &'a [(Vec3, f32)],
) -> impl Iterator<Item = (&'a Draw, (Vec3, f32))> + 'a {
    let opaque = frame.draws.len() - frame.transparent.len();
    frame.draws[..opaque]
        .iter()
        .filter(|draw| !crate::frame::is_ground(&frame.meshes[draw.mesh].key))
        .map(|draw| (draw, world_sphere(draw, mesh_bounds[draw.mesh])))
}

/// A draw's world-space bounding sphere from its mesh's local one, scaled by
/// the model's largest axis.
pub(super) fn world_sphere(draw: &Draw, (local, radius): (Vec3, f32)) -> (Vec3, f32) {
    let scale = draw
        .model
        .to_scale_rotation_translation()
        .0
        .abs()
        .max_element();
    (draw.model.transform_point3(local), radius * scale)
}

/// Fit three stable orthographic sun cameras to bounded view-frustum slices.
///
/// Each slice uses a quantized bounding sphere instead of a tight AABB. Camera
/// translation then changes only the snapped light-space centre; small camera
/// rotations cannot resize the projection and make every shadow texel swim.
///
/// The slice only bounds the receivers. The near plane reaches back to every
/// caster (`casters`: world bounding spheres) whose light-space footprint
/// overlaps the cascade, so the shadow on a receiver is the same from every
/// camera that sees it.
pub(super) fn cascade_matrices(
    eye: Vec3,
    forward: Vec3,
    fov_y: f32,
    aspect: f32,
    to_light: Vec3,
    casters: &[(Vec3, f32)],
) -> [Mat4; CASCADE_COUNT] {
    cascade_fits(eye, forward, fov_y, aspect, to_light, casters).map(|fit| fit.view_proj)
}

/// One sun cascade, with the numbers its matrix was built from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CascadeFit {
    /// Camera distances of the view slice this cascade covers.
    pub slice: (f32, f32),
    /// The fixed light frame every cascade shares.
    pub light_view: Mat4,
    /// Snapped slice centre in light space, x and y.
    pub centre: (f32, f32),
    /// Quantized half-extent of the orthographic square.
    pub radius: f32,
    /// Light-space depth of the slice's nearest and furthest corner.
    pub slice_depth: (f32, f32),
    /// Nearest depth after reaching back to the casters.
    pub caster_depth: f32,
    /// The orthographic near and far planes as passed (far margin, near
    /// margin applied).
    pub planes: (f32, f32),
    pub view_proj: Mat4,
}

/// Every cascade's fit. See [`cascade_matrices`].
pub(crate) fn cascade_fits(
    eye: Vec3,
    forward: Vec3,
    fov_y: f32,
    aspect: f32,
    to_light: Vec3,
    casters: &[(Vec3, f32)],
) -> [CascadeFit; CASCADE_COUNT] {
    let forward = forward.normalize_or(Vec3::Y);
    let world_up = if forward.z.abs() > 0.99 {
        Vec3::Y
    } else {
        Vec3::Z
    };
    let right = forward.cross(world_up).normalize_or(Vec3::X);
    let up = right.cross(forward).normalize_or(Vec3::Z);
    let light_dir = to_light.normalize_or(Vec3::new(0.4, -0.6, 0.7));
    let light_up = if light_dir.z.abs() > 0.99 {
        Vec3::Y
    } else {
        Vec3::Z
    };
    let tan_half = (fov_y * 0.5).tan();

    std::array::from_fn(|cascade| {
        let near = if cascade == 0 {
            CAMERA_NEAR
        } else {
            CASCADE_SPLITS[cascade - 1]
        };
        let far = CASCADE_SPLITS[cascade];
        let corners = [near, far].map(|distance| {
            let center = eye + forward * distance;
            let half_y = tan_half * distance;
            let half_x = half_y * aspect;
            [
                center - right * half_x - up * half_y,
                center + right * half_x - up * half_y,
                center - right * half_x + up * half_y,
                center + right * half_x + up * half_y,
            ]
        });
        let corners = [
            corners[0][0],
            corners[0][1],
            corners[0][2],
            corners[0][3],
            corners[1][0],
            corners[1][1],
            corners[1][2],
            corners[1][3],
        ];
        let center = corners.iter().copied().sum::<Vec3>() / corners.len() as f32;
        // Quantization prevents floating-point radius noise from changing the
        // projection scale. 1/16 m is well below one far-cascade texel.
        let radius = corners
            .iter()
            .map(|corner| corner.distance(center))
            .fold(0.0_f32, f32::max);
        let radius = (radius * 16.0).ceil() / 16.0;
        // Keep light orientation and origin independent of the camera. The
        // orthographic bounds carry the moving slice; this is what lets their
        // centre snap in world-shadow space instead of remaining perpetually
        // zero in a camera-following look-at matrix.
        let light_view = Mat4::look_at_rh(light_dir, Vec3::ZERO, light_up);
        let center_light = light_view.transform_point3(center);
        let texel = (2.0 * radius) / SHADOW_SIZE as f32;
        let snapped_x = (center_light.x / texel).round() * texel;
        let snapped_y = (center_light.y / texel).round() * texel;
        let (min_depth, max_depth) = corners
            .iter()
            .map(|corner| -light_view.transform_point3(*corner).z)
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), depth| {
                (min.min(depth), max.max(depth))
            });
        // A caster between the sun and the slice can be far from it along
        // the light: a truss 10 m up is 140 m from its shadow under a 4
        // degree sun. A near plane fitted to the slice clipped such casters,
        // and which ones depended on where the camera stood.
        let near_depth = casters
            .iter()
            .filter_map(|&(centre, reach)| {
                let centre = light_view.transform_point3(centre);
                ((centre.x - snapped_x).abs() <= radius + reach
                    && (centre.y - snapped_y).abs() <= radius + reach)
                    .then_some(-centre.z - reach)
            })
            .fold(min_depth, f32::min);
        // Swap near/far to map the nearest caster to one and the furthest
        // receiver to zero, matching the scene's GreaterEqual reverse-Z
        // convention. The signed distances are valid for an orthographic
        // projection and let this fixed light frame cover venues on either
        // side of its origin. The far margin keeps the blend band at the end
        // of the previous slice, which also samples this map, inside it.
        let planes = (max_depth + 25.0, near_depth - 1.0);
        CascadeFit {
            slice: (near, far),
            light_view,
            centre: (snapped_x, snapped_y),
            radius,
            slice_depth: (min_depth, max_depth),
            caster_depth: near_depth,
            planes,
            view_proj: Mat4::orthographic_rh(
                snapped_x - radius,
                snapped_x + radius,
                snapped_y - radius,
                snapped_y + radius,
                planes.0,
                planes.1,
            ) * light_view,
        }
    })
}

/// Hash of every opaque draw that writes the camera depth buffer: mesh
/// identity and model matrix bits, in draw order. Fixture bodies count here,
/// unlike `fixture_shadow_caster_hash`, because the haze ray stops at them.
pub(super) fn opaque_depth_hash(frame: &Frame, opaque: usize) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut push_byte = |byte: u8| {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    };
    for draw in frame.draws.iter().take(opaque) {
        for byte in frame.meshes[draw.mesh].key.as_bytes() {
            push_byte(*byte);
        }
        push_byte(0);
        for value in draw.model.to_cols_array() {
            for byte in value.to_bits().to_le_bytes() {
                push_byte(byte);
            }
        }
    }
    hash
}

/// Cache identity of each sun cascade. The cascades rasterise the whole opaque
/// list — moving heads cast sun shadows — so their caster identity is
/// [`opaque_depth_hash`], not the fixture maps' caster hash, which leaves
/// fixture bodies out and would keep a moved head's old shadow.
pub(super) fn cascade_cache_keys(
    light_view_proj: &[Mat4; CASCADE_COUNT],
    depth_hash: u64,
) -> [ShadowCacheKey; CASCADE_COUNT] {
    light_view_proj.map(|matrix| ShadowCacheKey {
        matrix_bits: shadow_matrix_bits(&matrix.to_cols_array_2d()),
        caster_hash: depth_hash,
    })
}

#[cfg(test)]
mod tests {
    use glam::{Mat4, Vec3};

    use super::cascade_matrices;
    use crate::gpu::{CAMERA_FAR, CAMERA_NEAR, CASCADE_COUNT, SHADOW_SIZE};

    #[test]
    fn reverse_z_projection_is_monotonic_and_maps_bounded_planes() {
        let projection =
            Mat4::perspective_rh(48f32.to_radians(), 16.0 / 9.0, CAMERA_FAR, CAMERA_NEAR);
        let depth = |distance: f32| projection.project_point3(Vec3::new(0.0, 0.0, -distance)).z;
        assert!((depth(CAMERA_NEAR) - 1.0).abs() < 1e-5);
        assert!(depth(CAMERA_FAR).abs() < 1e-6);
        let samples = [CAMERA_NEAR, 1.0, 10.0, 100.0, CAMERA_FAR];
        assert!(samples
            .windows(2)
            .all(|pair| depth(pair[0]) > depth(pair[1])));
    }

    #[test]
    fn cascades_are_finite_bounded_and_texel_stable_under_small_translation() {
        let eye = Vec3::new(4.5, -5.0, 3.0);
        let forward = (Vec3::new(0.0, 0.8, 0.0) - eye).normalize();
        let to_light = Vec3::new(2.0, -3.0, 6.0).normalize();
        let matrices =
            cascade_matrices(eye, forward, 48f32.to_radians(), 16.0 / 9.0, to_light, &[]);
        assert_eq!(matrices.len(), CASCADE_COUNT);
        assert!(matrices.iter().all(Mat4::is_finite));

        // Move perpendicular to the light by much less than a near-cascade
        // texel. The snapped world-to-shadow transform must not shimmer.
        let lateral = to_light.cross(Vec3::Z).normalize_or(Vec3::X);
        let shifted = cascade_matrices(
            eye + lateral * (1.0 / SHADOW_SIZE as f32),
            forward,
            48f32.to_radians(),
            16.0 / 9.0,
            to_light,
            &[],
        );
        let max_delta = matrices[0]
            .to_cols_array()
            .into_iter()
            .zip(shifted[0].to_cols_array())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            max_delta < 1e-5,
            "sub-texel camera motion changed the near cascade by {max_delta}"
        );
    }
}
